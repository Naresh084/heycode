//! Portable session command and focus-recap controls.
use crate::session_browser::{SessionCommandBridge, SessionCommandRequest};
use heycode_agent::{
    Agent, Command, CommandArgument, CommandDescriptor, CommandSource, CommandTiming, UiEvent,
};
use std::sync::Arc;

struct Rewind {
    descriptor: CommandDescriptor,
    bridge: SessionCommandBridge,
}
#[async_trait::async_trait]
impl Command for Rewind {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &Agent, args: &str) -> anyhow::Result<()> {
        let mut parts = args.split_whitespace();
        let Some(turn) = parts.next() else {
            self.bridge.request(SessionCommandRequest::RewindPicker(
                crate::rewind_picker::RewindPickerRequest::from_agent(agent),
            ));
            return Ok(());
        };
        let turn = turn
            .parse::<u64>()
            .map_err(|_| anyhow::anyhow!("usage: /rewind [turn [files]]"))?;
        let files = match parts.next() {
            None => false,
            Some("files") => true,
            _ => anyhow::bail!("usage: /rewind [turn [files]]"),
        };
        if parts.next().is_some() {
            anyhow::bail!("usage: /rewind [turn [files]]");
        }
        let child = agent.rewind(turn, files).await?;
        agent.ui().emit(UiEvent::Info {
            text: format!(
                "rewound before turn {turn}; original conversation preserved; switching to {child}"
            ),
        });
        self.bridge.request(SessionCommandRequest::Resume(child));
        Ok(())
    }
}

pub(crate) fn rewind_command(
    bridge: SessionCommandBridge,
) -> Result<Arc<dyn Command>, heycode_agent::CommandMetadataError> {
    Ok(Arc::new(Rewind {
        descriptor: CommandDescriptor::new(
            "rewind",
            "Restore a durable conversation checkpoint, optionally native file edits",
            vec![
                CommandArgument::optional("turn", "Durable turn id; omit to choose a checkpoint")?,
                CommandArgument::optional(
                    "files",
                    "Also restore confirmed native edits, refusing conflicts",
                )?,
            ],
            CommandTiming::Queued,
            CommandSource::from_plugin("tui")?,
        )?,
        bridge,
    }))
}

/// Debounces return recaps by completed turn identity, with no wall-clock sleeps.
#[derive(Default)]
pub(crate) struct ReturnRecap {
    unfocused: bool,
    shown: Option<u64>,
}
impl ReturnRecap {
    pub(crate) fn lost_focus(&mut self) {
        self.unfocused = true;
    }
    pub(crate) fn returned(
        &mut self,
        now_ms: u64,
        completed: usize,
        latest: Option<(u64, u64)>,
        enabled: bool,
    ) -> bool {
        let returned = std::mem::take(&mut self.unfocused);
        let Some((seq, time_ms)) = latest else {
            return false;
        };
        if !returned
            || !enabled
            || completed < 3
            || self.shown == Some(seq)
            || now_ms.saturating_sub(time_ms) < 180_000
        {
            return false;
        }
        self.shown = Some(seq);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recap_requires_return_idle_age_three_turns_and_opt_in_and_never_repeats() {
        let mut recap = ReturnRecap::default();
        assert!(!recap.returned(200_000, 3, Some((10, 0)), true));
        recap.lost_focus();
        assert!(!recap.returned(200_000, 2, Some((10, 0)), true));
        recap.lost_focus();
        assert!(!recap.returned(200_000, 3, Some((10, 0)), false));
        recap.lost_focus();
        assert!(recap.returned(200_000, 3, Some((10, 0)), true));
        recap.lost_focus();
        assert!(!recap.returned(300_000, 3, Some((10, 0)), true));
        recap.lost_focus();
        assert!(!recap.returned(300_000, 4, Some((20, 200_000)), true));
        recap.lost_focus();
        assert!(recap.returned(400_000, 4, Some((20, 200_000)), true));
    }
}
