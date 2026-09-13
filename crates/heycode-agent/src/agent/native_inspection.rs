//! Human inspection commands run native children under a real permission ceiling.
use super::{Agent, AgentIdle, SessionEventKind, TurnEndReason, UiEvent, next_turn};
use crate::{
    ChildPermissions, SubagentContinuation, SubagentId, SubagentProviderId, SubagentRegistry,
    SubagentRequest, SubagentSeed,
};

impl Agent {
    /// Run a native inspection preset as one durable, cancellable human turn.
    /// File presets may explicitly select inference routes, but cannot widen
    /// this command's read-only ceiling or select an external child runtime.
    ///
    /// # Errors
    /// Missing/native-incompatible presets, invalid configuration, child
    /// failures, cancellation, or durable log failures.
    pub async fn run_native_inspection(
        &self,
        registry: &SubagentRegistry,
        command: &str,
        instructions: &str,
    ) -> anyhow::Result<()> {
        let (preset_id, default_prompt) = match command {
            "review" => (
                "reviewer",
                "Review the current workspace changes for actionable correctness defects, with file and line evidence.",
            ),
            "advisor" | "ask-advisor" => (
                "advisor",
                "Inspect the current workspace and advise on the technical question or decision, using concrete code evidence.",
            ),
            "security-review" => (
                "security-review",
                "Review the current workspace changes for actionable security defects, with source locations and evidence of reachability and impact.",
            ),
            _ => anyhow::bail!("unknown native inspection command"),
        };
        let _gate = self.turn_gate.lock().await;
        let lease = self.cancel.begin_turn()?;
        let cancellation = lease.token().clone();
        let result = async {
            let preset = registry.preset(preset_id)
                .ok_or_else(|| anyhow::anyhow!("native preset `{preset_id}` is unavailable"))?;
            if preset.provider().is_some_and(|provider| provider.as_str() != "native") {
                anyhow::bail!("/{command} requires a native preset; configure inference_provider/model for an explicit inference route");
            }
            let mut config = preset.config().clone();
            config.permissions = if config.permissions == ChildPermissions::Deny {
                ChildPermissions::Deny
            } else {
                ChildPermissions::ReadOnly
            };
            config.background = false;
            let selection = self.selection();
            // Snapshot the actual live route at admission. Explicit preset
            // overrides remain authoritative; there is no automatic upgrade.
            if config.inference_provider.is_none() {
                config.inference_provider = Some(selection.provider_name);
            }
            if config.model.is_none() {
                config.model = Some(selection.model);
            }
            if config.effort.is_none() {
                config.effort = self.reasoning_effort().map(|effort| effort.as_str().to_owned());
            }
            let owner = {
                let session = self.session.lock().unwrap_or_else(|error| error.into_inner());
                SubagentId::new(session.id().as_str())?
            };
            let user_intent = if instructions.is_empty() {
                format!("/{command}")
            } else {
                format!("/{command} {instructions}")
            };
            let prompt = if instructions.is_empty() {
                default_prompt.to_owned()
            } else {
                format!("{default_prompt}\n\nUser instructions:\n{instructions}")
            };
            let request = SubagentRequest::with_authority(
                preset_id, prompt, SubagentSeed::Fresh, SubagentContinuation::OneShot,
                registry.root_authority(owner),
            )?.with_preset(&preset)?
                .with_configuration(preset.instructions(), config)?
                .with_provider(SubagentProviderId::new("native")?);
            let turn = {
                let mut session = self.session.lock().unwrap_or_else(|error| error.into_inner());
                let turn = next_turn(session.events());
                session.append(SessionEventKind::UserMessage { text: user_intent.clone() })?;
                session.append(SessionEventKind::TurnStart { turn })?;
                session.append(SessionEventKind::StepStart { turn, step: 1 })?;
                turn
            };
            self.emit(UiEvent::UserEcho { text: user_intent });
            self.emit(UiEvent::TurnStarted { turn });
            let started = registry.start(request, cancellation.clone()).await;
            let (text, reason) = if cancellation.is_cancelled() {
                (format!("/{command} cancelled."), TurnEndReason::Aborted)
            } else {
                match &started {
                    Ok(started) => (
                        format!("{}\n\n[task_id: {} · {preset_id}]", started.text, started.id),
                        TurnEndReason::Stop,
                    ),
                    Err(error) => (format!("/{command} failed: {error}"), TurnEndReason::Error),
                }
            };
            {
                let mut session = self.session.lock().unwrap_or_else(|error| error.into_inner());
                session.append(SessionEventKind::AssistantMessage {
                    turn, step: 1, content: text.clone(), reasoning: None, tool_calls: None, usage: None,
                })?;
                session.append(SessionEventKind::StepEnd { turn, step: 1 })?;
                session.append(SessionEventKind::TurnEnd { turn, reason })?;
            }
            self.emit(UiEvent::AssistantDelta { text });
            self.emit(UiEvent::TurnFinished {
                reason: match reason { TurnEndReason::Stop => "stop", TurnEndReason::Aborted => "aborted", _ => "error" }.to_owned(),
                usage: None, context_tokens: None,
            });
            started?;
            if cancellation.is_cancelled() {
                anyhow::bail!("native inspection cancelled");
            }
            Ok(())
        }.await;
        drop(lease);
        self.bus.emit(AgentIdle);
        result
    }
}
