//! U14 settings browser over the shared schema/form and CAS boundaries.

use std::cell::{Cell, RefCell};
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use heycode_agent::ui::{SettingsShellSection, SettingsShellSnapshot, SettingsShellTab};
use heycode_settings::{SettingsApplies, SettingsNamespace, SettingsService};
use heycode_ui::settings_ui::{FieldOrigin, SettingsField, SettingsSurface, SettingsUiRegistry};
use ratatui::layout::{Position, Rect};

/// One supported editor/value shape in the settings browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SettingsPanelValue {
    /// Boolean value; Enter toggles and commits immediately.
    Toggle(bool),
    /// Free text edited inline.
    Text(String),
    /// JSON number edited without a lossy numeric conversion.
    Number(String),
    /// Closed string choices in schema order.
    Choice {
        /// Allowed values.
        options: Vec<String>,
        /// Current effective value when valid.
        selected: Option<String>,
    },
    /// Secret presence only; no value exists in this view.
    Secret {
        /// Whether a value is configured.
        configured: bool,
    },
    /// Schema construct this build cannot edit.
    Unrenderable {
        /// Stable human explanation from the shared derivation layer.
        reason: String,
    },
    /// Namespace delegated to a plugin-owned custom panel.
    Custom {
        /// Registered panel contribution id.
        panel: String,
    },
}

/// One flattened settings field or custom-panel delegation row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPanelRow {
    namespace: String,
    path: String,
    value: SettingsPanelValue,
    origin: Option<FieldOrigin>,
    revision: u64,
    applies: SettingsApplies,
    editable: bool,
    explanation: Option<String>,
}

impl SettingsPanelRow {
    /// Namespace id.
    #[must_use]
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Dotted field path, empty only for a custom-panel delegation.
    #[must_use]
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Safe value shape.
    #[must_use]
    pub const fn value(&self) -> &SettingsPanelValue {
        &self.value
    }

    /// Effective value origin for derived fields.
    #[must_use]
    pub const fn origin(&self) -> Option<FieldOrigin> {
        self.origin
    }

    /// Whether the browser can commit this row.
    #[must_use]
    pub const fn editable(&self) -> bool {
        self.editable
    }

    /// Visible reason when the row is not editable or needs special handling.
    #[must_use]
    pub fn explanation(&self) -> Option<&str> {
        self.explanation.as_deref()
    }

    /// `live` or `restart` application timing.
    #[must_use]
    pub const fn applies(&self) -> SettingsApplies {
        self.applies
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SettingsEditor {
    row: usize,
    buffer: String,
}

/// Result of one panel key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsPanelKeyOutcome {
    /// Key was handled and the panel remains open.
    Handled,
    /// Close the panel.
    Close,
}

/// Keyboard focus inside the source-mapped settings shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SettingsShellFocus {
    /// Left/Right/Tab switches the four child tabs.
    Tabs,
    /// Config's live filter field.
    Search,
    /// One filtered Config row.
    Rows,
    /// Read-only Status, Usage, or Stats content.
    Content,
}

/// Shared source-mapped shell around live Config and immutable diagnostics.
///
/// Config remains the only mutable child and delegates every write to the
/// existing [`SettingsPanelView`] CAS path. Status, Usage, and Stats never
/// fetch or reconstruct state in the front end.
pub struct SettingsShellView {
    tab: SettingsShellTab,
    focus: SettingsShellFocus,
    query: String,
    config: SettingsPanelView,
    snapshot: SettingsShellSnapshot,
    stats_view: Option<crate::stats_view::StatsView>,
    content_offset: usize,
    content_viewport_rows: Cell<usize>,
    content_total_rows: Cell<usize>,
    tab_hits: RefCell<Vec<(Rect, SettingsShellTab)>>,
    stats_tab_hits: RefCell<Vec<(Rect, crate::stats_view::StatsViewTab)>>,
    search_hit: Cell<Option<Rect>>,
}

impl SettingsShellView {
    /// Open one source-selected child over a single Settings revision view.
    ///
    /// # Errors
    /// Registry/snapshot failures prevent a partial Config child from opening.
    pub fn open(
        settings: Arc<SettingsService>,
        ui: Arc<SettingsUiRegistry>,
        tab: SettingsShellTab,
        snapshot: SettingsShellSnapshot,
    ) -> Result<Self, String> {
        let stats_view = snapshot
            .stats_snapshot()
            .map(|stats| crate::stats_view::StatsView::new(stats, chrono::Utc::now().date_naive()));
        Ok(Self {
            tab,
            focus: if tab == SettingsShellTab::Config {
                SettingsShellFocus::Search
            } else {
                SettingsShellFocus::Tabs
            },
            query: String::new(),
            config: SettingsPanelView::open(settings, ui)?,
            snapshot,
            stats_view,
            content_offset: 0,
            content_viewport_rows: Cell::new(0),
            content_total_rows: Cell::new(0),
            tab_hits: RefCell::new(Vec::new()),
            stats_tab_hits: RefCell::new(Vec::new()),
            search_hit: Cell::new(None),
        })
    }

    /// Active child.
    #[must_use]
    pub const fn tab(&self) -> SettingsShellTab {
        self.tab
    }

    /// Current keyboard focus owner.
    #[must_use]
    pub const fn focus(&self) -> SettingsShellFocus {
        self.focus
    }

    /// Current Config query. It is always empty outside Config.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Live Config child.
    #[must_use]
    pub const fn config(&self) -> &SettingsPanelView {
        &self.config
    }

    /// Mutable Config child for the owning event loop.
    pub const fn config_mut(&mut self) -> &mut SettingsPanelView {
        &mut self.config
    }

    /// Immutable request snapshot for non-Config children.
    #[must_use]
    pub const fn snapshot(&self) -> &SettingsShellSnapshot {
        &self.snapshot
    }

    /// Interactive source-shaped Stats projection, when typed facts exist.
    #[must_use]
    pub const fn stats_view(&self) -> Option<&crate::stats_view::StatsView> {
        self.stats_view.as_ref()
    }

    /// First non-Config content line to render.
    #[must_use]
    pub const fn content_offset(&self) -> usize {
        self.content_offset
    }

    /// Publish the current frame's wrapped non-Config layout for bounded scroll.
    pub(crate) fn set_content_layout_rows(&self, viewport_rows: usize, total_rows: usize) {
        self.content_viewport_rows.set(viewport_rows);
        self.content_total_rows.set(total_rows);
    }

    /// Active non-Config section; Config returns `None`.
    #[must_use]
    pub const fn section(&self) -> Option<&SettingsShellSection> {
        self.snapshot.section(self.tab)
    }

    /// Config rows matching namespace, path, visible value, or explanation.
    /// Returned indices are the stable indices expected by mouse hit regions.
    #[must_use]
    pub fn filtered_rows(&self) -> Vec<(usize, &SettingsPanelRow)> {
        let query = self.query.to_lowercase();
        self.config
            .rows()
            .iter()
            .enumerate()
            .filter(|(_, row)| query.is_empty() || row_search_text(row).contains(&query))
            .collect()
    }

    /// Source footer for the current focus state.
    #[must_use]
    pub fn footer(&self) -> &'static str {
        if self.config.edit_buffer().is_some() && self.tab == SettingsShellTab::Config {
            return "Enter to save · Esc to cancel";
        }
        if self.focus == SettingsShellFocus::Content
            && self.tab == SettingsShellTab::Stats
            && let Some(stats) = self.stats_view.as_ref()
        {
            return match stats.tab() {
                crate::stats_view::StatsViewTab::Overview => {
                    "←/→ Overview/Models · r cycle dates · ↑ to tabs · Esc to close"
                }
                crate::stats_view::StatsViewTab::Models => {
                    "←/→ Overview/Models · ↑ to tabs · Esc to close"
                }
            };
        }
        match self.focus {
            SettingsShellFocus::Tabs => "←/→/tab to switch · ↓ to return · Esc to close",
            SettingsShellFocus::Search => {
                "Type to filter · Enter/↓ to select · ↑ to tabs · Esc to clear"
            }
            SettingsShellFocus::Rows => "Enter/Space to change · / to search · Esc to close",
            SettingsShellFocus::Content => "↑ to tabs · Esc to close",
        }
    }

    /// Publish only current-frame header and search hit regions.
    pub(crate) fn set_shell_mouse_regions(
        &self,
        tabs: Vec<(Rect, SettingsShellTab)>,
        search: Option<Rect>,
    ) {
        *self.tab_hits.borrow_mut() = tabs;
        self.search_hit.set(search);
    }

    /// Publish current-frame hit regions for Overview and Models.
    pub(crate) fn set_stats_mouse_regions(
        &self,
        tabs: Vec<(Rect, crate::stats_view::StatsViewTab)>,
    ) {
        *self.stats_tab_hits.borrow_mut() = tabs;
    }

    /// Mouse changes selection/focus only. Settings still require Enter/Space.
    pub(crate) fn handle_mouse(&mut self, event: MouseEvent) {
        if self.tab != SettingsShellTab::Config
            && matches!(
                event.kind,
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
            )
        {
            self.focus = SettingsShellFocus::Content;
            match event.kind {
                MouseEventKind::ScrollUp => self.scroll_content(-3),
                MouseEventKind::ScrollDown => self.scroll_content(3),
                _ => {}
            }
            return;
        }
        if !matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {
            if self.tab == SettingsShellTab::Config
                && matches!(
                    event.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                )
                && self.config.edit_buffer().is_none()
            {
                self.focus = SettingsShellFocus::Rows;
                self.config.handle_mouse(event);
                self.ensure_visible_selection();
            }
            return;
        }
        let point = Position::new(event.column, event.row);
        let selected_tab = self
            .tab_hits
            .borrow()
            .iter()
            .find_map(|(area, tab)| area.contains(point).then_some(*tab));
        if let Some(tab) = selected_tab {
            self.select_tab(tab);
            self.focus = SettingsShellFocus::Tabs;
            return;
        }
        if self.tab == SettingsShellTab::Stats
            && let Some(tab) = self
                .stats_tab_hits
                .borrow()
                .iter()
                .find_map(|(area, tab)| area.contains(point).then_some(*tab))
        {
            if let Some(stats) = self.stats_view.as_mut() {
                stats.select_tab(tab);
            }
            self.content_offset = 0;
            self.focus = SettingsShellFocus::Content;
            return;
        }
        if self.tab != SettingsShellTab::Config || self.config.edit_buffer().is_some() {
            return;
        }
        if self
            .search_hit
            .get()
            .is_some_and(|area| area.contains(point))
        {
            self.focus = SettingsShellFocus::Search;
            return;
        }
        let before = self.config.selected();
        self.config.handle_mouse(event);
        if self.config.selected() != before
            || self
                .config
                .mouse_rows
                .borrow()
                .iter()
                .any(|(area, _)| area.contains(point))
        {
            self.focus = SettingsShellFocus::Rows;
        }
    }

    /// Handle one source-mapped shell key without performing a network read.
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SettingsPanelKeyOutcome {
        if self.tab == SettingsShellTab::Config && self.config.edit_buffer().is_some() {
            return self.config.handle_key(code, modifiers);
        }
        match self.focus {
            SettingsShellFocus::Tabs => self.handle_tab_key(code, modifiers),
            SettingsShellFocus::Search => self.handle_search_key(code, modifiers),
            SettingsShellFocus::Rows => self.handle_row_key(code, modifiers),
            SettingsShellFocus::Content => match code {
                KeyCode::Left | KeyCode::Right
                    if self.tab == SettingsShellTab::Stats && self.stats_view.is_some() =>
                {
                    if let Some(stats) = self.stats_view.as_mut() {
                        stats.toggle_tab();
                    }
                    self.content_offset = 0;
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::Char('r')
                    if modifiers.is_empty()
                        && self.tab == SettingsShellTab::Stats
                        && self.stats_view.as_ref().is_some_and(|stats| {
                            stats.tab() == crate::stats_view::StatsViewTab::Overview
                        }) =>
                {
                    if let Some(stats) = self.stats_view.as_mut() {
                        stats.cycle_range();
                    }
                    self.content_offset = 0;
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::Up => {
                    if self.content_offset == 0 {
                        self.focus = SettingsShellFocus::Tabs;
                    } else {
                        self.scroll_content(-1);
                    }
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::Down => {
                    self.scroll_content(1);
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::PageUp => {
                    self.scroll_content(-10);
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::PageDown => {
                    self.scroll_content(10);
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::Home => {
                    self.content_offset = 0;
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::End => {
                    self.content_offset = self.maximum_content_offset();
                    SettingsPanelKeyOutcome::Handled
                }
                KeyCode::Esc => SettingsPanelKeyOutcome::Close,
                _ => SettingsPanelKeyOutcome::Handled,
            },
        }
    }

    /// Route paste only to Config search or an explicit text/number editor.
    /// Newlines and control characters remain inert search text; they never
    /// reach the composer or execute a command.
    pub fn paste(&mut self, text: &str) {
        if self.tab != SettingsShellTab::Config {
            return;
        }
        if self.config.edit_buffer().is_some() {
            self.config.paste(text);
            return;
        }
        if self.focus != SettingsShellFocus::Search {
            return;
        }
        for character in text.chars() {
            let character = if character.is_control() {
                ' '
            } else {
                character
            };
            if !push_bounded(&mut self.query, character, 4_096) {
                break;
            }
        }
        self.select_first_match();
    }

    fn handle_tab_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SettingsPanelKeyOutcome {
        match code {
            KeyCode::Left | KeyCode::BackTab => self.select_tab(self.tab.previous()),
            KeyCode::Right => self.select_tab(self.tab.next()),
            KeyCode::Tab if modifiers.contains(KeyModifiers::SHIFT) => {
                self.select_tab(self.tab.previous());
            }
            KeyCode::Tab => self.select_tab(self.tab.next()),
            KeyCode::Down | KeyCode::Enter => {
                self.focus = if self.tab == SettingsShellTab::Config {
                    SettingsShellFocus::Search
                } else {
                    SettingsShellFocus::Content
                };
            }
            KeyCode::Esc => return SettingsPanelKeyOutcome::Close,
            _ => {}
        }
        SettingsPanelKeyOutcome::Handled
    }

    fn handle_search_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SettingsPanelKeyOutcome {
        match code {
            KeyCode::Up => self.focus = SettingsShellFocus::Tabs,
            KeyCode::Down | KeyCode::Enter => self.focus_first_match(),
            KeyCode::Backspace => {
                self.query.pop();
                self.select_first_match();
            }
            KeyCode::Esc if self.query.is_empty() => self.focus_first_match(),
            KeyCode::Esc => {
                self.query.clear();
                self.select_first_match();
            }
            KeyCode::Char(character)
                if !modifiers.intersects(
                    KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER,
                ) =>
            {
                let _ = push_bounded(&mut self.query, character, 4_096);
                self.select_first_match();
            }
            _ => {}
        }
        SettingsPanelKeyOutcome::Handled
    }

    fn handle_row_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SettingsPanelKeyOutcome {
        match code {
            KeyCode::Char('/') if modifiers.is_empty() => {
                self.query.clear();
                self.focus = SettingsShellFocus::Search;
            }
            KeyCode::Up => self.move_filtered(-1),
            KeyCode::Down => self.move_filtered(1),
            KeyCode::Enter | KeyCode::Char(' ') if modifiers.is_empty() => {
                let outcome = self.config.handle_key(KeyCode::Enter, KeyModifiers::NONE);
                self.ensure_visible_selection();
                return outcome;
            }
            KeyCode::Esc => return SettingsPanelKeyOutcome::Close,
            _ => {}
        }
        SettingsPanelKeyOutcome::Handled
    }

    fn select_tab(&mut self, tab: SettingsShellTab) {
        self.tab = tab;
        self.query.clear();
        self.content_offset = 0;
    }

    fn matching_indices(&self) -> Vec<usize> {
        self.filtered_rows()
            .into_iter()
            .map(|(index, _)| index)
            .collect()
    }

    fn select_first_match(&mut self) {
        if let Some(first) = self.matching_indices().first().copied() {
            self.config.selected = first;
        }
    }

    fn focus_first_match(&mut self) {
        if let Some(first) = self.matching_indices().first().copied() {
            self.config.selected = first;
            self.focus = SettingsShellFocus::Rows;
        }
    }

    fn move_filtered(&mut self, delta: isize) {
        let indices = self.matching_indices();
        let Some(position) = indices
            .iter()
            .position(|index| *index == self.config.selected)
        else {
            self.focus_first_match();
            return;
        };
        if delta.is_negative() {
            if position == 0 {
                self.focus = SettingsShellFocus::Search;
            } else {
                self.config.selected = indices[position - 1];
            }
        } else if position + 1 < indices.len() {
            self.config.selected = indices[position + 1];
        }
    }

    fn ensure_visible_selection(&mut self) {
        let indices = self.matching_indices();
        if !indices.contains(&self.config.selected)
            && let Some(first) = indices.first().copied()
        {
            self.config.selected = first;
        }
    }

    fn maximum_content_offset(&self) -> usize {
        self.content_total_rows
            .get()
            .saturating_sub(self.content_viewport_rows.get())
    }

    fn scroll_content(&mut self, delta: isize) {
        self.content_offset = if delta.is_negative() {
            self.content_offset.saturating_sub(delta.unsigned_abs())
        } else {
            self.content_offset
                .saturating_add(delta.unsigned_abs())
                .min(self.maximum_content_offset())
        };
    }
}

/// Live settings browser. Commits always go through `SettingsService` CAS.
pub struct SettingsPanelView {
    settings: Arc<SettingsService>,
    ui: Arc<SettingsUiRegistry>,
    rows: Vec<SettingsPanelRow>,
    selected: usize,
    editor: Option<SettingsEditor>,
    notice: Option<String>,
    mouse_rows: std::cell::RefCell<Vec<(ratatui::layout::Rect, usize)>>,
}

impl SettingsPanelView {
    /// Read every registered namespace and build its derived/custom surface.
    ///
    /// # Errors
    /// Registry/snapshot failures are safe strings and open no partial panel.
    pub fn open(
        settings: Arc<SettingsService>,
        ui: Arc<SettingsUiRegistry>,
    ) -> Result<Self, String> {
        let rows = build_rows(&settings, &ui)?;
        Ok(Self {
            settings,
            ui,
            rows,
            selected: 0,
            editor: None,
            notice: None,
            mouse_rows: std::cell::RefCell::new(Vec::new()),
        })
    }

    /// Current flattened rows.
    #[must_use]
    pub fn rows(&self) -> &[SettingsPanelRow] {
        &self.rows
    }

    /// Selected row index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Inline edit buffer, when text/number editing is active.
    #[must_use]
    pub fn edit_buffer(&self) -> Option<&str> {
        self.editor.as_ref().map(|editor| editor.buffer.as_str())
    }

    /// Latest safe commit/refusal notice.
    #[must_use]
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Publish only the current frame's visible row hit areas.
    pub(crate) fn set_mouse_rows(&self, rows: Vec<(ratatui::layout::Rect, usize)>) {
        *self.mouse_rows.borrow_mut() = rows;
    }

    /// Mouse selection never commits a setting; Enter remains the explicit action.
    pub(crate) fn handle_mouse(&mut self, event: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        if self.editor.is_some() {
            return;
        }
        match event.kind {
            MouseEventKind::ScrollUp => {
                let _ = self.handle_key(KeyCode::Up, KeyModifiers::NONE);
            }
            MouseEventKind::ScrollDown => {
                let _ = self.handle_key(KeyCode::Down, KeyModifiers::NONE);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let point = ratatui::layout::Position::new(event.column, event.row);
                if let Some((_, index)) = self
                    .mouse_rows
                    .borrow()
                    .iter()
                    .find(|(area, index)| *index < self.rows.len() && area.contains(point))
                {
                    self.selected = *index;
                }
            }
            _ => {}
        }
    }

    /// Handle one terminal key, including CAS commits.
    pub fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SettingsPanelKeyOutcome {
        if self.editor.is_some() {
            return self.handle_editor_key(code, modifiers);
        }
        match code {
            KeyCode::Esc => SettingsPanelKeyOutcome::Close,
            KeyCode::Up => {
                if !self.rows.is_empty() {
                    self.selected = self
                        .selected
                        .checked_sub(1)
                        .unwrap_or(self.rows.len().saturating_sub(1));
                }
                SettingsPanelKeyOutcome::Handled
            }
            KeyCode::Down => {
                if !self.rows.is_empty() {
                    self.selected = (self.selected + 1) % self.rows.len();
                }
                SettingsPanelKeyOutcome::Handled
            }
            KeyCode::Enter => {
                self.activate_selected();
                SettingsPanelKeyOutcome::Handled
            }
            _ => SettingsPanelKeyOutcome::Handled,
        }
    }

    /// Append pasted text only while editing a text/number value.
    pub fn paste(&mut self, text: &str) {
        let Some(editor) = self.editor.as_mut() else {
            return;
        };
        let remaining = 4_096_usize.saturating_sub(editor.buffer.len());
        editor.buffer.extend(text.chars().take(remaining));
    }

    fn activate_selected(&mut self) {
        let Some(row) = self.rows.get(self.selected).cloned() else {
            self.notice = Some("no registered settings namespaces".to_owned());
            return;
        };
        if !row.editable {
            self.notice = Some(
                row.explanation
                    .unwrap_or_else(|| "this setting is read-only".to_owned()),
            );
            return;
        }
        match &row.value {
            SettingsPanelValue::Toggle(value) => {
                self.commit(&row, serde_json::Value::Bool(!value));
            }
            SettingsPanelValue::Choice { options, selected } => {
                if options.is_empty() {
                    self.notice = Some("this choice has no allowed values".to_owned());
                    return;
                }
                let next = selected
                    .as_ref()
                    .and_then(|current| options.iter().position(|value| value == current))
                    .map_or(0, |index| (index + 1) % options.len());
                self.commit(&row, serde_json::Value::String(options[next].clone()));
            }
            SettingsPanelValue::Text(value) | SettingsPanelValue::Number(value) => {
                self.editor = Some(SettingsEditor {
                    row: self.selected,
                    buffer: value.clone(),
                });
                self.notice = None;
            }
            SettingsPanelValue::Secret { .. }
            | SettingsPanelValue::Unrenderable { .. }
            | SettingsPanelValue::Custom { .. } => {
                self.notice = Some("this row is not editable in the derived browser".to_owned());
            }
        }
    }

    fn handle_editor_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SettingsPanelKeyOutcome {
        match code {
            KeyCode::Esc => {
                self.editor = None;
            }
            KeyCode::Backspace => {
                if let Some(editor) = self.editor.as_mut() {
                    editor.buffer.pop();
                }
            }
            KeyCode::Char(character) if !modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(editor) = self.editor.as_mut()
                    && editor.buffer.len() < 4_096
                {
                    editor.buffer.push(character);
                }
            }
            KeyCode::Enter => {
                let Some(editor) = self.editor.take() else {
                    return SettingsPanelKeyOutcome::Handled;
                };
                let Some(row) = self.rows.get(editor.row).cloned() else {
                    self.notice = Some("the edited setting disappeared".to_owned());
                    return SettingsPanelKeyOutcome::Handled;
                };
                let value = match row.value {
                    SettingsPanelValue::Text(_) => serde_json::Value::String(editor.buffer),
                    SettingsPanelValue::Number(_) => {
                        match serde_json::from_str::<serde_json::Value>(&editor.buffer) {
                            Ok(value) if value.is_number() => value,
                            _ => {
                                self.notice = Some("enter one valid JSON number".to_owned());
                                return SettingsPanelKeyOutcome::Handled;
                            }
                        }
                    }
                    _ => {
                        self.notice = Some("the edited setting changed type".to_owned());
                        return SettingsPanelKeyOutcome::Handled;
                    }
                };
                self.commit(&row, value);
            }
            _ => {}
        }
        SettingsPanelKeyOutcome::Handled
    }

    fn commit(&mut self, row: &SettingsPanelRow, value: serde_json::Value) {
        let namespace = match SettingsNamespace::new(&row.namespace) {
            Ok(namespace) => namespace,
            Err(_) => {
                self.notice = Some("settings namespace is invalid".to_owned());
                return;
            }
        };
        let snapshot = match self.settings.get(&namespace) {
            Ok(Some(snapshot)) => snapshot,
            Ok(None) | Err(_) => {
                self.notice = Some("settings namespace is unavailable".to_owned());
                return;
            }
        };
        let mut user = snapshot
            .user()
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        if set_path(&mut user, &row.path, value).is_err() {
            self.notice = Some("settings path cannot be updated".to_owned());
            return;
        }
        match self
            .settings
            .replace_user(&namespace, user, Some(row.revision))
        {
            Ok(committed) => {
                self.notice = Some(match committed.applies() {
                    SettingsApplies::Live => "saved and applied".to_owned(),
                    SettingsApplies::Restart => "saved; applies after restart".to_owned(),
                });
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        self.reload();
    }

    fn reload(&mut self) {
        match build_rows(&self.settings, &self.ui) {
            Ok(rows) => {
                self.rows = rows;
                self.selected = self.selected.min(self.rows.len().saturating_sub(1));
            }
            Err(error) => self.notice = Some(error),
        }
    }
}

fn push_bounded(value: &mut String, character: char, maximum_bytes: usize) -> bool {
    if value
        .len()
        .saturating_add(character.len_utf8())
        .gt(&maximum_bytes)
    {
        false
    } else {
        value.push(character);
        true
    }
}

fn row_search_text(row: &SettingsPanelRow) -> String {
    let value = match &row.value {
        SettingsPanelValue::Toggle(value) => value.to_string(),
        SettingsPanelValue::Text(value) | SettingsPanelValue::Number(value) => value.clone(),
        SettingsPanelValue::Choice { selected, .. } => {
            selected.as_deref().unwrap_or("unselected").to_owned()
        }
        SettingsPanelValue::Secret { configured } => if *configured {
            "configured"
        } else {
            "not configured"
        }
        .to_owned(),
        SettingsPanelValue::Unrenderable { reason } => reason.clone(),
        SettingsPanelValue::Custom { panel } => panel.clone(),
    };
    format!(
        "{} {} {} {}",
        row.namespace,
        row.path,
        value,
        row.explanation.as_deref().unwrap_or_default()
    )
    .to_lowercase()
}

fn build_rows(
    settings: &SettingsService,
    ui: &SettingsUiRegistry,
) -> Result<Vec<SettingsPanelRow>, String> {
    let mut rows = Vec::new();
    for snapshot in settings.describe().map_err(|error| error.to_string())? {
        match ui.surface(&snapshot).map_err(|error| error.to_string())? {
            SettingsSurface::Custom(panel) => rows.push(SettingsPanelRow {
                namespace: snapshot.namespace().as_str().to_owned(),
                path: String::new(),
                value: SettingsPanelValue::Custom {
                    panel: panel.as_str().to_owned(),
                },
                origin: None,
                revision: snapshot.revision(),
                applies: snapshot.applies(),
                editable: false,
                explanation: Some(format!(
                    "custom panel `{}` owns this namespace",
                    panel.as_str()
                )),
            }),
            SettingsSurface::Derived(form) => {
                rows.extend(form.fields().iter().map(|field| row(&snapshot, field)));
            }
            _ => return Err("this build cannot render the settings surface".to_owned()),
        }
    }
    Ok(rows)
}

fn row(snapshot: &heycode_settings::SettingsSnapshot, field: &SettingsField) -> SettingsPanelRow {
    let (value, origin) = match field {
        SettingsField::Toggle { value, origin, .. } => {
            (SettingsPanelValue::Toggle(*value), Some(*origin))
        }
        SettingsField::Text { value, origin, .. } => {
            (SettingsPanelValue::Text(value.clone()), Some(*origin))
        }
        SettingsField::Number { value, origin, .. } => {
            (SettingsPanelValue::Number(value.clone()), Some(*origin))
        }
        SettingsField::Choice {
            options,
            selected,
            origin,
            ..
        } => (
            SettingsPanelValue::Choice {
                options: options.clone(),
                selected: selected.clone(),
            },
            Some(*origin),
        ),
        SettingsField::Secret {
            configured, origin, ..
        } => (
            SettingsPanelValue::Secret {
                configured: *configured,
            },
            Some(*origin),
        ),
        SettingsField::Unrenderable { reason, .. } => (
            SettingsPanelValue::Unrenderable {
                reason: reason.to_string(),
            },
            None,
        ),
        _ => (
            SettingsPanelValue::Unrenderable {
                reason: "this build cannot render the settings field".to_owned(),
            },
            None,
        ),
    };
    let project_owned = origin == Some(FieldOrigin::Project);
    let editable = field.editable()
        && !project_owned
        && !matches!(
            value,
            SettingsPanelValue::Secret { .. } | SettingsPanelValue::Unrenderable { .. }
        );
    let explanation = if project_owned {
        Some("project layer has higher precedence; edit the trusted project settings".to_owned())
    } else if origin == Some(FieldOrigin::Managed) {
        Some("managed policy locks this field".to_owned())
    } else {
        match &value {
            SettingsPanelValue::Secret { .. } => {
                Some("credential values are never exposed to this browser".to_owned())
            }
            SettingsPanelValue::Unrenderable { reason } => Some(reason.clone()),
            _ => None,
        }
    };
    SettingsPanelRow {
        namespace: snapshot.namespace().as_str().to_owned(),
        path: field.path().to_owned(),
        value,
        origin,
        revision: snapshot.revision(),
        applies: snapshot.applies(),
        editable,
        explanation,
    }
}

fn set_path(root: &mut serde_json::Value, path: &str, value: serde_json::Value) -> Result<(), ()> {
    let mut steps = path.split('.').peekable();
    let mut current = root;
    while let Some(step) = steps.next() {
        if steps.peek().is_none() {
            let object = current.as_object_mut().ok_or(())?;
            object.insert(step.to_owned(), value);
            return Ok(());
        }
        let object = current.as_object_mut().ok_or(())?;
        current = object
            .entry(step.to_owned())
            .or_insert_with(|| serde_json::json!({}));
    }
    Err(())
}

#[cfg(test)]
mod mouse_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;
    use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
    use heycode_settings::{SettingsDefinition, SettingsDocuments, SettingsSchema};
    use ratatui::layout::Rect;
    use serde_json::json;

    struct SuccessWriter;

    impl heycode_settings::SettingsWriter for SuccessWriter {
        fn persist_user(
            &self,
            _namespace: &SettingsNamespace,
            _section: &serde_json::Value,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn shell_fixture() -> Result<
        (
            SettingsShellView,
            Arc<SettingsService>,
            SettingsNamespace,
            heycode_core::Context,
        ),
        String,
    > {
        let settings = Arc::new(SettingsService::with_writer(
            SettingsDocuments::new(),
            Arc::new(SuccessWriter),
        ));
        let namespace = SettingsNamespace::new("shell").map_err(|error| error.to_string())?;
        let schema = SettingsSchema::new(
            json!({
                "type":"object",
                "properties":{
                    "enabled":{"type":"boolean"},
                    "label":{"type":"string"}
                }
            }),
            json!({"enabled":false,"label":"demo"}),
            |_| Ok(()),
        )
        .map_err(|error| error.to_string())?;
        let context = heycode_core::Context::new();
        settings
            .register(&context, SettingsDefinition::new(namespace.clone(), schema))
            .map_err(|error| error.to_string())?;
        let snapshot = SettingsShellSnapshot {
            status: SettingsShellSection::Ready {
                text: "status\nruntime: native".to_owned(),
            },
            usage: SettingsShellSection::Ready {
                text: "usage\nturns: 0".to_owned(),
            },
            stats: SettingsShellSection::Unavailable {
                reason: "stats unavailable: no cross-session aggregate owner".to_owned(),
            },
            stats_snapshot: None,
        };
        let view = SettingsShellView::open(
            settings.clone(),
            Arc::new(SettingsUiRegistry::new()),
            SettingsShellTab::Config,
            snapshot,
        )?;
        Ok((view, settings, namespace, context))
    }

    fn press(view: &mut SettingsShellView, code: KeyCode) -> SettingsPanelKeyOutcome {
        view.handle_key(code, KeyModifiers::NONE)
    }

    fn type_text(view: &mut SettingsShellView, value: &str) {
        for character in value.chars() {
            assert_eq!(
                press(view, KeyCode::Char(character)),
                SettingsPanelKeyOutcome::Handled
            );
        }
    }

    #[test]
    fn shell_matches_source_search_rows_and_tab_focus_transitions() -> Result<(), String> {
        let (mut view, _, _, _context) = shell_fixture()?;
        assert_eq!(view.tab(), SettingsShellTab::Config);
        assert_eq!(view.focus(), SettingsShellFocus::Search);
        assert_eq!(
            view.footer(),
            "Type to filter · Enter/↓ to select · ↑ to tabs · Esc to clear"
        );

        type_text(&mut view, "label");
        assert_eq!(view.filtered_rows().len(), 1);
        press(&mut view, KeyCode::Down);
        assert_eq!(view.focus(), SettingsShellFocus::Rows);
        assert_eq!(
            view.footer(),
            "Enter/Space to change · / to search · Esc to close"
        );
        press(&mut view, KeyCode::Char('/'));
        assert_eq!(view.focus(), SettingsShellFocus::Search);
        assert!(view.query().is_empty());
        press(&mut view, KeyCode::Esc);
        assert_eq!(view.focus(), SettingsShellFocus::Rows);
        press(&mut view, KeyCode::Up);
        assert_eq!(view.focus(), SettingsShellFocus::Search);
        press(&mut view, KeyCode::Up);
        assert_eq!(view.focus(), SettingsShellFocus::Tabs);
        assert_eq!(
            view.footer(),
            "←/→/tab to switch · ↓ to return · Esc to close"
        );

        for expected in [
            SettingsShellTab::Usage,
            SettingsShellTab::Stats,
            SettingsShellTab::Status,
            SettingsShellTab::Config,
        ] {
            press(&mut view, KeyCode::Right);
            assert_eq!(view.tab(), expected);
            assert_eq!(view.focus(), SettingsShellFocus::Tabs);
        }
        Ok(())
    }

    #[test]
    fn shell_search_paste_is_inert_and_space_uses_existing_cas_commit() -> Result<(), String> {
        let (mut view, settings, namespace, _context) = shell_fixture()?;
        view.paste("/status\nprivate");
        assert_eq!(view.query(), "/status private");
        assert!(view.filtered_rows().is_empty());
        let before = settings
            .get(&namespace)
            .map_err(|error| error.to_string())?
            .ok_or("missing settings")?;
        assert_eq!(before.revision(), 0);

        while !view.query().is_empty() {
            press(&mut view, KeyCode::Backspace);
        }
        type_text(&mut view, "enabled");
        press(&mut view, KeyCode::Down);
        press(&mut view, KeyCode::Char(' '));
        let after = settings
            .get(&namespace)
            .map_err(|error| error.to_string())?
            .ok_or("missing settings")?;
        assert_eq!(after.revision(), 1);
        assert_eq!(after.resolved()["enabled"], true);
        assert_eq!(view.config().notice(), Some("saved and applied"));
        Ok(())
    }

    #[test]
    fn shell_mouse_selects_tabs_search_and_rows_without_committing() -> Result<(), String> {
        let (mut view, settings, namespace, _context) = shell_fixture()?;
        let event = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        view.set_shell_mouse_regions(
            vec![
                (Rect::new(2, 1, 6, 1), SettingsShellTab::Status),
                (Rect::new(10, 1, 6, 1), SettingsShellTab::Config),
            ],
            Some(Rect::new(2, 3, 30, 1)),
        );
        view.config()
            .set_mouse_rows(vec![(Rect::new(2, 5, 30, 1), 1)]);
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 3, 1));
        assert_eq!(view.tab(), SettingsShellTab::Status);
        assert_eq!(view.focus(), SettingsShellFocus::Tabs);
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 11, 1));
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 3, 3));
        assert_eq!(view.focus(), SettingsShellFocus::Search);
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 3, 5));
        assert_eq!(view.focus(), SettingsShellFocus::Rows);
        assert_eq!(view.config().selected(), 1);
        let snapshot = settings
            .get(&namespace)
            .map_err(|error| error.to_string())?
            .ok_or("missing settings")?;
        assert_eq!(snapshot.revision(), 0);
        assert_eq!(snapshot.resolved()["enabled"], false);
        Ok(())
    }

    #[test]
    fn unavailable_stats_remain_distinct_from_empty_usage() -> Result<(), String> {
        let (mut view, _, _, _context) = shell_fixture()?;
        press(&mut view, KeyCode::Up);
        press(&mut view, KeyCode::Right);
        assert_eq!(view.tab(), SettingsShellTab::Usage);
        assert!(matches!(
            view.section(),
            Some(SettingsShellSection::Ready { .. })
        ));
        press(&mut view, KeyCode::Right);
        assert_eq!(view.tab(), SettingsShellTab::Stats);
        assert!(matches!(
            view.section(),
            Some(SettingsShellSection::Unavailable { .. })
        ));
        assert!(!view.section().unwrap().plain_text().contains("turns: 0"));
        Ok(())
    }

    #[test]
    fn typed_stats_child_switches_views_ranges_and_mouse_focus() -> Result<(), String> {
        let (mut view, _, _, _context) = shell_fixture()?;
        view.stats_view = Some(crate::stats_view::StatsView::new(
            &heycode_session::SessionStatsSnapshot::default(),
            chrono::NaiveDate::from_ymd_opt(2026, 9, 12).unwrap(),
        ));
        press(&mut view, KeyCode::Up);
        press(&mut view, KeyCode::Right);
        press(&mut view, KeyCode::Right);
        assert_eq!(view.tab(), SettingsShellTab::Stats);
        press(&mut view, KeyCode::Down);
        assert_eq!(view.focus(), SettingsShellFocus::Content);
        assert_eq!(
            view.stats_view().unwrap().tab(),
            crate::stats_view::StatsViewTab::Overview
        );
        assert_eq!(
            view.stats_view().unwrap().range(),
            crate::stats_view::StatsDateRange::AllTime
        );
        press(&mut view, KeyCode::Char('r'));
        assert_eq!(
            view.stats_view().unwrap().range(),
            crate::stats_view::StatsDateRange::LastSevenDays
        );
        press(&mut view, KeyCode::Right);
        assert_eq!(
            view.stats_view().unwrap().tab(),
            crate::stats_view::StatsViewTab::Models
        );
        assert_eq!(
            view.footer(),
            "←/→ Overview/Models · ↑ to tabs · Esc to close"
        );

        view.set_stats_mouse_regions(vec![(
            Rect::new(4, 5, 10, 1),
            crate::stats_view::StatsViewTab::Overview,
        )]);
        view.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(
            view.stats_view().unwrap().tab(),
            crate::stats_view::StatsViewTab::Overview
        );
        assert_eq!(view.content_offset(), 0);
        assert_eq!(view.focus(), SettingsShellFocus::Content);
        Ok(())
    }

    #[test]
    fn non_config_content_scroll_is_viewport_bounded_and_returns_to_tabs() -> Result<(), String> {
        let (mut view, _, _, _context) = shell_fixture()?;
        view.snapshot.status = SettingsShellSection::Ready {
            text: (0..30)
                .map(|index| format!("status line {index}"))
                .collect::<Vec<_>>()
                .join("\n"),
        };
        press(&mut view, KeyCode::Up);
        press(&mut view, KeyCode::Left);
        assert_eq!(view.tab(), SettingsShellTab::Status);
        view.set_content_layout_rows(4, 30);
        press(&mut view, KeyCode::Down);
        assert_eq!(view.focus(), SettingsShellFocus::Content);
        press(&mut view, KeyCode::PageDown);
        assert_eq!(view.content_offset(), 10);
        press(&mut view, KeyCode::End);
        assert_eq!(view.content_offset(), 26);
        press(&mut view, KeyCode::Down);
        assert_eq!(view.content_offset(), 26);
        press(&mut view, KeyCode::Up);
        assert_eq!(view.content_offset(), 25);
        press(&mut view, KeyCode::Home);
        assert_eq!(view.content_offset(), 0);
        press(&mut view, KeyCode::Up);
        assert_eq!(view.focus(), SettingsShellFocus::Tabs);
        Ok(())
    }

    #[test]
    fn mouse_selection_preserves_settings_and_active_editor() -> Result<(), String> {
        let settings = Arc::new(SettingsService::new(SettingsDocuments::new()));
        let namespace = SettingsNamespace::new("mouse").map_err(|e| e.to_string())?;
        let schema = SettingsSchema::new(
            json!({"type":"object","properties":{"enabled":{"type":"boolean"},"label":{"type":"string"}}}),
            json!({"enabled":false,"label":"demo"}),
            |_| Ok(()),
        ).map_err(|e| e.to_string())?;
        let context = heycode_core::Context::new();
        settings
            .register(&context, SettingsDefinition::new(namespace.clone(), schema))
            .map_err(|e| e.to_string())?;
        let mut view =
            SettingsPanelView::open(settings.clone(), Arc::new(SettingsUiRegistry::new()))?;
        view.set_mouse_rows(vec![
            (Rect::new(2, 4, 20, 1), 0),
            (Rect::new(2, 5, 20, 1), 1),
        ]);
        let event = |kind, column, row| MouseEvent {
            kind,
            column,
            row,
            modifiers: KeyModifiers::NONE,
        };
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 3, 5));
        assert_eq!(view.selected(), 1);
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 40, 4));
        assert_eq!(view.selected(), 1);
        view.handle_mouse(event(MouseEventKind::ScrollUp, 3, 5));
        assert_eq!(view.selected(), 0);
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 3, 4));
        let snapshot = settings
            .get(&namespace)
            .map_err(|e| e.to_string())?
            .ok_or("missing settings")?;
        assert_eq!(snapshot.revision(), 0);
        assert_eq!(snapshot.resolved()["enabled"], false);
        view.handle_mouse(event(MouseEventKind::ScrollDown, 3, 5));
        view.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        let original = view.edit_buffer().map(str::to_owned);
        assert_eq!(original.as_deref(), Some("demo"));
        view.handle_mouse(event(MouseEventKind::ScrollUp, 3, 5));
        view.handle_mouse(event(MouseEventKind::Down(MouseButton::Left), 3, 4));
        assert_eq!(view.selected(), 1);
        assert_eq!(view.edit_buffer(), original.as_deref());
        Ok(())
    }
}
