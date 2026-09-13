//! Session-local explicit fallback configuration and routing ownership.
use crate::RoutingService;
use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandDescriptor, CommandSource, CommandTiming, UiEvent,
};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Weak},
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FallbackConfig {
    pub(crate) provider: String,
    pub(crate) model: String,
}
fn read(path: &Path) -> anyhow::Result<Option<FallbackConfig>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
        Ok(metadata) => anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink() && metadata.len() <= 16384,
            "fallback configuration is not a bounded regular file"
        ),
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16385)
        .read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() <= 16384,
        "fallback configuration exceeds its bound"
    );
    Ok(serde_json::from_slice(&bytes)?)
}
fn write(path: &Path, config: Option<&FallbackConfig>) -> anyhow::Result<()> {
    if let Ok(metadata) = std::fs::symlink_metadata(path) {
        anyhow::ensure!(
            metadata.is_file() && !metadata.file_type().is_symlink(),
            "unsafe fallback configuration path"
        );
    }
    let mut options = atomic_write_file::AtomicWriteFile::options();
    #[cfg(unix)]
    {
        use atomic_write_file::unix::OpenOptionsExt as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        options.preserve_mode(false).mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(&serde_json::to_vec(&config)?)?;
    file.commit()?;
    Ok(())
}
struct FallbackOwner {
    service: Weak<RoutingService>,
    path: PathBuf,
}
impl heycode_agent::RequestFallback for FallbackOwner {
    fn apply(
        &self,
        from: &heycode_llm::LlmSelection,
        cancellation: &tokio_util::sync::CancellationToken,
    ) -> anyhow::Result<bool> {
        anyhow::ensure!(!cancellation.is_cancelled(), "fallback cancelled");
        let Some(config) = read(&self.path)? else {
            return Ok(false);
        };
        if config.provider == from.provider_name && config.model == from.model {
            return Ok(false);
        }
        let service = self
            .service
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("fallback routing owner ended"))?;
        service.apply_configured_fallback(from, &config)?;
        Ok(true)
    }
}
pub(crate) fn install(
    context: &mut heycode_core::Context,
    agent: &Arc<heycode_agent::Agent>,
    service: &Arc<RoutingService>,
) -> anyhow::Result<()> {
    let path = path(agent)?;
    let owner = Arc::new(FallbackOwner {
        service: Arc::downgrade(service),
        path: path.clone(),
    });
    let registration = agent.install_request_fallback(owner)?;
    context.effect(move || drop(registration));
    agent.pre_step_seam().push_effect(
        context,
        PrepareSavedFallback {
            service: Arc::downgrade(service),
            agent: Arc::downgrade(agent),
            path,
            attempted: std::sync::Mutex::new(None),
            read_error_shown: std::sync::atomic::AtomicBool::new(false),
        },
    );
    Ok(())
}
fn path(agent: &heycode_agent::Agent) -> anyhow::Result<PathBuf> {
    let session = agent
        .session()
        .lock()
        .map_err(|_| anyhow::anyhow!("session unavailable"))?;
    Ok(session
        .path()
        .parent()
        .ok_or_else(|| anyhow::anyhow!("session directory unavailable"))?
        .join("fallback.json"))
}
pub(crate) fn command(service: Arc<RoutingService>) -> anyhow::Result<Arc<dyn Command>> {
    Ok(Arc::new(FallbackCommand {
        service,
        descriptor: CommandDescriptor::new(
            "fallback",
            "Configure one explicit fallback route for definitive pre-output failures",
            vec![
                CommandArgument::optional(
                    "provider",
                    "Registered provider, off, or omitted for status",
                )?,
                CommandArgument::optional("model", "Catalog-proven model id")?,
            ],
            CommandTiming::Queued,
            CommandSource::from_plugin("routing")?,
        )?,
    }))
}
struct FallbackCommand {
    service: Arc<RoutingService>,
    descriptor: CommandDescriptor,
}
#[async_trait]
impl Command for FallbackCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let path = path(agent)?;
        let parts = args.split_whitespace().collect::<Vec<_>>();
        let message = match parts.as_slice() {
            [] => match read(&path)? {
                Some(config) => {
                    let readiness = match self.service.validate_fallback(&config) {
                        Ok(_) => "ready".to_owned(),
                        Err(error) => format!("unavailable until preparation succeeds: {error}"),
                    };
                    format!("Fallback: {} / {} ({readiness}). At most once per turn, only before provider output or tool effects.", config.provider, config.model)
                }
                None => "Fallback is off. Use /fallback <provider> <model> to authorize a fallback route.".into(),
            },
            ["off"] => {
                write(&path, None)?;
                "Fallback is off.".into()
            }
            [provider, model] => {
                let config = FallbackConfig { provider: (*provider).into(), model: (*model).into() };
                agent.providers().activate(provider, model, tokio_util::sync::CancellationToken::new()).await?;
                self.service.validate_fallback(&config)?;
                write(&path, Some(&config))?;
                format!("Fallback configured: {provider} / {model}. This authorizes sending this conversation to that route after a definitive pre-output availability failure. No switch after partial output or tool effects; /fallback off disables it.")
            }
            _ => anyhow::bail!("usage: /fallback [<provider> <model>|off]"),
        };
        agent.ui().emit(UiEvent::Info { text: message });
        Ok(())
    }
}

/// Prepares only an already-authorized saved target before request dispatch.
/// A failure keeps the primary route usable and is reported once per configuration.
struct PrepareSavedFallback {
    service: Weak<RoutingService>,
    agent: Weak<heycode_agent::Agent>,
    path: PathBuf,
    attempted: std::sync::Mutex<Option<Option<FallbackConfig>>>,
    read_error_shown: std::sync::atomic::AtomicBool,
}
#[async_trait]
impl heycode_core::Layer<heycode_agent::PreStepDecision> for PrepareSavedFallback {
    async fn handle(
        &self,
        input: &mut heycode_agent::PreStepDecision,
        mut next: heycode_core::Next<'_, heycode_agent::PreStepDecision>,
    ) -> anyhow::Result<()> {
        let config = match read(&self.path) {
            Ok(config) => config,
            Err(error) => {
                if !self
                    .read_error_shown
                    .swap(true, std::sync::atomic::Ordering::Relaxed)
                    && let Some(agent) = self.agent.upgrade()
                {
                    agent.ui().emit(UiEvent::Info {
                        text: format!("Saved fallback unavailable: {error}"),
                    });
                }
                return next.run(input).await;
            }
        };
        let prepare = {
            let mut attempted = self.attempted.lock().unwrap_or_else(|e| e.into_inner());
            if attempted.as_ref() == Some(&config) {
                false
            } else {
                *attempted = Some(config.clone());
                true
            }
        };
        if prepare
            && let Some(config) = config
            && let (Some(agent), Some(service)) = (self.agent.upgrade(), self.service.upgrade())
        {
            let result: anyhow::Result<()> = async {
                agent
                    .providers()
                    .activate(&config.provider, &config.model, input.cancellation.clone())
                    .await?;
                service.validate_fallback(&config)?;
                Ok(())
            }
            .await;
            if input.cancellation.is_cancelled() {
                *self.attempted.lock().unwrap_or_else(|e| e.into_inner()) = None;
            }
            if let Err(error) = result {
                agent.ui().emit(UiEvent::Info { text: format!("Saved fallback unavailable; primary route retained: {}. Reconfigure /fallback after resolving setup.", error.to_string().chars().take(1200).collect::<String>()) });
            }
        }
        next.run(input).await
    }
}
