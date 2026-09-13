//! Human-only requests for a quiescent, same-session composition restart.

use std::sync::{Arc, Mutex};

use crate::terminal::TuiDisplayMode;
use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandMetadataError,
    CommandSource, CommandTiming,
};

/// The configuration dimension a same-session restart should refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecompositionAction {
    /// Rebuild installed plugin contributions from the current durable configuration.
    ReloadPlugins,
    /// Rebuild the terminal using the requested presentation.
    Display(TuiDisplayMode),
    /// Rebuild the effective native sandbox under the same session owner.
    Sandbox(heycode_exec::SandboxMode),
}

pub(crate) struct PendingRecomposition {
    pub(crate) action: RecompositionAction,
    pub(crate) permit: heycode_agent::RecompositionPermit,
}

#[derive(Default)]
struct State {
    attached: bool,
    display: TuiDisplayMode,
    pending: Option<PendingRecomposition>,
}

/// One attached terminal owns this inbox; dropping it releases abandoned fences.
#[derive(Clone, Default)]
pub struct RecompositionBridge(Arc<Mutex<State>>);

pub(crate) struct Attachment(RecompositionBridge);
impl Drop for Attachment {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.0.lock() {
            state.attached = false;
            state.pending = None;
        }
    }
}

impl RecompositionBridge {
    pub(crate) fn attach(&self, display: TuiDisplayMode) -> anyhow::Result<Attachment> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Terminal restart inbox unavailable"))?;
        anyhow::ensure!(!state.attached, "Terminal restart inbox already attached");
        state.attached = true;
        state.display = display;
        Ok(Attachment(self.clone()))
    }

    pub(crate) fn take(&self) -> anyhow::Result<Option<PendingRecomposition>> {
        Ok(self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Terminal restart inbox unavailable"))?
            .pending
            .take())
    }

    pub(crate) async fn request_action(
        &self,
        agent: &heycode_agent::Agent,
        action: RecompositionAction,
    ) -> anyhow::Result<()> {
        let permit = agent.acquire_recomposition_permit().await?;
        self.request(PendingRecomposition { action, permit })
    }

    fn display(&self) -> Option<TuiDisplayMode> {
        self.0
            .lock()
            .ok()
            .and_then(|state| state.attached.then_some(state.display))
    }

    fn request(&self, request: PendingRecomposition) -> anyhow::Result<()> {
        let mut state = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Terminal restart inbox unavailable"))?;
        anyhow::ensure!(state.attached, "Terminal restart is no longer attached");
        anyhow::ensure!(
            state.pending.is_none(),
            "A terminal restart is already pending"
        );
        state.pending = Some(request);
        Ok(())
    }
}

struct RecomposeCommand {
    descriptor: CommandDescriptor,
    bridge: RecompositionBridge,
    reload: bool,
    unavailable: CommandAvailability,
}

#[async_trait]
impl Command for RecomposeCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    fn availability(&self) -> CommandAvailability {
        if self.bridge.display().is_some() {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }
    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let display = self.bridge.display().ok_or_else(|| {
            anyhow::anyhow!("Terminal restart requires an attached interactive session")
        })?;
        let action = parse(self.reload, args, display)?;
        self.bridge.request_action(agent, action).await
    }
}

fn parse(reload: bool, args: &str, display: TuiDisplayMode) -> anyhow::Result<RecompositionAction> {
    if reload {
        anyhow::ensure!(args.trim().is_empty(), "Usage: /reload-plugins");
        return Ok(RecompositionAction::ReloadPlugins);
    }
    Ok(RecompositionAction::Display(match args.trim() {
        "" => match display {
            TuiDisplayMode::Automatic => TuiDisplayMode::ScreenReader,
            TuiDisplayMode::ScreenReader => TuiDisplayMode::Automatic,
        },
        "auto" => TuiDisplayMode::Automatic,
        "screen-reader" => TuiDisplayMode::ScreenReader,
        _ => anyhow::bail!("Usage: /tui [auto|screen-reader]"),
    }))
}

/// Queued commands never interrupt an active foreground turn or invent a model request.
///
/// # Errors
/// Invalid command metadata.
pub fn commands(
    source: CommandSource,
    bridge: RecompositionBridge,
) -> Result<Vec<Arc<dyn Command>>, CommandMetadataError> {
    let unavailable = CommandAvailability::unavailable(
        "Terminal restart requires an attached interactive session",
    )?;
    [
        (
            "reload-plugins",
            "Reload installed plugins by reopening this session",
            true,
        ),
        (
            "tui",
            "Switch terminal presentation and reopen this session",
            false,
        ),
    ]
    .into_iter()
    .map(|(id, description, reload)| {
        let arguments = if reload {
            Vec::new()
        } else {
            vec![CommandArgument::optional(
                "mode",
                "auto or screen-reader; omitted toggles",
            )?]
        };
        Ok(Arc::new(RecomposeCommand {
            descriptor: CommandDescriptor::new(
                id,
                description,
                arguments,
                CommandTiming::Queued,
                source.clone(),
            )?,
            bridge: bridge.clone(),
            reload,
            unavailable: unavailable.clone(),
        }) as Arc<dyn Command>)
    })
    .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    #[test]
    fn restart_gate_preserves_drafts_pending_inputs_and_permission_holds() {
        let mut state = crate::app::AppState::new("model", "/work/project".into());
        assert!(!state.recomposition_blocked());
        for draft in [" ", "unsent draft", "line one\nline two"] {
            state.input.select_all();
            state.input.insert_str(draft);
            assert!(state.recomposition_blocked());
            assert_eq!(state.input.lines().join("\n"), draft);
        }
        state.input.select_all();
        state.input.cut();
        state.pending_send = Some("queued user input".to_owned());
        assert!(state.recomposition_blocked());
        assert_eq!(state.pending_send.as_deref(), Some("queued user input"));
        state.pending_send = None;
        state.apply(&heycode_agent::UiEvent::ApprovalRequested {
            owner_session: None,
            id: 7,
            name: "bash".to_owned(),
            args_preview: "command: local fixture".to_owned(),
        });
        assert!(state.recomposition_blocked());
        assert_eq!(state.pending_ask.as_ref().unwrap().id, 7);
    }
}
