//! Interactive skills catalog state, separate from the read-only capability
//! views used by agents and hooks.

use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{
    Frame,
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
};

const MAX_SEARCH_CHARS: usize = 128;

/// Result of one keyboard action owned by the skills modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillsPanelAction {
    None,
    Close,
}

/// Safe source-backed row for visual and flat renderers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillsPanelRow {
    name: String,
    description: String,
    source: String,
    location: String,
    admission: heycode_skills::SkillAdmission,
    source_restricted: bool,
    catalog_tokens: usize,
}

impl SkillsPanelRow {
    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn description(&self) -> &str {
        &self.description
    }

    pub(crate) fn source(&self) -> &str {
        &self.source
    }

    pub(crate) fn location(&self) -> &str {
        &self.location
    }

    pub(crate) const fn admission(&self) -> heycode_skills::SkillAdmission {
        self.admission
    }

    pub(crate) const fn access_label(&self) -> &'static str {
        self.admission.as_str()
    }

    const fn record_is_source_restricted(&self) -> bool {
        self.source_restricted
    }

    const fn catalog_tokens(&self) -> usize {
        self.catalog_tokens
    }
}

/// Session-local search over a persisted, revision-checked catalog snapshot.
pub(crate) struct SkillsPanel {
    skills: Arc<heycode_skills::SkillSet>,
    catalog: heycode_skills::SkillCatalogSnapshot,
    rows: Vec<SkillsPanelRow>,
    search: String,
    search_active: bool,
    details_expanded: bool,
    selected: usize,
    notice: Option<String>,
    notice_is_error: bool,
    skipped: usize,
    reload_failures: usize,
    /// Admission of every catalogued skill when the panel opened.
    ///
    /// The closing receipt is a diff against exactly this, so a refused stale
    /// toggle, a sort change and a search all close as `No changes` while a
    /// durably applied cycle names what it changed.
    opening_admissions: Vec<(String, heycode_skills::SkillAdmission)>,
}

/// Name-to-admission pairs for every catalogued skill, unfiltered by search.
fn admission_map(
    catalog: &heycode_skills::SkillCatalogSnapshot,
) -> Vec<(String, heycode_skills::SkillAdmission)> {
    catalog
        .records()
        .iter()
        .map(|row| (row.record().skill.name.clone(), row.admission()))
        .collect()
}

/// Skills whose admission differs between two durable catalog readings.
///
/// A skill missing from `before` — added by a reload while the panel was open
/// — is not reported as a change, because its admission was never something
/// this panel altered.
fn admission_changes_between(
    before: &[(String, heycode_skills::SkillAdmission)],
    after: &[(String, heycode_skills::SkillAdmission)],
) -> Vec<SkillAdmissionChange> {
    after
        .iter()
        .filter_map(|(name, current)| {
            let previous = before
                .iter()
                .find(|(candidate, _)| candidate == name)
                .map(|(_, admission)| *admission)?;
            (previous != *current).then(|| SkillAdmissionChange {
                name: name.clone(),
                before: previous,
                after: *current,
            })
        })
        .collect()
}

/// Wording for a skills-panel dismissal.
///
/// `No changes` is the source's own wording for the unchanged case. The
/// changed case has no source capture, so it states exactly what the panel
/// durably applied rather than inventing a summary.
fn admission_receipt(changes: &[SkillAdmissionChange]) -> String {
    if changes.is_empty() {
        return crate::app::SKILLS_PANEL_NO_CHANGES.to_owned();
    }
    changes
        .iter()
        .map(|change| format!("{} is now {}", change.name, change.after.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// One skill whose admission differs between panel open and panel close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SkillAdmissionChange {
    /// Canonical skill name.
    pub(crate) name: String,
    /// Admission the panel opened with.
    pub(crate) before: heycode_skills::SkillAdmission,
    /// Admission durably in force now.
    pub(crate) after: heycode_skills::SkillAdmission,
}

impl SkillsPanel {
    pub(crate) fn open(
        skills: Arc<heycode_skills::SkillSet>,
    ) -> Result<Self, heycode_skills::SkillRegistryError> {
        let catalog = skills.catalog_snapshot()?;
        let skipped = skills.skipped().len();
        let reload_failures = skills.reload_diagnostics().len();
        let opening_admissions = admission_map(&catalog);
        let mut panel = Self {
            opening_admissions,
            skills,
            catalog,
            rows: Vec::new(),
            search: String::new(),
            search_active: false,
            details_expanded: false,
            selected: 0,
            notice: None,
            notice_is_error: false,
            skipped,
            reload_failures,
        };
        panel.rebuild_rows(None);
        Ok(panel)
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[SkillsPanelRow] {
        &self.rows
    }

    pub(crate) fn search(&self) -> &str {
        &self.search
    }

    pub(crate) const fn search_active(&self) -> bool {
        self.search_active
    }

    #[cfg(test)]
    pub(crate) const fn details_expanded(&self) -> bool {
        self.details_expanded
    }

    #[cfg(test)]
    const fn sort(&self) -> heycode_skills::SkillSort {
        self.catalog.sort()
    }

    pub(crate) fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// Admission changes this panel actually applied since it opened.
    ///
    /// Sorting and search never appear here, and neither does a toggle the
    /// registry refused: the comparison is between two durable catalogs, not
    /// between two views of one.
    pub(crate) fn admission_changes(&self) -> Vec<SkillAdmissionChange> {
        admission_changes_between(&self.opening_admissions, &admission_map(&self.catalog))
    }

    /// The line this panel closes its command echo with.
    pub(crate) fn close_receipt(&self) -> String {
        admission_receipt(&self.admission_changes())
    }

    pub(crate) fn summary(&self) -> String {
        let total = self.catalog.records().len();
        let mut summary = if self.search.is_empty() {
            format!("{total} skills")
        } else {
            format!("{}/{total} skills", self.rows.len())
        };
        if self.catalog.sort() != heycode_skills::SkillSort::Name {
            summary.push_str(&format!(" · sorted by {}", self.catalog.sort().as_str()));
        }
        if self.skipped != 0 {
            summary.push_str(&format!(" · {} skipped", self.skipped));
        }
        if self.reload_failures != 0 {
            summary.push_str(&format!(" · {} reload failed", self.reload_failures));
        }
        summary
    }

    pub(crate) const fn hint(&self) -> &'static str {
        if self.search_active {
            "type to filter · ↓/enter to select · esc to clear"
        } else {
            "enter/space to cycle, / to search, t to sort, Esc to close"
        }
    }

    fn visible_notice(&self) -> Option<&str> {
        self.notice().filter(|_| self.notice_is_error)
    }

    fn caption_lines(&self, width: u16) -> Vec<String> {
        let caption = format!("{} · {}", self.summary(), self.hint());
        crate::markdown::wrap_styled(
            &[Span::raw(caption)],
            usize::from(width.saturating_sub(6)).max(1),
            0,
        )
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect()
        })
        .collect()
    }

    pub(crate) fn desired_height(&self, width: u16) -> u16 {
        let rows = 6
            + self.caption_lines(width).len()
            + self.rows.len().clamp(1, 12)
            + usize::from(self.details_expanded) * 3
            + usize::from(self.visible_notice().is_some());
        u16::try_from(rows).unwrap_or(u16::MAX)
    }

    pub(crate) fn move_selection(&mut self, delta: isize) {
        let len = self.rows.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = if delta < 0 {
            self.selected
                .checked_sub(delta.unsigned_abs())
                .unwrap_or(len - 1)
        } else {
            (self.selected + delta as usize) % len
        };
    }

    pub(crate) fn handle_key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> SkillsPanelAction {
        match code {
            KeyCode::Esc if self.search_active => {
                if self.search.is_empty() {
                    self.search_active = false;
                } else {
                    self.search.clear();
                    self.rebuild_rows(None);
                }
            }
            KeyCode::Esc => return SkillsPanelAction::Close,
            KeyCode::Enter | KeyCode::Down if self.search_active => {
                self.search_active = false;
            }
            KeyCode::Up => {
                self.move_selection(-1);
                self.details_expanded = false;
            }
            KeyCode::Down => {
                self.move_selection(1);
                self.details_expanded = false;
            }
            KeyCode::Left => self.details_expanded = false,
            KeyCode::Right => self.details_expanded = !self.rows.is_empty(),
            KeyCode::Enter => self.cycle_selected(),
            KeyCode::Backspace if self.search_active => {
                self.search.pop();
                self.rebuild_rows(None);
            }
            KeyCode::Char(character)
                if self.search_active && !modifiers.contains(KeyModifiers::CONTROL) =>
            {
                if self.search.chars().count() < MAX_SEARCH_CHARS && !character.is_control() {
                    self.search.push(character);
                    self.rebuild_rows(None);
                }
            }
            KeyCode::Char('/') => self.search_active = true,
            KeyCode::Char('t') if !modifiers.contains(KeyModifiers::CONTROL) => self.cycle_sort(),
            KeyCode::Char(' ') if !modifiers.contains(KeyModifiers::CONTROL) => {
                self.cycle_selected();
            }
            KeyCode::Char(character) if modifiers.is_empty() && !character.is_control() => {
                self.search_active = true;
                if self.search.chars().count() < MAX_SEARCH_CHARS {
                    self.search.push(character);
                }
                self.rebuild_rows(None);
            }
            _ => {}
        }
        SkillsPanelAction::None
    }

    /// Paste is owned by the modal and activates its bounded search field.
    /// It can never become a prompt, command or admission change.
    pub(crate) fn paste(&mut self, value: &str) {
        self.search_active = true;
        let remaining = MAX_SEARCH_CHARS.saturating_sub(self.search.chars().count());
        self.search.extend(
            value
                .chars()
                .filter(|character| !character.is_control())
                .take(remaining),
        );
        self.rebuild_rows(None);
    }

    fn cycle_selected(&mut self) {
        let Some(selected) = self.rows.get(self.selected) else {
            return;
        };
        let name = selected.name.clone();
        let admission = if selected.record_is_source_restricted() {
            match selected.admission {
                heycode_skills::SkillAdmission::UserOnly => heycode_skills::SkillAdmission::Off,
                _ => heycode_skills::SkillAdmission::UserOnly,
            }
        } else {
            selected.admission.next()
        };
        match self.skills.set_admission(&name, admission, &self.catalog) {
            Ok(catalog) => {
                self.catalog = catalog;
                self.notice = Some(format!("{name} is now {}", admission.as_str()));
                self.notice_is_error = false;
                self.rebuild_rows(Some(&name));
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                self.notice_is_error = true;
            }
        }
    }

    fn cycle_sort(&mut self) {
        let next = self.catalog.sort().next();
        match self.skills.set_sort(next, &self.catalog) {
            Ok(catalog) => {
                self.catalog = catalog;
                self.notice = Some(format!("sorted by {}", next.as_str()));
                self.notice_is_error = false;
                self.rebuild_rows(None);
                self.selected = 0;
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                self.notice_is_error = true;
            }
        }
    }

    fn rebuild_rows(&mut self, preserve_name: Option<&str>) {
        let preserve_name = preserve_name
            .map(str::to_owned)
            .or_else(|| self.rows.get(self.selected).map(|row| row.name.clone()));
        let query = self.search.to_lowercase();
        self.rows = self
            .catalog
            .records()
            .iter()
            .map(|catalog_row| {
                let record = catalog_row.record();
                let source = safe_text(record.source.scope().as_str(), 32);
                let location = if record.source.directory().is_empty() {
                    safe_text(record.source.root(), 256)
                } else {
                    safe_text(
                        &format!("{}/{}", record.source.root(), record.source.directory()),
                        256,
                    )
                };
                SkillsPanelRow {
                    name: safe_text(&record.skill.name, 128),
                    description: safe_text(&record.skill.description, 384),
                    source,
                    location,
                    admission: catalog_row.admission(),
                    source_restricted: record.skill.disable_model_invocation,
                    catalog_tokens: catalog_row.estimated_catalog_tokens(),
                }
            })
            .filter(|row| {
                query.is_empty()
                    || row.name.to_lowercase().contains(&query)
                    || row.description.to_lowercase().contains(&query)
                    || row.source.to_lowercase().contains(&query)
                    || row.location.to_lowercase().contains(&query)
            })
            .collect();
        self.selected = preserve_name
            .as_deref()
            .and_then(|name| self.rows.iter().position(|row| row.name == name))
            .unwrap_or_else(|| self.selected.min(self.rows.len().saturating_sub(1)));
    }

    /// Screen-reader projection of the compact source-shaped catalog. Full
    /// location and description are exposed only for the deliberately expanded
    /// row, matching the visual disclosure boundary.
    pub(crate) fn accessible_lines(&self) -> Vec<String> {
        let mut lines = vec![self.summary(), self.hint().to_owned()];
        if self.rows.is_empty() {
            lines.push("No matching skills".to_owned());
        }
        for (index, row) in self.rows.iter().enumerate() {
            lines.push(format!(
                "{} {} {}; {}",
                if index == self.selected {
                    "[selected]"
                } else {
                    ""
                },
                row.access_label(),
                row.name(),
                row.description()
            ));
            lines.push(format!(
                "source: {}; location: {}",
                row.source(),
                row.location()
            ));
        }

        if let Some(notice) = self.notice() {
            lines.push(format!("Notice: {notice}"));
        }
        lines
    }

    /// Paint a compact single-line catalog with a source-shaped search box.
    /// Provenance remains available through an explicit details disclosure;
    /// ordinary rows never grow to three lines.
    pub(crate) fn draw(&self, frame: &mut Frame<'_>, area: Rect, styles: crate::terminal::Styles) {
        if area.width < 4 || area.height == 0 {
            return;
        }
        if area.height < 8 {
            let selected = self
                .rows
                .get(self.selected)
                .map(|row| format!("❯ {} {}", row.access_label(), row.name()))
                .unwrap_or_else(|| "No matching skills".to_owned());
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from("Skills"),
                    Line::from(selected),
                    Line::from("↑↓ choose · Esc close"),
                ]),
                area,
            );
            return;
        }
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::TOP)
                .border_set(ratatui::symbols::border::Set {
                    horizontal_top: "▔",
                    ..ratatui::symbols::border::PLAIN
                })
                .border_style(Style::default().fg(styles.accent())),
            area,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "   Skills",
                Style::default()
                    .fg(styles.accent())
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect::new(area.x, area.y.saturating_add(1), area.width, 1),
        );
        let caption = self.caption_lines(area.width);
        let caption_height = u16::try_from(caption.len()).unwrap_or(u16::MAX);
        frame.render_widget(
            Paragraph::new(caption.into_iter().map(Line::from).collect::<Vec<_>>())
                .style(Style::default().fg(styles.dim())),
            Rect::new(
                area.x.saturating_add(3),
                area.y.saturating_add(2),
                area.width.saturating_sub(6),
                caption_height,
            ),
        );

        let search_area = Rect::new(
            area.x.saturating_add(3),
            area.y.saturating_add(3).saturating_add(caption_height),
            area.width.saturating_sub(6),
            3,
        );
        let search_text = if self.search.is_empty() && !self.search_active {
            " ⌕ Search skills…".to_owned()
        } else {
            format!(" ⌕ {}", self.search)
        };
        frame.render_widget(
            Paragraph::new(search_text)
                .style(Style::default().fg(if self.search_active {
                    styles.text()
                } else {
                    styles.dim()
                }))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .border_type(BorderType::Rounded)
                        .border_style(Style::default().fg(if self.search_active {
                            styles.accent()
                        } else {
                            styles.dim()
                        })),
                ),
            search_area,
        );

        let detail_rows = if self.details_expanded { 3 } else { 0 };
        let notice_rows = usize::from(self.visible_notice().is_some());
        let available = usize::from(area.bottom().saturating_sub(search_area.bottom()))
            .saturating_sub(detail_rows)
            .saturating_sub(notice_rows)
            .max(1);
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(available)
            .min(self.rows.len().saturating_sub(available));
        let row_area = Rect::new(
            area.x.saturating_add(3),
            search_area.bottom(),
            area.width.saturating_sub(6),
            u16::try_from(available).unwrap_or(u16::MAX),
        );
        let mut lines = Vec::new();
        for (index, row) in self.rows.iter().enumerate().skip(start).take(available) {
            let selected = index == self.selected;
            let state_style = match row.admission() {
                heycode_skills::SkillAdmission::On => Style::default().fg(styles.success()),
                heycode_skills::SkillAdmission::NameOnly => Style::default().fg(styles.text()),
                heycode_skills::SkillAdmission::UserOnly => Style::default().fg(styles.text()),
                heycode_skills::SkillAdmission::Off => Style::default().fg(styles.error()),
            };
            lines.push(Line::from(vec![
                Span::styled(
                    if selected { "❯ " } else { "  " },
                    Style::default().fg(styles.accent()),
                ),
                Span::styled(
                    format!(
                        "{} {:<9}  ",
                        state_marker(row.admission()),
                        row.access_label()
                    ),
                    state_style,
                ),
                Span::styled(
                    row.name().to_owned(),
                    if selected {
                        Style::default().fg(styles.accent())
                    } else {
                        Style::default().fg(styles.text())
                    },
                ),
                Span::styled(
                    format!(
                        " · {} · {}",
                        row.source(),
                        token_label(row.catalog_tokens())
                    ),
                    Style::default().fg(styles.dim()),
                ),
            ]));
        }
        if lines.is_empty() {
            lines.push(Line::from(Span::styled(
                if self.search.is_empty() {
                    "No skills found".to_owned()
                } else {
                    format!("No skills match \"{}\"", self.search)
                },
                Style::default().fg(styles.dim()),
            )));
        }
        frame.render_widget(Paragraph::new(lines), row_area);

        let mut footer_y = area.bottom();
        if self.details_expanded
            && let Some(row) = self.rows.get(self.selected)
        {
            footer_y = footer_y.saturating_sub(2);
            frame.render_widget(
                Paragraph::new(vec![
                    Line::from(vec![
                        Span::styled("  Description  ", Style::default().fg(styles.dim())),
                        Span::styled(
                            row.description().to_owned(),
                            Style::default().fg(styles.text()),
                        ),
                    ]),
                    Line::from(vec![
                        Span::styled("  Source       ", Style::default().fg(styles.dim())),
                        Span::styled(
                            format!("{} · {}", row.source(), row.location()),
                            Style::default().fg(styles.text()),
                        ),
                    ]),
                ]),
                Rect::new(area.x, footer_y, area.width, 2),
            );
        }
        if let Some(notice) = self.visible_notice() {
            footer_y = footer_y.saturating_sub(1);
            frame.render_widget(
                Paragraph::new(format!("  {notice}")).style(Style::default().fg(styles.warn())),
                Rect::new(area.x, footer_y, area.width, 1),
            );
        }
        if self.search_active() {
            let search_width = u16::try_from(unicode_width::UnicodeWidthStr::width(self.search()))
                .unwrap_or(u16::MAX);
            frame.set_cursor_position((
                search_area
                    .x
                    .saturating_add(4)
                    .saturating_add(search_width)
                    .min(search_area.right().saturating_sub(2)),
                search_area.y.saturating_add(1),
            ));
        }
    }
}

const fn state_marker(admission: heycode_skills::SkillAdmission) -> &'static str {
    match admission {
        heycode_skills::SkillAdmission::On => "✔",
        heycode_skills::SkillAdmission::NameOnly => "●",
        heycode_skills::SkillAdmission::UserOnly => "◯",
        heycode_skills::SkillAdmission::Off => "✘",
    }
}

fn token_label(tokens: usize) -> String {
    if tokens < 20 {
        "< 20 tok".to_owned()
    } else {
        format!("~{tokens} tok")
    }
}

fn safe_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(limit)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use ratatui::{Terminal, backend::TestBackend};

    fn skills() -> Arc<heycode_skills::SkillSet> {
        Arc::new(
            heycode_skills::SkillSet::new(vec![
                heycode_skills::Skill {
                    name: "zeta".to_owned(),
                    description: "Project reviewer".to_owned(),
                    disable_model_invocation: false,
                    body: "not rendered".to_owned(),
                },
                heycode_skills::Skill {
                    name: "alpha".to_owned(),
                    description: "User-only deploy helper".to_owned(),
                    disable_model_invocation: true,
                    body: "not rendered".to_owned(),
                },
            ])
            .unwrap(),
        )
    }

    #[test]
    fn search_owns_paste_and_escape_clears_then_returns_to_selection() {
        let mut panel = SkillsPanel::open(skills()).unwrap();
        panel.paste("PROJECT");
        assert!(panel.search_active());
        assert_eq!(panel.rows().len(), 1);
        assert_eq!(panel.rows()[0].name(), "zeta");
        panel.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(panel.search_active());
        assert!(panel.search().is_empty());
        assert_eq!(panel.rows().len(), 2);
        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(!panel.search_active());
        assert!(
            panel.notice().is_none(),
            "search selection must not mutate admission"
        );
        panel.handle_key(KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(panel.search(), "i", "ordinary letters activate filtering");
        panel.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        panel.handle_key(KeyCode::Esc, KeyModifiers::NONE);
        assert!(!panel.search_active());
        assert_eq!(
            panel.handle_key(KeyCode::Esc, KeyModifiers::NONE),
            SkillsPanelAction::Close
        );
    }

    #[test]
    fn a_dismissal_that_applied_nothing_closes_as_no_changes() {
        let mut panel = SkillsPanel::open(skills()).unwrap();
        // Sorting, searching and a refused toggle all leave admission alone.
        panel.paste("PROJECT");
        panel.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(panel.admission_changes().is_empty());
        assert_eq!(panel.close_receipt(), crate::app::SKILLS_PANEL_NO_CHANGES);
    }

    #[test]
    fn a_dismissal_after_admission_changes_names_what_changed() {
        let before = [
            ("alpha".to_owned(), heycode_skills::SkillAdmission::On),
            ("zeta".to_owned(), heycode_skills::SkillAdmission::On),
        ];
        let unchanged = admission_changes_between(&before, &before);
        assert!(unchanged.is_empty());
        assert_eq!(
            admission_receipt(&unchanged),
            crate::app::SKILLS_PANEL_NO_CHANGES
        );

        let after = [
            ("alpha".to_owned(), heycode_skills::SkillAdmission::NameOnly),
            ("zeta".to_owned(), heycode_skills::SkillAdmission::On),
        ];
        let one = admission_changes_between(&before, &after);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].name, "alpha");
        assert_eq!(one[0].before, heycode_skills::SkillAdmission::On);
        assert_eq!(one[0].after, heycode_skills::SkillAdmission::NameOnly);
        assert_eq!(admission_receipt(&one), "alpha is now name-only");

        let both = [
            ("alpha".to_owned(), heycode_skills::SkillAdmission::NameOnly),
            ("zeta".to_owned(), heycode_skills::SkillAdmission::Off),
        ];
        assert_eq!(
            admission_receipt(&admission_changes_between(&before, &both)),
            "alpha is now name-only, zeta is now off"
        );

        // A skill the panel never saw is not something the panel changed.
        let added = [
            ("alpha".to_owned(), heycode_skills::SkillAdmission::On),
            ("zeta".to_owned(), heycode_skills::SkillAdmission::On),
            ("beta".to_owned(), heycode_skills::SkillAdmission::Off),
        ];
        assert!(admission_changes_between(&before, &added).is_empty());
    }

    #[test]
    fn detached_mutations_fail_visible_and_leave_the_prior_rows_unchanged() {
        let mut panel = SkillsPanel::open(skills()).unwrap();
        let before = panel.rows().to_vec();
        panel.handle_key(KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            panel
                .notice()
                .unwrap()
                .contains("settings are not writable")
        );
        assert_eq!(panel.rows(), before);
        panel.handle_key(KeyCode::Char('t'), KeyModifiers::NONE);
        assert!(
            panel
                .notice()
                .unwrap()
                .contains("settings are not writable")
        );
        assert_eq!(panel.sort(), heycode_skills::SkillSort::Name);
    }

    #[test]
    fn accessibility_retains_skill_metadata_while_visual_details_toggle() {
        let mut panel = SkillsPanel::open(skills()).unwrap();
        let collapsed = panel.accessible_lines();
        assert!(
            collapsed
                .iter()
                .any(|line| line.contains("[selected] user-only alpha; User-only deploy helper")),
            "{collapsed:?}"
        );
        assert!(
            collapsed
                .iter()
                .any(|line| line.contains("source: contribution; location: live registry")),
            "{collapsed:?}"
        );
        assert!(!panel.details_expanded());
        panel.handle_key(KeyCode::Right, KeyModifiers::NONE);
        assert!(panel.details_expanded());
        assert_eq!(panel.accessible_lines(), collapsed);
        panel.handle_key(KeyCode::Down, KeyModifiers::NONE);
        assert!(!panel.details_expanded());
    }

    #[test]
    fn source_shaped_render_has_bordered_search_and_single_line_catalog_rows() {
        let panel = SkillsPanel::open(skills()).unwrap();
        let mut terminal = Terminal::new(TestBackend::new(100, 22)).unwrap();
        terminal
            .draw(|frame| panel.draw(frame, frame.area(), crate::terminal::Styles::default()))
            .unwrap();
        let width = usize::from(terminal.backend().buffer().area.width);
        let rows = terminal
            .backend()
            .buffer()
            .content
            .chunks(width)
            .map(|cells| cells.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>();
        assert!(rows.iter().any(|row| row.contains("⌕ Search skills")));
        assert_eq!(rows.iter().filter(|row| row.contains("alpha")).count(), 1);
        assert_eq!(rows.iter().filter(|row| row.contains("zeta")).count(), 1);
        assert!(rows.iter().any(|row| row.contains("t to sort")));
        assert!(!rows.iter().any(|row| row.contains("Project reviewer")));
    }
}
