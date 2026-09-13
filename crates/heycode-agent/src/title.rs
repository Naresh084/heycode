//! Session titling: one short durable name per session.
//!
//! After the first successful turn (opt-in via `AgentOptions::auto_title`)
//! a background task asks the provider for a ≤6-word title and appends the
//! log-only `session/title` event. `/title <text>` sets one manually; the
//! latest title always wins on fold. Titles never enter model requests.

use std::sync::Arc;

use futures::StreamExt;

use heycode_core::EventBus;
use heycode_llm::{ChatRequest, LlmSelection, ProviderRegistry, StreamChunk};
use heycode_session::{Session, SessionEventKind};

use crate::ui::UiEvent;

/// Character cap fed to the summarizer.
const EXCERPT_MAX_CHARS: usize = 2_000;

/// Current title from the log, if any.
#[must_use]
pub fn current(events: &[heycode_session::SessionEvent]) -> Option<String> {
    events.iter().rev().find_map(|e| match &e.kind {
        SessionEventKind::SessionTitle { title } => Some(title.clone()),
        _ => None,
    })
}

/// Append a title durably (manual `/title` or generated). Latest wins.
///
/// # Errors
/// Log append failures.
pub async fn set(
    session: &Arc<std::sync::Mutex<Session>>,
    title: String,
) -> Result<(), heycode_session::AppendError> {
    let mut s = session.lock().unwrap_or_else(|e| e.into_inner());
    s.append(SessionEventKind::SessionTitle {
        title: title.clone(),
    })?;
    Ok(())
}

/// Spawn the one-shot background titler for the first turn.
pub(crate) fn spawn_titler(
    providers: Arc<ProviderRegistry>,
    selection: LlmSelection,
    session: Arc<std::sync::Mutex<Session>>,
    bus: EventBus,
) {
    // Skip when a title already exists (resumed sessions).
    {
        let s = session.lock().unwrap_or_else(|e| e.into_inner());
        if current(s.events()).is_some() {
            return;
        }
    }
    tokio::spawn(async move {
        let excerpt = {
            let s = session.lock().unwrap_or_else(|e| e.into_inner());
            let mut text = String::new();
            for e in s.events() {
                if let SessionEventKind::UserMessage { text: t } = &e.kind {
                    text.push_str(t);
                    text.push('\n');
                }
                if text.chars().count() >= EXCERPT_MAX_CHARS {
                    break;
                }
            }
            let cut: String = text.chars().take(EXCERPT_MAX_CHARS).collect();
            cut
        };
        if excerpt.trim().is_empty() {
            return;
        }

        let Some(provider) = providers.get(&selection.provider_name) else {
            return;
        };
        let request = ChatRequest {
            model: selection.model.clone(),
            messages: vec![
                heycode_llm::ChatMessage::system(
                    "Write a 3-6 word title naming the session's main task. \
                     Reply with ONLY the title text — no quotes, no punctuation at the end.",
                ),
                heycode_llm::ChatMessage::user(excerpt),
            ],
            tools: None,
            temperature: None,
            max_tokens: None,
        };
        let mut stream = provider.stream(request);
        let mut title = String::new();
        while let Some(item) = stream.next().await {
            match item {
                Ok(StreamChunk::TextDelta(t)) => title.push_str(&t),
                Ok(StreamChunk::Finish(_)) => break,
                Ok(_) => {}
                Err(_) => return, // titling is best-effort by design
            }
        }
        let title = normalize(&title);
        if title.is_empty() {
            return;
        }
        let appended = {
            let mut session = session.lock().unwrap_or_else(|error| error.into_inner());
            // A manual /rename may have committed while the provider was
            // generating this title. Check and append under the same lock.
            current(session.events()).is_none()
                && session
                    .append(SessionEventKind::SessionTitle {
                        title: title.clone(),
                    })
                    .is_ok()
        };
        if appended {
            bus.emit(UiEvent::Info {
                text: format!("titled: {title}"),
            });
        }
    });
}

/// Trim, strip wrapping quotes, collapse newlines, cap length.
fn normalize(raw: &str) -> String {
    let mut t = raw.trim().trim_matches('"').trim().to_owned();
    t = t.lines().next().unwrap_or_default().trim().to_owned();
    if t.chars().count() > 60 {
        t = t.chars().take(60).collect();
    }
    t
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_quotes_and_newlines() {
        assert_eq!(normalize("\"Fix login bug\"\n"), "Fix login bug");
        assert_eq!(normalize("  multi\nline  "), "multi");
        assert_eq!(normalize(&"x".repeat(100)).chars().count(), 60);
    }

    #[test]
    fn current_folds_latest_title() {
        let mk = |seq: u64, kind| heycode_session::SessionEvent {
            v: 1,
            seq,
            time_ms: 0,
            kind,
        };
        let events = vec![
            mk(0, SessionEventKind::UserMessage { text: "hi".into() }),
            mk(
                1,
                SessionEventKind::SessionTitle {
                    title: "first".into(),
                },
            ),
            mk(
                2,
                SessionEventKind::SessionTitle {
                    title: "second".into(),
                },
            ),
        ];
        assert_eq!(current(&events).as_deref(), Some("second"));
    }
}
