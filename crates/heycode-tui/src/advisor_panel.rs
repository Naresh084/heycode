//! Persistent advisor route picker.
//!
//! This module owns only interaction and projection. Persistence, route
//! validation and live application remain in `heycode_agent::AdvisorService`.

use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};
use heycode_agent::{AdvisorRouteChoice, AdvisorSelection, AdvisorStatus};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Style, Stylize},
    text::Line,
    widgets::{Block, Borders, Clear, Paragraph},
};
use tokio_util::sync::CancellationToken;

use crate::command_palette::fuzzy_score;

#[derive(Debug, Default)]
struct BridgeState {
    attached: bool,
    pending: Option<AdvisorPanelView>,
}

/// Single-slot handoff from `/advisor` to the interactive shell.
#[derive(Debug, Clone, Default)]
pub struct AdvisorPanelBridge {
    state: Arc<Mutex<BridgeState>>,
}

impl AdvisorPanelBridge {
    /// Empty detached bridge.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the live shell consumer attached.
    pub fn attach(&self) {
        self.lock().attached = true;
    }

    /// Whether a live shell consumes picker requests.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.lock().attached
    }

    /// Replace the pending open request. Slash commands are serialized, so a
    /// second unconsumed request means the latest committed status is the one
    /// the shell should render.
    pub fn request(&self, view: AdvisorPanelView) {
        self.lock().pending = Some(view);
    }

    /// Take the pending picker/status request.
    #[must_use]
    pub fn take(&self) -> Option<AdvisorPanelView> {
        self.lock()
            .pending
            .take()
            .filter(|view| !view.operation_cancelled())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BridgeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// One commit intent returned to the owning application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisorPanelAction {
    /// Persistently disable consultation.
    Disable,
    /// Persist and immediately apply one exact owner/model route.
    Select(AdvisorSelection),
}

/// One safe picker row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorPanelRow {
    /// Human-facing provider label.
    pub owner_label: String,
    /// Stable provider-qualified owner key.
    pub owner_key: String,
    /// Human-facing model label.
    pub model_label: String,
    /// Exact model id.
    pub model: String,
    /// Whether this row is the committed live route.
    pub current: bool,
    /// Exact commit performed by Enter.
    pub action: AdvisorPanelAction,
}

/// Immutable status facts copied from the owning service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdvisorPanelStatus {
    /// Enabled exact route, or disabled.
    pub selection: Option<AdvisorSelection>,
    /// Live route generation.
    pub generation: u64,
    /// Persistent settings revision.
    pub settings_revision: u64,
    /// Descendant inference requests already reserved this session.
    pub descendant_requests_reserved: u64,
    /// Configured descendant request bound; zero means unlimited.
    pub descendant_request_limit: u64,
    /// Configured per-request output bound; zero means provider/model default.
    pub descendant_output_limit: u32,
}

impl From<&AdvisorStatus> for AdvisorPanelStatus {
    fn from(status: &AdvisorStatus) -> Self {
        Self {
            selection: status.selection.clone(),
            generation: status.generation,
            settings_revision: status.settings_revision,
            descendant_requests_reserved: status.descendant_budget.requests_reserved,
            descendant_request_limit: status.descendant_budget.limits.max_requests,
            descendant_output_limit: status.descendant_budget.limits.max_output_tokens,
        }
    }
}

/// Searchable no-argument `/advisor` picker state.
#[derive(Debug)]
pub struct AdvisorPanelView {
    status: AdvisorPanelStatus,
    rows: Vec<AdvisorPanelRow>,
    query: String,
    cursor: usize,
    notice: Option<String>,
    warnings: Vec<String>,
    operation: Option<CancellationToken>,
    loading: bool,
}

impl AdvisorPanelView {
    /// Build a picker from one atomic service status and its current catalog rows.
    #[must_use]
    pub fn new(status: &AdvisorStatus, choices: Vec<AdvisorRouteChoice>) -> Self {
        let current = status.selection.as_ref();
        let mut rows = choices
            .into_iter()
            .map(|choice| {
                let selection = choice.selection;
                AdvisorPanelRow {
                    owner_label: choice.owner_label,
                    owner_key: selection.owner_key(),
                    model_label: choice.model_label,
                    model: selection.model().to_owned(),
                    current: current == Some(&selection),
                    action: AdvisorPanelAction::Select(selection),
                }
            })
            .collect::<Vec<_>>();
        rows.push(AdvisorPanelRow {
            owner_label: "No advisor".to_owned(),
            owner_key: "off".to_owned(),
            model_label: "Do not expose the advisor tool".to_owned(),
            model: String::new(),
            current: current.is_none(),
            action: AdvisorPanelAction::Disable,
        });
        let cursor = rows.iter().position(|row| row.current).unwrap_or(0);
        Self {
            status: status.into(),
            rows,
            query: String::new(),
            cursor,
            notice: None,
            warnings: Vec::new(),
            operation: None,
            loading: false,
        }
    }

    /// Build the non-interactive loading projection shown while every
    /// connected native provider catalog is refreshed. Escape owns the same
    /// cancellation token awaited by the command.
    #[must_use]
    pub fn loading(status: &AdvisorStatus, cancellation: CancellationToken) -> Self {
        let mut view = Self::new(status, Vec::new());
        view.rows.clear();
        view.cursor = 0;
        view.operation = Some(cancellation);
        view.loading = true;
        view
    }

    /// Bind a loaded replacement to the catalog operation that produced it.
    /// If Escape won the race, the bridge discards this replacement instead of
    /// reopening a cancelled picker.
    #[must_use]
    pub fn for_operation(mut self, cancellation: CancellationToken) -> Self {
        self.operation = Some(cancellation);
        self
    }

    /// Add concise feedback for an already-committed slash-command update.
    #[must_use]
    pub fn with_notice(mut self, notice: impl Into<String>) -> Self {
        self.notice = Some(notice.into());
        self
    }

    /// Add honest provider/catalog diagnostics to a loaded picker.
    #[must_use]
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }

    /// Whether provider model catalogs are still loading.
    #[must_use]
    pub fn is_loading(&self) -> bool {
        self.loading
    }

    /// Immutable live/persistent status facts.
    #[must_use]
    pub const fn status(&self) -> &AdvisorPanelStatus {
        &self.status
    }

    /// Current search text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Optional update feedback.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Ranked filtered rows. Empty queries preserve provider/model order.
    #[must_use]
    pub fn visible_rows(&self) -> Vec<&AdvisorPanelRow> {
        let query = self.query.trim().to_ascii_lowercase();
        if query.is_empty() {
            return self.rows.iter().collect();
        }
        let mut matches = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| {
                let score = [
                    fuzzy_score(&row.owner_key.to_ascii_lowercase(), &query, 0),
                    fuzzy_score(&row.model.to_ascii_lowercase(), &query, 40),
                    fuzzy_score(&row.owner_label.to_ascii_lowercase(), &query, 80),
                    fuzzy_score(&row.model_label.to_ascii_lowercase(), &query, 120),
                ]
                .into_iter()
                .flatten()
                .min()?;
                Some((score, index, row))
            })
            .collect::<Vec<_>>();
        matches.sort_by_key(|(score, index, _)| (*score, *index));
        matches.into_iter().map(|(_, _, row)| row).collect()
    }

    /// Selected visible row.
    #[must_use]
    pub fn selected(&self) -> Option<&AdvisorPanelRow> {
        self.visible_rows().get(self.cursor).copied()
    }

    /// Handle one modal event. Selection is returned for the owning app to
    /// validate and commit through `AdvisorService`; Escape closes.
    pub fn handle(&mut self, event: &Event) -> AdvisorPanelOutcome {
        let Event::Key(key) = event else {
            return AdvisorPanelOutcome::Handled;
        };
        if key.kind != KeyEventKind::Press {
            return AdvisorPanelOutcome::Handled;
        }
        if self.loading {
            if key.code == KeyCode::Esc {
                if let Some(cancellation) = &self.operation {
                    cancellation.cancel();
                }
                return AdvisorPanelOutcome::Close;
            }
            return AdvisorPanelOutcome::Handled;
        }
        match key.code {
            KeyCode::Esc => AdvisorPanelOutcome::Close,
            KeyCode::Enter => self.selected().map_or(AdvisorPanelOutcome::Handled, |row| {
                AdvisorPanelOutcome::Commit(row.action.clone())
            }),
            KeyCode::Up => {
                let count = self.visible_rows().len();
                self.cursor = if count == 0 {
                    0
                } else {
                    self.cursor.checked_sub(1).unwrap_or(count - 1)
                };
                AdvisorPanelOutcome::Handled
            }
            KeyCode::Down => {
                let count = self.visible_rows().len();
                self.cursor = if count == 0 {
                    0
                } else {
                    (self.cursor + 1) % count
                };
                AdvisorPanelOutcome::Handled
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.cursor = 0;
                AdvisorPanelOutcome::Handled
            }
            KeyCode::Char(character)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !character.is_control()
                    && self.query.len() < 256 =>
            {
                self.query.push(character);
                self.cursor = 0;
                AdvisorPanelOutcome::Handled
            }
            _ => AdvisorPanelOutcome::Handled,
        }
    }

    /// Complete semantic representation for screen readers and snapshot tests.
    #[must_use]
    pub fn accessible_lines(&self) -> Vec<String> {
        self.lines(false)
    }

    fn lines(&self, compact: bool) -> Vec<String> {
        let mut lines = self.prefix_lines();
        if self.is_loading() {
            lines.push("Loading connected provider model catalogs…".to_owned());
            lines.push(String::new());
            lines.push("Esc to cancel".to_owned());
            return lines;
        }
        let visible = self.visible_rows();
        if visible.is_empty() {
            lines.push("No advisor routes match this filter".to_owned());
        } else {
            lines.extend(
                visible
                    .into_iter()
                    .enumerate()
                    .map(|(index, row)| self.row_line(index, index == self.cursor, row, compact)),
            );
        }
        lines.extend(self.footer_lines());
        lines
    }

    fn display_lines(&self, max_lines: usize) -> Vec<String> {
        if max_lines == 0 {
            return Vec::new();
        }
        let complete = self.lines(true);
        if complete.len() <= max_lines || self.is_loading() {
            return complete.into_iter().take(max_lines).collect();
        }

        let prefix = self.prefix_lines();
        let footer = self.footer_lines();
        let prefix_len = prefix.len();
        let fixed_lines = prefix_len.saturating_add(footer.len());
        let available_rows = max_lines.saturating_sub(fixed_lines);
        if available_rows == 0 {
            return complete.into_iter().take(max_lines).collect();
        }

        let visible = self.visible_rows();
        if visible.is_empty() {
            return complete.into_iter().take(max_lines).collect();
        }
        let available_rows = available_rows.min(visible.len());
        let start = self
            .cursor
            .saturating_sub(available_rows / 2)
            .min(visible.len().saturating_sub(available_rows));
        let end = start + available_rows;

        let mut lines = prefix;
        lines.extend(visible[start..end].iter().enumerate().map(|(offset, row)| {
            let index = start + offset;
            self.row_line(index, index == self.cursor, row, true)
        }));
        lines.extend(footer);
        lines
    }

    /// Bounded bottom-panel height, including its top separator.
    #[must_use]
    pub fn desired_height(&self) -> u16 {
        u16::try_from(self.accessible_lines().len().saturating_add(1))
            .unwrap_or(u16::MAX)
            .min(22)
    }

    fn prefix_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Advisor".to_owned(),
            "Choose a model for optional consultation.".to_owned(),
            String::new(),
        ];
        if !self.query.is_empty() {
            lines.push(format!("Filter: {}", self.query));
        }
        lines.extend(self.warnings.iter().map(|warning| format!("⚠ {warning}")));
        if let Some(notice) = &self.notice {
            lines.push(notice.clone());
        }
        lines
    }

    fn footer_lines(&self) -> [String; 2] {
        [
            String::new(),
            "Type to filter · ↑/↓ to select · Enter to confirm · Esc to cancel".to_owned(),
        ]
    }

    fn row_line(
        &self,
        index: usize,
        selected: bool,
        row: &AdvisorPanelRow,
        compact: bool,
    ) -> String {
        let marker = if selected { "› " } else { "  " };
        let current = if row.current { "  ✔ current" } else { "" };
        match &row.action {
            AdvisorPanelAction::Disable => {
                format!(
                    "{marker}{}. No advisor — do not consult another model{current}",
                    index + 1
                )
            }
            AdvisorPanelAction::Select(_) if compact => format!(
                "{marker}{}. {} — {}{current}",
                index + 1,
                row.owner_label,
                row.model
            ),
            AdvisorPanelAction::Select(_) => format!(
                "{marker}{}. {} — {} ({} · {}){current}",
                index + 1,
                row.owner_label,
                row.model_label,
                row.model,
                row.owner_key
            ),
        }
    }

    fn operation_cancelled(&self) -> bool {
        self.operation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }
}

/// Result of one picker event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdvisorPanelOutcome {
    /// Event consumed; panel remains open.
    Handled,
    /// Close without changing the route.
    Close,
    /// Validate and commit this exact action through the service.
    Commit(AdvisorPanelAction),
}

/// Draw an unboxed advisor picker at the bottom of the owning surface.
pub fn draw(frame: &mut Frame<'_>, view: &AdvisorPanelView, area: Rect) {
    let margin = 2.min(area.width.saturating_sub(1) / 2);
    let width = area.width.saturating_sub(margin.saturating_mul(2)).max(1);
    let height = view.desired_height().min(area.height).max(1);
    let popup = Rect::new(
        area.x.saturating_add(margin),
        area.y.saturating_add(area.height.saturating_sub(height)),
        width,
        height,
    );
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(crate::palette::DIM));
    let inner = block.inner(popup);
    frame.render_widget(block, popup);
    let lines = view
        .display_lines(usize::from(inner.height))
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if line.starts_with("› ") {
                Line::styled(line, Style::default().bold().fg(crate::palette::ACCENT))
            } else if line.starts_with('⚠') {
                Line::styled(line, Style::default().fg(crate::palette::WARN))
            } else if index == 0 {
                Line::styled(line, Style::default().bold())
            } else if index == 1 || line.starts_with("Type to filter") || line == "Esc to cancel" {
                Line::styled(line, Style::default().fg(crate::palette::DIM))
            } else {
                Line::from(line)
            }
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use heycode_agent::{BackendControlOwner, SubagentBudgetLimits, SubagentBudgetSnapshot};
    use ratatui::{Terminal, backend::TestBackend};

    fn selection(provider: &str, model: &str) -> AdvisorSelection {
        AdvisorSelection::new(
            BackendControlOwner::NativeInference {
                provider: provider.to_owned(),
            },
            model,
            None,
        )
        .unwrap()
    }

    fn view() -> AdvisorPanelView {
        let selected = selection("direct", "strong");
        AdvisorPanelView::new(
            &AdvisorStatus {
                selection: Some(selected.clone()),
                generation: 2,
                settings_revision: 7,
                descendant_budget: SubagentBudgetSnapshot {
                    limits: SubagentBudgetLimits {
                        max_requests: 12,
                        max_output_tokens: 4_096,
                        ..Default::default()
                    },
                    requests_reserved: 3,
                    in_flight: 0,
                },
            },
            vec![
                AdvisorRouteChoice {
                    selection: selected,
                    owner_label: "Direct".into(),
                    model_label: "Strong".into(),
                },
                AdvisorRouteChoice {
                    selection: selection("openrouter", "vendor/strong"),
                    owner_label: "OpenRouter".into(),
                    model_label: "Strong via OpenRouter".into(),
                },
            ],
        )
    }

    #[test]
    fn picker_keeps_provider_ownership_visible_and_disable_explicit() {
        let view = view();
        assert_eq!(view.visible_rows().len(), 3);
        assert!(view.visible_rows()[0].current);
        assert_eq!(view.visible_rows()[0].owner_key, "native:direct");
        assert!(matches!(
            view.visible_rows()[2].action,
            AdvisorPanelAction::Disable
        ));
        let lines = view.accessible_lines();
        assert!(lines.iter().any(|line| {
            line.contains("1. Direct — Strong") && line.contains("native:direct")
        }));
        assert!(lines.iter().any(|line| line.contains("✔ current")));
        assert!(
            lines
                .iter()
                .all(|line| !line.contains("Descendant") && !line.contains("generation")),
            "technical status belongs to `/advisor status`, not the picker"
        );
    }

    #[test]
    fn disabled_route_is_the_default_selected_numbered_row() {
        let status = AdvisorStatus {
            selection: None,
            generation: 9,
            settings_revision: 4,
            descendant_budget: SubagentBudgetSnapshot {
                limits: SubagentBudgetLimits::default(),
                requests_reserved: 2,
                in_flight: 0,
            },
        };
        let view = AdvisorPanelView::new(
            &status,
            vec![AdvisorRouteChoice {
                selection: selection("openrouter", "vendor/model"),
                owner_label: "OpenRouter".to_owned(),
                model_label: "Vendor Model".to_owned(),
            }],
        );

        assert!(matches!(
            view.selected().map(|row| &row.action),
            Some(AdvisorPanelAction::Disable)
        ));
        assert!(
            view.accessible_lines()
                .iter()
                .any(|line| line.starts_with("› 2. No advisor") && line.contains("✔ current"))
        );
    }

    #[test]
    fn loading_is_honest_and_escape_cancels_without_reopening() {
        let status = AdvisorStatus {
            selection: None,
            generation: 1,
            settings_revision: 0,
            descendant_budget: SubagentBudgetSnapshot {
                limits: SubagentBudgetLimits::default(),
                requests_reserved: 0,
                in_flight: 0,
            },
        };
        let bridge = AdvisorPanelBridge::new();
        let cancellation = CancellationToken::new();
        let mut loading = AdvisorPanelView::loading(&status, cancellation.clone());
        assert!(loading.is_loading());
        assert!(
            loading
                .accessible_lines()
                .iter()
                .any(|line| line.contains("Loading connected provider model catalogs"))
        );
        assert_eq!(
            loading.handle(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            ))),
            AdvisorPanelOutcome::Close
        );
        assert!(cancellation.is_cancelled());

        bridge.request(
            AdvisorPanelView::new(&status, Vec::new()).for_operation(cancellation.clone()),
        );
        assert!(
            bridge.take().is_none(),
            "a completed refresh cannot reopen a picker after Escape won the race"
        );
    }

    #[test]
    fn picker_renders_as_an_unboxed_bottom_surface_at_eighty_by_twenty_four() {
        let status = AdvisorStatus {
            selection: None,
            generation: 1,
            settings_revision: 0,
            descendant_budget: SubagentBudgetSnapshot {
                limits: SubagentBudgetLimits::default(),
                requests_reserved: 0,
                in_flight: 0,
            },
        };
        let view = AdvisorPanelView::new(
            &status,
            vec![AdvisorRouteChoice {
                selection: selection("openrouter", "vendor/model"),
                owner_label: "OpenRouter".to_owned(),
                model_label: "Vendor Model".to_owned(),
            }],
        );
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, &view, frame.area()))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = buffer
            .content
            .chunks(80)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        let heading = rows.iter().position(|row| row.contains("Advisor")).unwrap();
        let rendered = rows.join("\n");

        assert!(heading >= 16, "picker must be bottom-aligned:\n{rendered}");
        assert!(rendered.contains("› 2. No advisor"), "{rendered}");
        assert!(
            rendered.contains("Enter to confirm · Esc to cancel"),
            "{rendered}"
        );
        assert!(
            !rendered.contains('┌')
                && !rendered.contains('┐')
                && !rendered.contains('└')
                && !rendered.contains('┘'),
            "the picker must not render a popup box:\n{rendered}"
        );
    }

    #[test]
    fn filtering_never_collapses_equal_model_names_across_providers() {
        let mut view = view();
        for character in "openrouter".chars() {
            let _ = view.handle(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Char(character),
                KeyModifiers::NONE,
            )));
        }
        let rows = view.visible_rows();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].owner_key, "native:openrouter");
        assert!(matches!(
            view.handle(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            ))),
            AdvisorPanelOutcome::Commit(AdvisorPanelAction::Select(_))
        ));
    }

    #[test]
    fn compact_viewport_keeps_a_deep_selection_on_screen() {
        let status = AdvisorStatus {
            selection: Some(selection("provider", "model-00")),
            generation: 1,
            settings_revision: 0,
            descendant_budget: SubagentBudgetSnapshot {
                limits: SubagentBudgetLimits::default(),
                requests_reserved: 0,
                in_flight: 0,
            },
        };
        let choices = (0..30)
            .map(|index| AdvisorRouteChoice {
                selection: selection("provider", &format!("model-{index:02}")),
                owner_label: "Provider".to_owned(),
                model_label: format!("Model {index:02}"),
            })
            .collect();
        let mut view = AdvisorPanelView::new(&status, choices);
        for _ in 0..28 {
            let _ = view.handle(&Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Down,
                KeyModifiers::NONE,
            )));
        }

        let lines = view.display_lines(20);

        assert_eq!(lines.len(), 20);
        assert!(lines.iter().any(|line| line.starts_with("› ")));
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("› ") && line.contains("model-28"))
        );
        assert!(
            view.accessible_lines().len() > lines.len(),
            "the accessibility projection stays complete while drawing is viewport-bounded"
        );
    }
}
