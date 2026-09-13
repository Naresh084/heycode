//! Durable scripts and nested tool operation records, separate from provider call pairing.
use crate::{SessionEvent, SessionEventKind};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

/// One explicitly saved script, reusable within its owning session.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SavedScript {
    /// Human-selected name.
    pub name: String,
    /// Async JavaScript function body.
    pub source: String,
    /// Selected tool names; current policy is rechecked on each execution.
    pub tools: BTreeSet<String>,
}

/// Durable script lifecycle; no record implies that an external action succeeded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum CodeModeChange {
    /// Save or replace a named script definition.
    Saved {
        /// Exact definition.
        script: SavedScript,
    },
    /// Start an explicit run, never implicitly replayed after restart.
    Started {
        /// Stable run identity.
        run_id: String,
        /// Source and selected tools frozen for this run.
        script: SavedScript,
    },
    /// Persist intent before approval and tool execution.
    CallStarted {
        /// Owning run.
        run_id: String,
        /// Unique per-run call number.
        call: u32,
        /// Selected tool name.
        name: String,
        /// Exact submitted JSON arguments.
        arguments: Value,
    },
    /// Settle an admitted operation; missing settlement remains unknown on crash.
    CallFinished {
        /// Owning run.
        run_id: String,
        /// Corresponding call number.
        call: u32,
        /// Successful bounded JSON, absent for failure.
        value: Option<Value>,
        /// Failure/cancellation explanation.
        error: Option<String>,
    },
    /// Settle the script; failed scripts may have successful preceding calls.
    Finished {
        /// Owning run.
        run_id: String,
        /// Bounded script result, absent for failure.
        result: Option<Value>,
        /// Failure/cancellation explanation.
        error: Option<String>,
    },
}
impl CodeModeChange {
    /// Validate bounded durable fields.
    /// # Errors
    /// Invalid identifiers, sizes or contradictory outcomes are refused.
    pub fn validate_shape(&self) -> Result<(), String> {
        fn script_ok(script: &SavedScript) -> bool {
            id_ok(&script.name)
                && !script.source.trim().is_empty()
                && script.source.len() <= 1024 * 1024
                && script.tools.len() <= 512
                && script.tools.iter().all(|name| id_ok(name))
        }
        fn outcome(value: &Option<Value>, error: &Option<String>) -> bool {
            value.is_some() != error.is_some()
                && error
                    .as_ref()
                    .is_none_or(|e| !e.is_empty() && e.len() <= 8192)
        }
        let valid = match self {
            Self::Saved { script } => script_ok(script),
            Self::Started { run_id, script } => id_ok(run_id) && script_ok(script),
            Self::CallStarted {
                run_id,
                call,
                name,
                arguments,
            } => id_ok(run_id) && *call < 512 && id_ok(name) && arguments.is_object(),
            Self::CallFinished {
                run_id,
                call,
                value,
                error,
            } => id_ok(run_id) && *call < 512 && outcome(value, error),
            Self::Finished {
                run_id,
                result,
                error,
            } => id_ok(run_id) && outcome(result, error),
        };
        if !valid || serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 5 * 1024 * 1024 {
            return Err("invalid code-mode change".into());
        }
        Ok(())
    }
}
fn id_ok(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value.trim() == value
        && !value.chars().any(char::is_control)
}

/// Replayed operation status. A missing settlement is deliberately retained.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScriptCallView {
    /// Tool name.
    pub name: String,
    /// Exact arguments.
    pub arguments: Value,
    /// True only with a durable settlement.
    pub settled: bool,
    /// Successful result.
    pub value: Option<Value>,
    /// Explicit failure.
    pub error: Option<String>,
}
/// Replayed run; `settled == false` on recovery means interrupted, not complete.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ScriptRunView {
    /// Frozen run definition.
    pub script: SavedScript,
    /// Per-call execution evidence.
    pub calls: BTreeMap<u32, ScriptCallView>,
    /// True only after explicit run settlement.
    pub settled: bool,
    /// Successful result.
    pub result: Option<Value>,
    /// Failure message.
    pub error: Option<String>,
}
/// Session-scoped saved scripts and run history.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct CodeModeProjection {
    /// Current named definitions.
    pub saved: BTreeMap<String, SavedScript>,
    /// All admitted runs.
    pub runs: BTreeMap<String, ScriptRunView>,
}
impl CodeModeProjection {
    /// Validate and apply a lifecycle event in order.
    /// # Errors
    /// Duplicate starts, unselected calls and unmatched/duplicate settlements fail.
    pub fn apply(&mut self, change: &CodeModeChange) -> Result<(), String> {
        change.validate_shape()?;
        match change {
            CodeModeChange::Saved { script } => {
                self.saved.insert(script.name.clone(), script.clone());
            }
            CodeModeChange::Started { run_id, script } => {
                if self.runs.contains_key(run_id) {
                    return Err("duplicate script run".into());
                }
                self.runs.insert(
                    run_id.clone(),
                    ScriptRunView {
                        script: script.clone(),
                        calls: BTreeMap::new(),
                        settled: false,
                        result: None,
                        error: None,
                    },
                );
            }
            CodeModeChange::CallStarted {
                run_id,
                call,
                name,
                arguments,
            } => {
                let run = self.active(run_id)?;
                if !run.script.tools.contains(name) || run.calls.contains_key(call) {
                    return Err("unselected or duplicate script call".into());
                }
                run.calls.insert(
                    *call,
                    ScriptCallView {
                        name: name.clone(),
                        arguments: arguments.clone(),
                        settled: false,
                        value: None,
                        error: None,
                    },
                );
            }
            CodeModeChange::CallFinished {
                run_id,
                call,
                value,
                error,
            } => {
                let run = self.active(run_id)?;
                let call = run.calls.get_mut(call).ok_or("script call never started")?;
                if call.settled {
                    return Err("script call already settled".into());
                }
                call.settled = true;
                call.value = value.clone();
                call.error = error.clone();
            }
            CodeModeChange::Finished {
                run_id,
                result,
                error,
            } => {
                let run = self.active(run_id)?;
                if result.is_some() && run.calls.values().any(|call| !call.settled) {
                    return Err("script success with unsettled calls".into());
                }
                run.settled = true;
                run.result = result.clone();
                run.error = error.clone();
            }
        }
        Ok(())
    }
    fn active(&mut self, id: &str) -> Result<&mut ScriptRunView, String> {
        let run = self.runs.get_mut(id).ok_or("script run never started")?;
        if run.settled {
            return Err("script run already settled".into());
        }
        Ok(run)
    }
}
/// Rebuild script truth without adding messages to provider tool-call history.
/// # Errors
/// Invalid ordering or domain metadata fails loudly.
pub fn project_code_mode(events: &[SessionEvent]) -> Result<CodeModeProjection, String> {
    let mut projection = CodeModeProjection::default();
    for event in events {
        if let SessionEventKind::CodeModeChange { change } = &event.kind {
            projection.apply(change)?;
        }
    }
    Ok(projection)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::Session;
    use serde_json::json;
    fn script() -> SavedScript {
        SavedScript {
            name: "fixture".into(),
            source: "await tools.read({});".into(),
            tools: ["read".into()].into(),
        }
    }
    #[test]
    fn crash_replay_preserves_unsettled_intent_without_provider_messages() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(dir.path()).unwrap();
        for change in [
            CodeModeChange::Started {
                run_id: "r".into(),
                script: script(),
            },
            CodeModeChange::CallStarted {
                run_id: "r".into(),
                call: 0,
                name: "read".into(),
                arguments: json!({}),
            },
        ] {
            session
                .append(SessionEventKind::CodeModeChange {
                    change: Box::new(change),
                })
                .unwrap();
        }
        let reopened = Session::open(session.path().parent().unwrap()).unwrap();
        let projection = project_code_mode(reopened.events()).unwrap();
        assert!(!projection.runs["r"].settled);
        assert!(!projection.runs["r"].calls[&0].settled);
        assert!(crate::derive_messages(reopened.events()).is_empty());
        assert!(
            session
                .append(SessionEventKind::CodeModeChange {
                    change: Box::new(CodeModeChange::Finished {
                        run_id: "r".into(),
                        result: Some(json!({})),
                        error: None
                    })
                })
                .is_err()
        );
    }
    #[test]
    fn malformed_order_and_duplicate_settlement_never_commit() {
        let dir = tempfile::tempdir().unwrap();
        let mut session = Session::create(dir.path()).unwrap();
        let finish = CodeModeChange::Finished {
            run_id: "r".into(),
            result: Some(json!(1)),
            error: None,
        };
        assert!(
            session
                .append(SessionEventKind::CodeModeChange {
                    change: Box::new(finish.clone())
                })
                .is_err()
        );
        session
            .append(SessionEventKind::CodeModeChange {
                change: Box::new(CodeModeChange::Started {
                    run_id: "r".into(),
                    script: script(),
                }),
            })
            .unwrap();
        session
            .append(SessionEventKind::CodeModeChange {
                change: Box::new(finish.clone()),
            })
            .unwrap();
        let before = session.events().len();
        assert!(
            session
                .append(SessionEventKind::CodeModeChange {
                    change: Box::new(finish)
                })
                .is_err()
        );
        assert_eq!(before, session.events().len());
    }
}
