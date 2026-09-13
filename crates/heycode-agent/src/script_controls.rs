//! Live, owner-scoped controls for bounded JavaScript runs.
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(crate) struct ScriptControls(Mutex<BTreeMap<String, Arc<LiveScript>>>);
pub(crate) struct LiveScript {
    owner: String,
    state: Mutex<RunState>,
    changed: tokio::sync::Notify,
    cancellation: CancellationToken,
}
#[derive(Default)]
struct RunState {
    paused: bool,
    active: usize,
}
pub(crate) struct RunLease {
    controls: Arc<ScriptControls>,
    id: String,
    pub(crate) live: Arc<LiveScript>,
}
pub(crate) struct CallLease(Arc<LiveScript>);
impl Drop for CallLease {
    fn drop(&mut self) {
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.active = state.active.saturating_sub(1);
    }
}
impl Drop for RunLease {
    fn drop(&mut self) {
        self.live.cancellation.cancel();
        self.controls
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}
impl LiveScript {
    pub(crate) fn cancellation(&self) -> CancellationToken {
        self.cancellation.clone()
    }
    pub(crate) async fn enter(self: &Arc<Self>) -> anyhow::Result<CallLease> {
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            anyhow::ensure!(!self.cancellation.is_cancelled(), "script stopped");
            {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if !state.paused {
                    state.active += 1;
                    return Ok(CallLease(self.clone()));
                }
            }
            tokio::select! {
                () = self.cancellation.cancelled() => anyhow::bail!("script stopped while paused"),
                () = &mut notified => {}
            }
        }
    }
    fn snapshot(&self, id: &str) -> Value {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let status = if self.cancellation.is_cancelled() {
            "stopping"
        } else if state.paused && state.active == 0 {
            "paused_at_tool_boundary"
        } else if state.paused {
            "pausing"
        } else {
            "running"
        };
        json!({"run_id":id,"state":status,"active_calls":state.active})
    }
}
impl ScriptControls {
    pub(crate) fn start(
        self: &Arc<Self>,
        owner: String,
        id: String,
        cancellation: CancellationToken,
    ) -> anyhow::Result<RunLease> {
        let mut runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        anyhow::ensure!(runs.len() < 128, "live script limit reached");
        let live = Arc::new(LiveScript {
            owner,
            state: Mutex::new(RunState::default()),
            changed: tokio::sync::Notify::new(),
            cancellation: cancellation.child_token(),
        });
        anyhow::ensure!(!runs.contains_key(&id), "duplicate script identity");
        runs.insert(id.clone(), live.clone());
        Ok(RunLease {
            controls: self.clone(),
            id,
            live,
        })
    }
    pub(crate) fn list(&self, owner: &str) -> Vec<Value> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|(_, run)| run.owner == owner)
            .map(|(id, run)| run.snapshot(id))
            .collect()
    }
    pub(crate) fn control(&self, owner: &str, id: &str, action: &str) -> anyhow::Result<Value> {
        let runs = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let run = runs
            .get(id)
            .filter(|r| r.owner == owner)
            .ok_or_else(|| anyhow::anyhow!("no live script with this id in the current session"))?;
        match action {
            "pause" => run.state.lock().unwrap_or_else(|e| e.into_inner()).paused = true,
            "resume" => {
                run.state.lock().unwrap_or_else(|e| e.into_inner()).paused = false;
                run.changed.notify_waiters();
            }
            "stop" => run.cancellation.cancel(),
            _ => anyhow::bail!("unknown script control"),
        }
        Ok(run.snapshot(id))
    }
    pub(crate) fn shutdown(&self) {
        for run in self.0.lock().unwrap_or_else(|e| e.into_inner()).values() {
            run.cancellation.cancel();
        }
    }
}

pub(crate) fn command(controls: Arc<ScriptControls>) -> anyhow::Result<Arc<dyn crate::Command>> {
    Ok(Arc::new(ScriptsCommand {
        controls,
        descriptor: crate::CommandDescriptor::new(
            "scripts",
            "Inspect, pause, resume or stop live scripts in this session",
            vec![
                crate::CommandArgument::optional(
                    "action",
                    "pause, resume, stop, or omitted to list",
                )?,
                crate::CommandArgument::optional("run-id", "Live script id")?,
            ],
            crate::CommandTiming::Immediate,
            crate::CommandSource::from_plugin("code-mode")?,
        )?,
    }))
}
struct ScriptsCommand {
    controls: Arc<ScriptControls>,
    descriptor: crate::CommandDescriptor,
}
#[async_trait::async_trait]
impl crate::Command for ScriptsCommand {
    fn descriptor(&self) -> &crate::CommandDescriptor {
        &self.descriptor
    }
    async fn execute(&self, agent: &crate::Agent, args: &str) -> anyhow::Result<()> {
        let owner = agent
            .session()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .id()
            .to_string();
        let parts = args.split_whitespace().collect::<Vec<_>>();
        let value = match parts.as_slice() {
            [] => json!({"live_scripts":self.controls.list(&owner)}),
            [action @ ("pause" | "resume" | "stop"), id] => {
                self.controls.control(&owner, id, action)?
            }
            _ => anyhow::bail!("usage: /scripts [pause|resume|stop <run_id>]"),
        };
        agent.ui().emit(crate::UiEvent::Info { text: format!("{value}\nPause blocks new tool calls after admitted calls settle. Script time limits continue while paused; stopped runs are not replayed.") });
        Ok(())
    }
}
