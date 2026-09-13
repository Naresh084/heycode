//! Revision-checked settings picker for the native automatic compaction window.
use crate::{panel_frame as panel, terminal::Styles};
use crossterm::event::{KeyCode, KeyModifiers};
use heycode_settings::{SettingsNamespace, SettingsService};
use ratatui::{
    style::{Style, Stylize},
    text::{Line, Span},
};
use std::sync::Arc;

pub(crate) struct AutoCompactPanel {
    settings: Arc<SettingsService>,
    revision: u64,
    current: Option<u64>,
    selected: usize,
    choices: Vec<Option<u64>>,
    enabled: bool,
    error: Option<String>,
}

impl AutoCompactPanel {
    pub(crate) fn open(settings: Arc<SettingsService>, enabled: bool) -> Result<Self, String> {
        let namespace = SettingsNamespace::new("autocompact").map_err(|error| error.to_string())?;
        let snapshot = settings
            .get(&namespace)
            .map_err(|error| error.to_string())?
            .ok_or("Auto-compaction settings are unavailable.")?;
        let current = match snapshot
            .resolved()
            .get("mode")
            .and_then(serde_json::Value::as_str)
        {
            Some("auto") => None,
            Some("fixed") => Some(
                snapshot
                    .resolved()
                    .get("tokens")
                    .and_then(serde_json::Value::as_u64)
                    .filter(|tokens| (100_000..=1_000_000).contains(tokens))
                    .ok_or("Invalid auto-compaction window.")?,
            ),
            _ => return Err("Invalid auto-compaction mode.".to_owned()),
        };
        let mut choices = vec![None];
        choices.extend((1..=10).map(|n| Some(n * 100_000)));
        if !choices.contains(&current) {
            choices.push(current);
            choices.sort_unstable();
        }
        let selected = choices
            .iter()
            .position(|choice| *choice == current)
            .unwrap_or(0);
        Ok(Self {
            settings,
            revision: snapshot.revision(),
            current,
            selected,
            choices,
            enabled,
            error: None,
        })
    }

    pub(crate) fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Option<String> {
        if !modifiers.is_empty() {
            return None;
        }
        match code {
            KeyCode::Esc => {
                return Some(self.unchanged_receipt());
            }
            KeyCode::Up => self.selected = (self.selected + 1) % self.choices.len(),
            KeyCode::Down => {
                self.selected = (self.selected + self.choices.len() - 1) % self.choices.len()
            }
            KeyCode::Enter => {
                let selected = self.choices[self.selected];
                if selected == self.current {
                    return Some(self.unchanged_receipt());
                }
                if !self.enabled || !self.settings.writable() {
                    self.error = Some(
                        "Auto-compaction settings cannot be changed in this profile.".to_owned(),
                    );
                    return None;
                }
                let result = self.commit(selected);
                match result {
                    Ok(applied) => {
                        return Some(format!("Auto-compact window set to: {}", label(applied)));
                    }
                    Err(error) => self.error = Some(error),
                }
            }
            _ => {}
        }
        None
    }

    fn unchanged_receipt(&self) -> String {
        match Self::open(self.settings.clone(), self.enabled) {
            Ok(fresh) => format!("Auto-compact window unchanged: {}", label(fresh.current)),
            Err(_) => "Auto-compact window unchanged".to_owned(),
        }
    }

    fn commit(&self, selected: Option<u64>) -> Result<Option<u64>, String> {
        let namespace = SettingsNamespace::new("autocompact").map_err(|error| error.to_string())?;
        let section = serde_json::json!({ "mode": if selected.is_some() { "fixed" } else { "auto" }, "tokens": selected.unwrap_or(200_000) });
        let committed = self
            .settings
            .replace_user(&namespace, section, Some(self.revision))
            .map_err(|error| error.to_string())?;
        // Settings watchers apply the effective value to the live control. Report
        // the resolved layer, even when a project/managed override wins.
        Ok(
            if committed
                .resolved()
                .get("mode")
                .and_then(serde_json::Value::as_str)
                == Some("fixed")
            {
                committed
                    .resolved()
                    .get("tokens")
                    .and_then(serde_json::Value::as_u64)
            } else {
                None
            },
        )
    }

    pub(crate) fn lines(&self, width: u16, styles: Styles) -> Vec<Line<'static>> {
        let mut lines = vec![
            panel::title("Auto-compact window", styles),
            panel::note(&format!("Current setting: {}", label(self.current)), styles),
            panel::blank(),
        ];
        lines.extend(panel::wrap("This command configures when auto-compaction happens. The actual threshold is capped by the active model's available input budget.", width, Style::default().fg(styles.text())));
        lines.push(panel::blank());
        lines.extend(panel::wrap("The auto setting uses a window tuned to the active model and profile. It is recommended for normal use; you can override it below.", width, Style::default().fg(styles.text())));
        lines.push(panel::blank());
        if self.choices[self.selected].is_some() {
            lines.extend(panel::wrap(
                "Overriding auto may increase token usage, especially when resuming long sessions.",
                width,
                Style::default().fg(styles.warn()),
            ));
            lines.push(panel::blank());
        }
        lines.push(Line::from(vec![
            Span::raw(format!("{}Select auto-compact window: ", panel::INDENT)),
            Span::styled(
                match self.choices[self.selected] {
                    None => "auto".to_owned(),
                    Some(tokens) => format!("{} tokens", label(Some(tokens))),
                },
                Style::default().fg(styles.accent()).bold(),
            ),
        ]));
        if !self.enabled {
            lines.extend(panel::wrap_note(
                "Automatic compaction is disabled by this profile.",
                width,
                styles,
            ));
        }
        if !self.settings.writable() {
            lines.extend(panel::wrap_note(
                "Settings are read-only in this profile.",
                width,
                styles,
            ));
        }
        if let Some(error) = &self.error {
            lines.extend(panel::wrap_note(error, width, styles));
        }
        lines.push(panel::blank());
        lines.extend(panel::wrap_note(
            "↑/↓ to change · Enter to apply · Esc to cancel",
            width,
            styles,
        ));
        lines
    }
}

fn label(tokens: Option<u64>) -> String {
    match tokens {
        None => "auto".to_owned(),
        Some(1_000_000) => "1m".to_owned(),
        Some(tokens) if tokens.is_multiple_of(1_000) => format!("{}k", tokens / 1_000),
        Some(tokens) => tokens.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use heycode_settings::{SettingsDefinition, SettingsDocuments, SettingsSchema, SettingsWriter};
    use std::sync::Mutex;

    struct Writer {
        writes: Mutex<Vec<serde_json::Value>>,
        fail: bool,
    }
    impl SettingsWriter for Writer {
        fn persist_user(
            &self,
            _: &SettingsNamespace,
            section: &serde_json::Value,
        ) -> Result<(), String> {
            if self.fail {
                return Err("controlled persistence failure".to_owned());
            }
            self.writes.lock().unwrap().push(section.clone());
            Ok(())
        }
    }
    fn fixture(fail: bool) -> (AutoCompactPanel, Arc<Writer>, heycode_core::Context) {
        let writer = Arc::new(Writer {
            writes: Mutex::new(Vec::new()),
            fail,
        });
        let settings = Arc::new(SettingsService::with_writer(
            SettingsDocuments::new(),
            writer.clone(),
        ));
        let context = heycode_core::Context::new();
        let schema = SettingsSchema::new(serde_json::json!({ "type":"object", "properties": { "mode":{"type":"string","enum":["auto","fixed"]}, "tokens":{"type":"integer","minimum":100000,"maximum":1000000} } }), serde_json::json!({"mode":"auto","tokens":200000}), |_| Ok(())).unwrap();
        settings
            .register(
                &context,
                SettingsDefinition::new(SettingsNamespace::new("autocompact").unwrap(), schema),
            )
            .unwrap();
        (
            AutoCompactPanel::open(settings, true).unwrap(),
            writer,
            context,
        )
    }
    #[test]
    fn arrows_follow_reference_direction_and_wrap() {
        let (mut panel, writer, _context) = fixture(false);
        panel.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(panel.choices[panel.selected], Some(1_000_000));
        panel.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(panel.choices[panel.selected], None);
        for tokens in [100_000, 200_000, 300_000] {
            panel.handle_key(KeyCode::Up, KeyModifiers::NONE);
            assert_eq!(panel.choices[panel.selected], Some(tokens));
        }
        assert!(writer.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn cancel_and_modified_enter_do_not_write() {
        let (mut panel, writer, _context) = fixture(false);
        panel.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            panel.handle_key(KeyCode::Enter, KeyModifiers::CONTROL),
            None
        );
        assert_eq!(
            panel
                .handle_key(KeyCode::Esc, KeyModifiers::NONE)
                .as_deref(),
            Some("Auto-compact window unchanged: auto")
        );
        assert!(writer.writes.lock().unwrap().is_empty());
    }
    #[test]
    fn apply_persists_exact_choice_and_notifies_live_settings_owner() {
        let (mut panel, writer, context) = fixture(false);
        let observed = Arc::new(Mutex::new(None));
        let capture = observed.clone();
        panel
            .settings
            .watch(
                &context,
                &SettingsNamespace::new("autocompact").unwrap(),
                move |change| {
                    *capture.lock().unwrap() = Some(change.next().resolved().clone());
                },
            )
            .unwrap();
        panel.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(
            panel
                .handle_key(KeyCode::Enter, KeyModifiers::NONE)
                .as_deref(),
            Some("Auto-compact window set to: 100k")
        );
        assert_eq!(
            writer.writes.lock().unwrap().as_slice(),
            &[serde_json::json!({"mode":"fixed","tokens":100000})]
        );
        assert_eq!(
            *observed.lock().unwrap(),
            Some(serde_json::json!({"mode":"fixed","tokens":100000}))
        );
    }
    #[test]
    fn persistence_failure_keeps_picker_and_current_value() {
        let (mut panel, writer, _context) = fixture(true);
        panel.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(panel.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
        assert!(panel.error.is_some());
        assert_eq!(panel.current, None);
        assert!(writer.writes.lock().unwrap().is_empty());
        let reopened = AutoCompactPanel::open(panel.settings.clone(), true).unwrap();
        assert_eq!(reopened.current, None);
    }
    #[test]
    fn stale_picker_cannot_overwrite_newer_settings() {
        let (mut panel, writer, _context) = fixture(false);
        panel
            .settings
            .replace_user(
                &SettingsNamespace::new("autocompact").unwrap(),
                serde_json::json!({"mode":"fixed","tokens":300000}),
                Some(panel.revision),
            )
            .unwrap();
        panel.handle_key(KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(panel.handle_key(KeyCode::Enter, KeyModifiers::NONE), None);
        assert!(panel.error.is_some());
        assert_eq!(writer.writes.lock().unwrap().len(), 1);
        let reopened = AutoCompactPanel::open(panel.settings.clone(), true).unwrap();
        assert_eq!(reopened.current, Some(300000));
        assert_eq!(
            panel
                .handle_key(KeyCode::Esc, KeyModifiers::NONE)
                .as_deref(),
            Some("Auto-compact window unchanged: 300k")
        );
    }
}
