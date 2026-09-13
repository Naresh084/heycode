//! Neutral provider-facing projection of the session log.
//!
//! The wire types here are deliberately free of session and provider
//! vocabulary: `heycode-llm` maps them onto its own request types without ever
//! importing session types (AGENTS.md §2 dependency rules).

use crate::event::{SessionEvent, SessionEventKind};

/// Conversation participant roles on the neutral wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// System prompt role. Never produced by [`derive_messages`]; callers
    /// inject it at request assembly time.
    System,
    /// Human/user input.
    User,
    /// Model output.
    Assistant,
    /// Tool execution outcome bound to one call id.
    Tool,
}

/// One model-initiated tool call on the neutral wire.
#[derive(Debug, Clone, PartialEq)]
pub struct WireToolCall {
    /// Provider call id echoed back by the matching tool message.
    pub id: String,
    /// Tool name as declared in the registry.
    pub name: String,
    /// Raw JSON text of arguments, verbatim from the model.
    pub arguments: String,
}

/// One provider-ready conversation message folded from the session log.
#[derive(Debug, Clone, PartialEq)]
pub struct WireMessage {
    /// Who produced this message.
    pub role: Role,
    /// Text content; may be empty for tool-call-only assistant messages.
    pub content: String,
    /// Durable attachment metadata selected for this user message.
    pub attachments: Vec<heycode_core::AttachmentMetadata>,
    /// Explicit route chosen for each non-image attachment.
    pub document_routes: Vec<heycode_core::DocumentInputRoute>,
    /// Tool calls requested by an assistant message, in provider order.
    pub tool_calls: Option<Vec<WireToolCall>>,
    /// For [`Role::Tool`] messages: the id of the call this result answers.
    pub tool_call_id: Option<heycode_core::CallId>,
    /// For [`Role::Tool`] messages: whether execution failed or was denied.
    pub tool_result_is_error: Option<bool>,
    /// Typed external-content boundary for a tool result.
    pub untrusted_content: Option<heycode_core::UntrustedContentBoundary>,
}

/// Fold a session log into ordered neutral messages.
///
/// Mapping: `user/message` → [`Role::User`] verbatim; `assistant/message` →
/// [`Role::Assistant`] with its tool calls attached (arguments verbatim);
/// `tool/result` → [`Role::Tool`] bound to its call id. Everything else —
/// chunks (presentational) and turn/step bookkeeping — is skipped.
///
/// Compaction: when a `compaction/applied` event appears, every provider-
/// visible event BEFORE it collapses away and the summary enters as one User
/// message, so requests carry only `[summary, …events after compaction]`.
/// Later compactions replace earlier ones. `tool/call` records are skipped:
/// the assistant's own `tool_calls` already carry them.
///
/// This is the faithful fold: a call the log never answered stays unanswered
/// here, because that is what the log says. A consumer replaying a log that
/// may have been crashed wants [`derive_messages_repaired`] instead.
#[must_use]
pub fn derive_messages(events: &[SessionEvent]) -> Vec<WireMessage> {
    fold(events, Unanswered::Keep)
}

/// Fold a session log into ordered neutral messages, closing every tool call
/// the log never answered as interrupted.
///
/// Identical to [`derive_messages`] on a log that closed everything it opened,
/// so a consumer may use it unconditionally. On a log a crash cut short it
/// differs in exactly one way: each unanswered call gains a [`Role::Tool`]
/// message bound to its id, flagged `tool_result_is_error`, whose content says
/// the outcome is unknown.
///
/// Both alternatives are lies. Dropping the call would hide from the model
/// that it ever asked; leaving it unanswered ships a request no provider
/// accepts, and the shapes that do survive read as an outcome the tool never
/// produced. Naming it interrupted is the only reading the log supports.
///
/// The log is not modified: this is a read-time projection, and
/// [`crate::project_repair`] is how a consumer learns a repair happened at
/// all. Compaction shadowing applies first, so a call inside a compacted range
/// needs no closing message and gets none. Repairs for one assistant message
/// are emitted, in the order the model requested them, immediately before the
/// next message that is not one of its own results — so every id in a
/// tool-call block is answered before the block ends.
#[must_use]
pub fn derive_messages_repaired(events: &[SessionEvent]) -> Vec<WireMessage> {
    fold(events, Unanswered::Close)
}

/// What the fold does with a tool call no `tool/result` answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unanswered {
    /// Leave it unanswered, exactly as the log has it.
    Keep,
    /// Close it with an interrupted, unknown-outcome result.
    Close,
}

/// Content of a synthesized result for a call the log never answered.
///
/// Worded for a model reading it mid-conversation: it must not be able to
/// treat this as the tool's output, and must not re-run a side effect on the
/// assumption that nothing happened.
const INTERRUPTED_TOOL_RESULT: &str = "heycode was interrupted before this tool call recorded a result. Its outcome is unknown: it may have completed, partially completed, or never started. Do not assume it succeeded.";

fn fold(events: &[SessionEvent], unanswered: Unanswered) -> Vec<WireMessage> {
    // Portable compaction is replace-by-range for every route. Native opaque
    // checkpoints are deliberately ignored here because this neutral fold has
    // no provider/model/protocol identity; exact-route request projection owns
    // their replacement semantics.
    let winner = events
        .iter()
        .enumerate()
        .fold(None::<(u64, &SessionEvent)>, |best, (idx, e)| {
            match &e.kind {
                SessionEventKind::CompactionApplied {
                    replaced_upto_seq, ..
                } => match best {
                    Some((best_to, _)) if best_to > *replaced_upto_seq => best,
                    _ => Some((*replaced_upto_seq, &events[idx])),
                },
                _ => best,
            }
        });
    let shadow_upto = winner.as_ref().map_or(0, |(upto, _)| *upto);
    let mut out = Vec::new();
    // The summary stands in for the shadowed prefix, so it leads the request
    // regardless of where the append-only marker physically sits.
    if let Some((_, marker)) = winner
        && let SessionEventKind::CompactionApplied { summary, .. } = &marker.kind
    {
        out.push(WireMessage {
            role: Role::User,
            content: format!("<compacted-summary>\n{summary}\n</compacted-summary>"),
            attachments: Vec::new(),
            document_routes: Vec::new(),
            tool_calls: None,
            tool_call_id: None,
            tool_result_is_error: None,
            untrusted_content: None,
        });
    }
    let mut pending_attachments = Vec::new();
    let mut pending_document_routes = Vec::new();
    // Ids the newest assistant message requested that no result has answered
    // yet. Only tracked when the caller asked for repair, so the faithful fold
    // stays byte-identical by construction rather than by agreement.
    let mut unanswered_ids: Vec<String> = Vec::new();
    for event in events {
        match &event.kind {
            SessionEventKind::CompactionApplied { .. } => continue,
            SessionEventKind::NativeCompactionApplied { .. } => continue,
            _ if winner.is_some() && event.seq <= shadow_upto => continue,
            _ => {}
        }
        match &event.kind {
            SessionEventKind::UserAttachments {
                attachments,
                document_routes,
            } => {
                pending_attachments = attachments.clone();
                pending_document_routes = document_routes.clone();
            }
            SessionEventKind::UserMessage { text } => {
                close_unanswered(&mut out, &mut unanswered_ids);
                out.push(WireMessage {
                    role: Role::User,
                    content: text.clone(),
                    attachments: std::mem::take(&mut pending_attachments),
                    document_routes: std::mem::take(&mut pending_document_routes),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: None,
                });
            }
            SessionEventKind::HookContribution { contribution } => {
                close_unanswered(&mut out, &mut unanswered_ids);
                out.push(WireMessage {
                    role: Role::User,
                    content: contribution.render_for_model(),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: contribution.boundary(),
                });
            }
            SessionEventKind::AssistantMessage {
                content,
                tool_calls,
                ..
            } => {
                close_unanswered(&mut out, &mut unanswered_ids);
                if unanswered == Unanswered::Close
                    && let Some(calls) = tool_calls
                {
                    unanswered_ids.extend(calls.iter().map(|call| call.id.clone()));
                }
                out.push(WireMessage {
                    role: Role::Assistant,
                    content: content.clone(),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: tool_calls.as_ref().map(|calls| {
                        calls
                            .iter()
                            .map(|c| WireToolCall {
                                id: c.id.clone(),
                                name: c.name.clone(),
                                arguments: c.arguments.clone(),
                            })
                            .collect()
                    }),
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: None,
                });
            }
            SessionEventKind::ToolResult {
                call_id,
                content,
                is_error,
                untrusted_content,
            } => {
                unanswered_ids.retain(|id| id != call_id.as_str());
                out.push(WireMessage {
                    role: Role::Tool,
                    content: content.clone(),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                    tool_result_is_error: Some(*is_error),
                    untrusted_content: *untrusted_content,
                });
            }
            SessionEventKind::RichToolResult {
                call_id,
                result,
                is_error,
                untrusted_content,
            } => {
                unanswered_ids.retain(|id| id != call_id.as_str());
                out.push(WireMessage {
                    role: Role::Tool,
                    content: result.render_for_model(),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: None,
                    tool_call_id: Some(call_id.clone()),
                    tool_result_is_error: Some(*is_error),
                    untrusted_content: *untrusted_content,
                });
            }
            // Presentational / lifecycle kinds carry no provider-visible content.
            SessionEventKind::SessionCreated { .. }
            | SessionEventKind::RuntimeLinked { .. }
            | SessionEventKind::RuntimeConfigured { .. }
            | SessionEventKind::TurnStart { .. }
            | SessionEventKind::TurnEnd { .. }
            | SessionEventKind::StepStart { .. }
            | SessionEventKind::StepEnd { .. }
            | SessionEventKind::AgentInboxSplice { .. }
            | SessionEventKind::GoalChange { .. }
            | SessionEventKind::WorkflowChange { .. }
            | SessionEventKind::ScheduleChange { .. }
            | SessionEventKind::TeamChange { .. }
            | SessionEventKind::WorkChange { .. }
            | SessionEventKind::ReviewChange { .. }
            | SessionEventKind::CodeModeChange { .. }
            | SessionEventKind::RequestHeader { .. }
            | SessionEventKind::RequestContext { .. }
            // ATT02 owns route-capability-checked model projection.
            | SessionEventKind::AttachmentAdded { .. }
            | SessionEventKind::AssistantChunk { .. }
            | SessionEventKind::AssistantAudio { .. }
            | SessionEventKind::AssistantProviderItem { .. }
            | SessionEventKind::AssistantResponseMetadata { .. }
            | SessionEventKind::ServerToolCall { .. }
            | SessionEventKind::ServerToolResult { .. }
            | SessionEventKind::ServerToolUsage { .. }
            | SessionEventKind::AssistantCitation { .. }
            | SessionEventKind::ToolCall { .. }
            // Presentational / lifecycle kinds carry no provider content.
            | SessionEventKind::CompactionApplied { .. }
            | SessionEventKind::NativeCompactionApplied { .. }
            | SessionEventKind::PlanReview { .. }
            | SessionEventKind::PlanMode { .. }
            | SessionEventKind::SessionActivated {}
        | SessionEventKind::SessionTitle { .. } => {}
        }
    }
    close_unanswered(&mut out, &mut unanswered_ids);
    out
}

/// Emit one interrupted result for every still-unanswered call, in the order
/// the model requested them, and clear the pending set.
///
/// A no-op for [`Unanswered::Keep`], which never fills the set.
fn close_unanswered(out: &mut Vec<WireMessage>, unanswered_ids: &mut Vec<String>) {
    for id in unanswered_ids.drain(..) {
        out.push(WireMessage {
            role: Role::Tool,
            content: INTERRUPTED_TOOL_RESULT.to_owned(),
            attachments: Vec::new(),
            document_routes: Vec::new(),
            tool_calls: None,
            tool_call_id: Some(heycode_core::CallId::from_raw(id)),
            // An unknown outcome must not enter the conversation wearing the
            // shape of a successful one; the error flag is the only signal
            // every protocol carries, so unknown travels as not-success.
            tool_result_is_error: Some(true),
            untrusted_content: None,
        });
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::event::{TokenUsage, ToolCallOut, TurnEndReason};

    fn ev(seq: u64, kind: SessionEventKind) -> SessionEvent {
        SessionEvent {
            v: 1,
            seq,
            time_ms: 1,
            kind,
        }
    }

    #[test]
    fn compaction_shadows_history_and_injects_summary() {
        let events = vec![
            ev(
                0,
                SessionEventKind::UserMessage {
                    text: "old question".into(),
                },
            ),
            ev(
                1,
                SessionEventKind::AssistantMessage {
                    turn: 1,
                    step: 1,
                    content: "old answer".into(),
                    reasoning: None,
                    tool_calls: None,
                    usage: Some(TokenUsage {
                        prompt_tokens: 1,
                        completion_tokens: 1,
                    }),
                },
            ),
            ev(
                2,
                SessionEventKind::CompactionApplied {
                    summary: "earlier: user asked X; assistant answered Y".into(),
                    replaced_upto_seq: 1,
                },
            ),
            ev(
                3,
                SessionEventKind::UserMessage {
                    text: "new question".into(),
                },
            ),
        ];
        let msgs = derive_messages(&events);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].role, Role::User);
        assert!(msgs[0].content.contains("<compacted-summary>"));
        assert!(msgs[0].content.contains("answered Y"));
        assert_eq!(msgs[1].content, "new question");
    }

    #[test]
    fn latest_compaction_wins() {
        let events = vec![
            ev(0, SessionEventKind::UserMessage { text: "a".into() }),
            ev(
                1,
                SessionEventKind::CompactionApplied {
                    summary: "first".into(),
                    replaced_upto_seq: 0,
                },
            ),
            ev(2, SessionEventKind::UserMessage { text: "b".into() }),
            ev(
                3,
                SessionEventKind::CompactionApplied {
                    summary: "second".into(),
                    replaced_upto_seq: 2,
                },
            ),
            ev(4, SessionEventKind::UserMessage { text: "c".into() }),
        ];
        let msgs = derive_messages(&events);
        assert_eq!(msgs.len(), 2);
        assert!(msgs[0].content.contains("second"));
        assert!(!msgs[0].content.contains("first"));
        assert_eq!(msgs[1].content, "c");
    }

    #[test]
    fn user_message_projects_verbatim() {
        let events = [ev(
            0,
            SessionEventKind::UserMessage {
                text: "fix the bug".into(),
            },
        )];
        assert_eq!(
            derive_messages(&events),
            vec![WireMessage {
                role: Role::User,
                content: "fix the bug".into(),
                attachments: Vec::new(),
                document_routes: Vec::new(),
                tool_calls: None,
                tool_call_id: None,
                tool_result_is_error: None,
                untrusted_content: None,
            }]
        );
    }

    #[test]
    fn assistant_message_carries_tool_calls_with_raw_arguments() {
        let events = [ev(
            1,
            SessionEventKind::AssistantMessage {
                turn: 0,
                step: 1,
                content: String::new(),
                reasoning: Some("pondering".into()),
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_9".into(),
                    name: "bash".into(),
                    arguments: r#"{"cmd":"ls -la"}"#.into(),
                }]),
                usage: Some(TokenUsage {
                    prompt_tokens: 12,
                    completion_tokens: 34,
                }),
            },
        )];
        assert_eq!(
            derive_messages(&events),
            vec![WireMessage {
                role: Role::Assistant,
                content: String::new(),
                attachments: Vec::new(),
                document_routes: Vec::new(),
                tool_calls: Some(vec![WireToolCall {
                    id: "call_9".into(),
                    name: "bash".into(),
                    arguments: r#"{"cmd":"ls -la"}"#.into(),
                }]),
                tool_call_id: None,
                tool_result_is_error: None,
                untrusted_content: None,
            }]
        );
    }

    #[test]
    fn tool_result_binds_call_error_and_untrusted_content() {
        let events = [
            ev(2, SessionEventKind::UserMessage { text: "go".into() }), // padding keeps seq realistic
            ev(
                3,
                SessionEventKind::ToolResult {
                    call_id: heycode_core::CallId::from_raw("call_9"),
                    content: "total 0".into(),
                    is_error: true,
                    untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
                },
            ),
        ];
        assert_eq!(
            derive_messages(&events)[1],
            WireMessage {
                role: Role::Tool,
                content: "total 0".into(),
                attachments: Vec::new(),
                document_routes: Vec::new(),
                tool_calls: None,
                tool_call_id: Some(heycode_core::CallId::from_raw("call_9")),
                tool_result_is_error: Some(true),
                untrusted_content: Some(heycode_core::UntrustedContentBoundary::web()),
            }
        );
    }

    #[test]
    fn chunks_and_lifecycle_kinds_are_ignored() {
        let events = [
            ev(0, SessionEventKind::TurnStart { turn: 0 }),
            ev(1, SessionEventKind::StepStart { turn: 0, step: 0 }),
            ev(
                2,
                SessionEventKind::AssistantChunk {
                    turn: 0,
                    step: 0,
                    text: Some("partial".into()),
                    reasoning: None,
                },
            ),
            ev(
                3,
                SessionEventKind::ToolCall {
                    turn: 0,
                    call_id: heycode_core::CallId::from_raw("c"),
                    name: "bash".into(),
                    args: serde_json::json!({}),
                },
            ),
            ev(4, SessionEventKind::StepEnd { turn: 0, step: 0 }),
            ev(
                5,
                SessionEventKind::TurnEnd {
                    turn: 0,
                    reason: TurnEndReason::Stop,
                },
            ),
            // compaction/applied has dedicated projection tests above
        ];
        assert!(derive_messages(&events).is_empty());
    }

    #[test]
    fn the_faithful_fold_leaves_a_call_the_log_never_answered_unanswered() {
        let events = [ev(
            0,
            SessionEventKind::AssistantMessage {
                turn: 0,
                step: 0,
                content: String::new(),
                reasoning: None,
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                }]),
                usage: None,
            },
        )];
        let messages = derive_messages(&events);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Assistant);
    }

    #[test]
    fn the_repairing_fold_answers_that_call_as_interrupted_rather_than_dropping_it() {
        let events = [ev(
            0,
            SessionEventKind::AssistantMessage {
                turn: 0,
                step: 0,
                content: String::new(),
                reasoning: None,
                tool_calls: Some(vec![ToolCallOut {
                    id: "call_1".into(),
                    name: "bash".into(),
                    arguments: "{}".into(),
                }]),
                usage: None,
            },
        )];
        let messages = derive_messages_repaired(&events);
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1],
            WireMessage {
                role: Role::Tool,
                content: INTERRUPTED_TOOL_RESULT.to_owned(),
                attachments: Vec::new(),
                document_routes: Vec::new(),
                tool_calls: None,
                tool_call_id: Some(heycode_core::CallId::from_raw("call_1")),
                tool_result_is_error: Some(true),
                untrusted_content: None,
            }
        );
    }

    #[test]
    fn an_interrupted_answer_never_reads_as_a_completed_one() {
        // The exact failure this row forbids: a repaired call must not be able
        // to pass for output the tool produced.
        assert!(INTERRUPTED_TOOL_RESULT.contains("interrupted"));
        assert!(INTERRUPTED_TOOL_RESULT.contains("unknown"));
        assert!(INTERRUPTED_TOOL_RESULT.contains("Do not assume it succeeded"));
    }

    #[test]
    fn a_call_a_compaction_shadowed_needs_no_closing_message_and_gets_none() {
        let events = [
            ev(
                0,
                SessionEventKind::AssistantMessage {
                    turn: 0,
                    step: 0,
                    content: String::new(),
                    reasoning: None,
                    tool_calls: Some(vec![ToolCallOut {
                        id: "call_1".into(),
                        name: "bash".into(),
                        arguments: "{}".into(),
                    }]),
                    usage: None,
                },
            ),
            ev(
                1,
                SessionEventKind::CompactionApplied {
                    summary: "earlier work".into(),
                    replaced_upto_seq: 0,
                },
            ),
            ev(
                2,
                SessionEventKind::UserMessage {
                    text: "next".into(),
                },
            ),
        ];
        let messages = derive_messages_repaired(&events);
        assert_eq!(
            messages.iter().map(|m| m.role).collect::<Vec<_>>(),
            vec![Role::User, Role::User],
            "the summary and the new message; the shadowed call is not on the wire to answer"
        );
        assert_eq!(messages, derive_messages(&events));
    }

    #[test]
    fn consecutive_assistant_messages_stay_separate() {
        let mk = |content: &str| SessionEventKind::AssistantMessage {
            turn: 0,
            step: 0,
            content: content.into(),
            reasoning: None,
            tool_calls: None,
            usage: None,
        };
        let events = [ev(0, mk("one")), ev(1, mk("two"))];
        let msgs = derive_messages(&events);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].content, "one");
        assert_eq!(msgs[1].content, "two");
    }
}
