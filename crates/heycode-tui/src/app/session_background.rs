//! Whole-session human controls. These never submit model input or answer a
//! permission request. The attached PTY child remains the only session writer.

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandAvailability, CommandDescriptor, CommandMetadataError, CommandSource,
    CommandTiming, UiEvent,
};
use heycode_session::background::{self as wire, Request, Response};

use crate::session_browser::{SessionCommandBridge, SessionCommandRequest};

#[derive(Clone, Copy)]
enum Kind {
    Detach,
    Fork,
    List,
}

struct SessionBackgroundCommand {
    descriptor: CommandDescriptor,
    kind: Kind,
    bridge: SessionCommandBridge,
    unavailable: CommandAvailability,
    unsupported_fork: CommandAvailability,
    session: Arc<std::sync::Mutex<heycode_session::Session>>,
}

#[async_trait]
impl Command for SessionBackgroundCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    fn availability(&self) -> CommandAvailability {
        if wire::child_connection().is_none() {
            return self.unavailable.clone();
        }
        if matches!(self.kind, Kind::Fork)
            && !self.session.lock().is_ok_and(|session| {
                session
                    .runtime_link()
                    .map(|(runtime, _)| runtime)
                    .or_else(|| session.metadata().and_then(|metadata| metadata.runtime()))
                    == Some("native")
            })
        {
            return self.unsupported_fork.clone();
        }
        CommandAvailability::available()
    }
    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        if !args.trim().is_empty() {
            anyhow::bail!("/{} takes no arguments", self.descriptor.id());
        }
        let (socket, _) = wire::child_connection()
            .ok_or_else(|| anyhow::anyhow!("whole-session host is unavailable"))?;
        match self.kind {
            Kind::Detach => {
                wire::request(&socket, &Request::Detach)?;
            }
            Kind::Fork => {
                if agent.runtime_id() != "native" {
                    anyhow::bail!(
                        "background conversation copies require the native runtime; this runtime needs its own provider fork operation"
                    );
                }
                if agent.approval_kind() == heycode_agent::ApprovalPolicyKind::Custom {
                    anyhow::bail!(
                        "a custom approval policy cannot be copied into a background session"
                    );
                }
                let selection = agent.selection();
                self.bridge
                    .request(SessionCommandRequest::BackgroundFork(wire::ForkOptions {
                        provider: selection.provider_name,
                        model: selection.model,
                        approval: agent.approval_kind().as_str().to_owned(),
                    }));
            }
            Kind::List => show_sessions(agent)?,
        }
        Ok(())
    }
}

pub(crate) fn commands(
    source: CommandSource,
    bridge: SessionCommandBridge,
    session: Arc<std::sync::Mutex<heycode_session::Session>>,
) -> Result<Vec<Arc<dyn Command>>, CommandMetadataError> {
    let unavailable = CommandAvailability::unavailable(
        "whole-session hosting requires a local interactive heycode terminal",
    )?;
    let unsupported_fork = CommandAvailability::unavailable(
        "background conversation copies require the native runtime; delegated runtimes need their own fork operation",
    )?;
    [
        (
            "background",
            "Detach this whole session; work keeps running locally",
            Kind::Detach,
            CommandTiming::Immediate,
        ),
        (
            "fork",
            "Copy this conversation into a separate background session",
            Kind::Fork,
            CommandTiming::Queued,
        ),
        (
            "sessions",
            "List attached and detached whole-session owners",
            Kind::List,
            CommandTiming::Immediate,
        ),
    ]
    .into_iter()
    .map(|(id, description, kind, timing)| {
        Ok(Arc::new(SessionBackgroundCommand {
            descriptor: CommandDescriptor::new(id, description, vec![], timing, source.clone())?,
            kind,
            bridge: bridge.clone(),
            unavailable: unavailable.clone(),
            unsupported_fork: unsupported_fork.clone(),
            session: session.clone(),
        }) as Arc<dyn Command>)
    })
    .collect()
}

fn home(agent: &heycode_agent::Agent) -> anyhow::Result<PathBuf> {
    let session = agent
        .session()
        .lock()
        .map_err(|_| anyhow::anyhow!("session unavailable"))?;
    session
        .path()
        .parent()
        .and_then(|path| path.parent())
        .and_then(|path| path.parent())
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("session is not under a heycode sessions root"))
}

fn show_sessions(agent: &heycode_agent::Agent) -> anyhow::Result<()> {
    let hosts = wire::live_hosts(&home(agent)?)?;
    let rows: Vec<_> = hosts
        .into_iter()
        .map(|host| {
            format!(
                "{}  {}  {}",
                host.session_id.as_deref().unwrap_or(&host.host_id),
                if host.stopping {
                    "stopping"
                } else if host.attached {
                    "attached"
                } else {
                    "detached"
                },
                host.cwd.display()
            )
        })
        .collect();
    agent.ui().emit(UiEvent::Info {
        text: if rows.is_empty() {
            "No live whole-session hosts.".to_owned()
        } else {
            format!("Whole sessions\n{}", rows.join("\n"))
        },
    });
    Ok(())
}

/// Prefixes deliberately keep session and background-operation domains distinct.
pub(crate) fn route_domain(
    agent: &heycode_agent::Agent,
    name: &str,
    args: &str,
) -> Option<anyhow::Result<()>> {
    if name == "tasks" && args.trim() == "sessions" {
        return Some(show_sessions(agent));
    }
    if name == "stop" && args.trim().starts_with("session ") {
        return Some((|| {
            let id = args
                .trim()
                .strip_prefix("session ")
                .unwrap_or_default()
                .trim();
            let host = wire::find_host(&home(agent)?, id)?
                .ok_or_else(|| anyhow::anyhow!("no live whole-session owner for {id}"))?;
            wire::request(&host.socket, &Request::Stop)?;
            agent.ui().emit(UiEvent::Info { text: format!("Graceful stop requested for session {id}; completion is pending until its process exits.") });
            Ok(())
        })());
    }
    None
}

/// Register a composed session and poll its process host on a blocking worker.
/// Host death asks the TUI to shut down; it never restarts inference on its own.
pub(crate) fn watch(
    agent: &heycode_agent::Agent,
) -> anyhow::Result<Option<tokio::sync::mpsc::Receiver<()>>> {
    let Some((socket, token)) = wire::child_connection() else {
        return Ok(None);
    };
    let session_id = agent
        .session()
        .lock()
        .map_err(|_| anyhow::anyhow!("session unavailable"))?
        .id()
        .to_string();
    wire::request(
        &socket,
        &Request::Register {
            token: token.clone(),
            session_id,
            cwd: agent.cwd(),
        },
    )?;
    let (tx, rx) = tokio::sync::mpsc::channel(1);
    std::thread::spawn(move || {
        while !tx.is_closed() {
            match wire::request(
                &socket,
                &Request::Poll {
                    token: token.clone(),
                },
            ) {
                Ok(Response::Ok) => {}
                _ => {
                    let _ = tx.blocking_send(());
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(120));
        }
    });
    Ok(Some(rx))
}

impl crate::app::AppState {
    pub(crate) fn resume_background_session(&mut self, id: &heycode_core::SessionId) -> bool {
        let Some((socket, token)) = wire::child_connection() else {
            return false;
        };
        let result = (|| -> anyhow::Result<bool> {
            let Some(current) = self.current_session.as_ref() else {
                return Ok(false);
            };
            let home = current
                .lock()
                .map_err(|_| anyhow::anyhow!("session unavailable"))?
                .path()
                .parent()
                .and_then(|path| path.parent())
                .and_then(|path| path.parent())
                .map(PathBuf::from)
                .ok_or_else(|| anyhow::anyhow!("session home unavailable"))?;
            let Some(host) = wire::find_host(&home, id.as_str())? else {
                return Ok(false);
            };
            wire::request(
                &socket,
                &Request::Switch {
                    token,
                    socket: host.socket,
                },
            )?;
            Ok(true)
        })();
        match result {
            Ok(switched) => switched,
            Err(error) => {
                self.items.push(crate::app::Item::Error(error.to_string()));
                true
            }
        }
    }

    pub(crate) fn fork_background_session(&mut self, options: wire::ForkOptions) {
        let result = (|| -> anyhow::Result<String> {
            if self.runtime != "native" {
                anyhow::bail!(
                    "background conversation copies require the native runtime; the active delegated session remains unchanged"
                );
            }
            let (socket, token) = wire::child_connection()
                .ok_or_else(|| anyhow::anyhow!("whole-session host is unavailable"))?;
            let parent = self
                .current_session_id()
                .ok_or_else(|| anyhow::anyhow!("current session unavailable"))?;
            let query = self
                .session_query
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("session lifecycle unavailable"))?;
            let child = query.fork(&parent, heycode_session::ForkBoundary::Latest)?;
            let session_id = child.id().to_string();
            drop(child);
            wire::request(&socket, &Request::Fork { token, session_id: session_id.clone(), cwd: self.cwd.clone(), options })
                .map_err(|error| anyhow::anyhow!("fork {session_id} was saved but its background host did not confirm startup: {error}; resume the saved id to recover"))?;
            Ok(format!(
                "Background session {session_id} started. Parent remains active. Use /resume {session_id} or heycode --resume {session_id} to attach."
            ))
        })();
        self.items.push(match result {
            Ok(text) => crate::app::Item::Info(text),
            Err(error) => crate::app::Item::Error(error.to_string()),
        });
    }
}
