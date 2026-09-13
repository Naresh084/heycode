//! A session-local detailed transcript, without changing stored messages.

use crossterm::event::{KeyCode, KeyEvent};
use heycode_ui::keymap::KeymapAction;

use super::{AppState, Item};

impl AppState {
    /// Whether retained tool and reasoning details replace the composer.
    #[must_use]
    pub const fn detailed_transcript(&self) -> bool {
        self.detailed_transcript
    }

    /// The actual configured key, so the footer also honors rebinding.
    #[must_use]
    pub fn transcript_toggle_hint(&self) -> String {
        self.keymap
            .chord(KeymapAction::ToggleTranscript)
            .to_string()
    }

    pub(super) fn toggle_detailed_transcript(&mut self) {
        self.detailed_transcript = !self.detailed_transcript;
        self.focus_reasoning_item(None);
        self.shortcut_list_open = false;
        for item in &mut self.items {
            set_detail(item, self.detailed_transcript);
        }
        self.tool_group_signature = None;
        self.transcript_cache.invalidate_layout();
    }

    /// New calls and replayed records inherit an already-open global view.
    pub(super) fn expand_detailed_transcript(&mut self) {
        if self.detailed_transcript {
            for item in &mut self.items {
                set_detail(item, true);
            }
        }
    }

    /// The hidden composer never accepts or submits text while inspecting.
    pub(super) fn handle_detailed_transcript_key(&mut self, key: KeyEvent) -> bool {
        if !self.detailed_transcript {
            return false;
        }
        let action = crate::terminal::chord(key).and_then(|chord| self.keymap.action(chord));
        if matches!(action, Some(KeymapAction::ToggleTranscript)) || key.code == KeyCode::Esc {
            self.toggle_detailed_transcript();
            return true;
        }
        if key.modifiers.is_empty() {
            match key.code {
                KeyCode::Up => self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(1),
                KeyCode::Down => {
                    self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(1)
                }
                KeyCode::Char('?') => self.shortcut_list_open = !self.shortcut_list_open,
                KeyCode::Backspace => self.shortcut_list_open = false,
                _ => {}
            }
        }
        // Route scrolling here: Ctrl+U otherwise edits a non-empty draft.
        match action {
            Some(KeymapAction::ScrollHalfPageUp | KeymapAction::ScrollPageUp) => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_add(20)
            }
            Some(KeymapAction::ScrollHalfPageDown | KeymapAction::ScrollPageDown) => {
                self.scroll_from_bottom = self.scroll_from_bottom.saturating_sub(20)
            }
            Some(KeymapAction::ScrollToOldest) => self.scroll_from_bottom = usize::MAX,
            Some(KeymapAction::ScrollToNewest) => self.scroll_from_bottom = 0,
            Some(KeymapAction::Quit) => return false,
            _ => {}
        }
        true
    }
}

fn set_detail(item: &mut Item, expanded: bool) {
    match item {
        Item::Tool { view, .. } => view.expanded = expanded,
        Item::Reasoning { view, .. } => view.expanded = Some(expanded),
        Item::FindingsReport {
            expanded: value, ..
        }
        | Item::Compaction {
            expanded: value, ..
        } => *value = expanded,
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{Event, KeyModifiers};
    use serde_json::json;

    fn ctrl_o(state: &mut AppState) {
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
    }

    #[test]
    fn global_details_expand_existing_and_later_tools_and_preserve_the_draft() {
        let mut state = AppState::new("model", std::env::temp_dir());
        state.replace_input("unsent draft".to_owned());
        let tool = Item::Tool {
            call_id: None,
            name: "bash".to_owned(),
            args: json!({"command":"printf fixture"}),
            result: Some((true, json!("retained output"))),
            untrusted_content: None,
            view: Default::default(),
        };
        state.items.push(tool.clone());
        ctrl_o(&mut state);
        assert!(state.detailed_transcript());
        state.items.push(tool);
        state.refresh_tool_groups();
        assert!(state.items.iter().all(
            |item| matches!(item, Item::Tool { view, .. } if view.expanded && !view.group_hidden)
        ));
        state.handle_terminal_event(&Event::Paste("/quit\n".into()));
        state.handle_terminal_event(&Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )));
        state.handle_terminal_event(&Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)));
        assert_eq!(state.scroll_from_bottom, 1);
        assert_eq!(state.input.lines(), ["unsent draft"]);
        assert!(state.pending_send.is_none());
        ctrl_o(&mut state);
        assert!(!state.detailed_transcript());
        assert!(
            state
                .items
                .iter()
                .all(|item| matches!(item, Item::Tool { view, .. } if !view.expanded))
        );
        assert_eq!(state.input.lines(), ["unsent draft"]);
    }

    #[test]
    fn old_compaction_binding_resolves_to_global_transcript_action() {
        assert_eq!(
            KeymapAction::parse("toggle-compaction"),
            Ok(KeymapAction::ToggleTranscript)
        );
        assert_eq!(KeymapAction::ToggleTranscript.as_str(), "toggle-transcript");
    }
}
