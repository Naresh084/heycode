//! U22 bounded read-only side-panel projections.
//!
//! Diff, jobs and agents consume the same live state as the transcript and
//! model-facing registries. They do not scan the workspace, mint job state or
//! expose cross-owner child ids. A renderer receives one bounded snapshot and
//! decides only layout/color.

use heycode_agent::{JobRegistry, JobState, SubagentRegistry};

use crate::app::Item;

const MAX_ROWS: usize = 64;
const MAX_ROW_CHARS: usize = 512;

/// One optional side-panel identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidePanelKind {
    /// Recent exact edit/write diffs already present in transcript state.
    Diff,
    /// Effect-owned background job snapshots.
    Jobs,
    /// Registered provider/preset facts without child identities.
    Agents,
}

impl SidePanelKind {
    /// Stable UI contribution id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Diff => "diff",
            Self::Jobs => "jobs",
            Self::Agents => "agents",
        }
    }

    /// Human title.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Diff => "Diff",
            Self::Jobs => "Tasks",
            Self::Agents => "Agents",
        }
    }

    /// Next panel in the single keyboard cycle; Agents closes the sidebar.
    #[must_use]
    pub const fn next(self) -> Option<Self> {
        match self {
            Self::Diff => Some(Self::Jobs),
            Self::Jobs => Some(Self::Agents),
            Self::Agents => None,
        }
    }
}

/// Semantic tone for one rendered row. Color is optional; the row text stays
/// self-describing in flat/no-color modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidePanelTone {
    /// Ordinary metadata.
    Normal,
    /// Added diff line or successful state.
    Positive,
    /// Removed diff line or failed state.
    Negative,
    /// Pending/cancelled/advisory state.
    Warning,
}

/// One bounded, control-free side-panel row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidePanelRow {
    text: String,
    tone: SidePanelTone,
}

impl SidePanelRow {
    fn new(text: impl AsRef<str>, tone: SidePanelTone) -> Self {
        Self {
            text: sanitize(text.as_ref()),
            tone,
        }
    }

    /// Safe row text.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Semantic tone.
    #[must_use]
    pub const fn tone(&self) -> SidePanelTone {
        self.tone
    }
}

/// Complete bounded side-panel view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidePanelSnapshot {
    kind: SidePanelKind,
    summary: String,
    rows: Vec<SidePanelRow>,
}

impl SidePanelSnapshot {
    /// Panel kind/title.
    #[must_use]
    pub const fn kind(&self) -> SidePanelKind {
        self.kind
    }

    /// Safe one-line summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Bounded rows.
    #[must_use]
    pub fn rows(&self) -> &[SidePanelRow] {
        &self.rows
    }
}

/// Marker capability stored in the UI registry for one side-panel slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidePanelContribution {
    kind: SidePanelKind,
}

impl SidePanelContribution {
    /// Marker for `kind`.
    #[must_use]
    pub const fn new(kind: SidePanelKind) -> Self {
        Self { kind }
    }

    /// Contributed panel kind.
    #[must_use]
    pub const fn kind(self) -> SidePanelKind {
        self.kind
    }
}

/// Construct one side-panel UI descriptor.
///
/// # Errors
/// Invalid ids/titles are returned by the shared UI registry boundary.
pub fn descriptor(
    id: impl Into<String>,
    title: impl Into<String>,
) -> Result<heycode_ui::UiContributionDescriptor, heycode_ui::UiRegistryError> {
    heycode_ui::UiContributionDescriptor::new(heycode_ui::UiSlot::SidePanel, id, title, 50)
}

/// Project recent edit/write diffs already held by transcript state.
#[must_use]
pub fn diff_snapshot(items: &[Item]) -> SidePanelSnapshot {
    let mut rows = Vec::new();
    for item in items {
        let Item::Tool {
            name,
            args,
            result: Some((ok, value)),
            ..
        } = item
        else {
            continue;
        };
        if !matches!(name.as_str(), "edit" | "write") {
            continue;
        }
        let value = normalized_result(value);
        let Some(diff) = value.get("diff").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let path = args
            .get("path")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("workspace edit");
        rows.push(SidePanelRow::new(
            format!("{} {path}", if *ok { "changed" } else { "failed" }),
            if *ok {
                SidePanelTone::Normal
            } else {
                SidePanelTone::Negative
            },
        ));
        rows.extend(diff.lines().map(|line| {
            let tone = match line.as_bytes().first() {
                Some(b'+') => SidePanelTone::Positive,
                Some(b'-') => SidePanelTone::Negative,
                _ => SidePanelTone::Normal,
            };
            SidePanelRow::new(line, tone)
        }));
    }
    retain_tail(&mut rows);
    SidePanelSnapshot {
        kind: SidePanelKind::Diff,
        summary: if rows.is_empty() {
            "No edit diff is present in this transcript.".to_owned()
        } else {
            format!("{} recent diff rows", rows.len())
        },
        rows,
    }
}

/// Project the effect-owned job registry.
#[must_use]
pub fn jobs_snapshot(jobs: Option<&JobRegistry>, items: &[Item]) -> SidePanelSnapshot {
    let todos = latest_todos(items);
    let completed = todos
        .iter()
        .filter(|todo| todo.get("status").and_then(serde_json::Value::as_str) == Some("completed"))
        .count();
    let mut rows = todos
        .iter()
        .map(|todo| {
            let status = todo
                .get("status")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("pending");
            let content = todo
                .get("content")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unnamed task");
            let tone = match status {
                "completed" => SidePanelTone::Positive,
                "in_progress" => SidePanelTone::Warning,
                _ => SidePanelTone::Normal,
            };
            SidePanelRow::new(format!("todo · {status} · {content}"), tone)
        })
        .collect::<Vec<_>>();
    let snapshots = jobs.map(JobRegistry::list).unwrap_or_default();
    rows.extend(snapshots.iter().map(|job| {
        let (state, tone) = match &job.state {
            JobState::Queued => ("queued", SidePanelTone::Warning),
            JobState::Cancelling => ("cancelling", SidePanelTone::Warning),
            JobState::Running => ("running", SidePanelTone::Warning),
            JobState::Settled(outcome) => (
                outcome.name(),
                if matches!(outcome, heycode_agent::JobOutcome::Completed) {
                    SidePanelTone::Positive
                } else {
                    SidePanelTone::Negative
                },
            ),
        };
        SidePanelRow::new(format!("job {} · {state} · {}", job.id, job.label), tone)
    }));
    retain_tail(&mut rows);
    let running = snapshots
        .iter()
        .filter(|job| job.state == JobState::Running)
        .count();
    SidePanelSnapshot {
        kind: SidePanelKind::Jobs,
        summary: format!(
            "{} todos · {completed} complete · {} jobs · {running} running{}",
            todos.len(),
            snapshots.len(),
            if jobs.is_none() {
                " · job registry unavailable"
            } else {
                ""
            }
        ),
        rows,
    }
}

fn latest_todos(items: &[Item]) -> Vec<serde_json::Value> {
    items
        .iter()
        .rev()
        .find_map(|item| {
            let Item::Tool {
                name,
                result: Some((true, value)),
                ..
            } = item
            else {
                return None;
            };
            matches!(name.as_str(), "todo_write" | "mcp__heycode__todo_write")
                .then(|| normalized_result(value).as_array().cloned())
                .flatten()
        })
        .unwrap_or_default()
}

/// Project registered providers/presets without exposing any owner's child ids.
#[must_use]
pub fn agents_snapshot(registry: Option<&SubagentRegistry>) -> SidePanelSnapshot {
    let Some(registry) = registry else {
        return SidePanelSnapshot {
            kind: SidePanelKind::Agents,
            summary: "Subagent registry is unavailable.".to_owned(),
            rows: Vec::new(),
        };
    };
    let descriptors = registry.descriptors();
    let presets = registry.presets();
    let mut rows = descriptors
        .iter()
        .map(|descriptor| {
            SidePanelRow::new(
                format!("provider {} · {}", descriptor.id(), descriptor.display()),
                SidePanelTone::Normal,
            )
        })
        .collect::<Vec<_>>();
    rows.extend(presets.iter().map(|preset| {
        SidePanelRow::new(
            format!("preset {} · {}", preset.id(), preset.display()),
            SidePanelTone::Normal,
        )
    }));
    retain_tail(&mut rows);
    SidePanelSnapshot {
        kind: SidePanelKind::Agents,
        summary: format!(
            "{} providers · {} presets · {} live children",
            descriptors.len(),
            presets.len(),
            registry.live_child_count()
        ),
        rows,
    }
}

fn normalized_result(value: &serde_json::Value) -> serde_json::Value {
    value.as_str().map_or_else(
        || value.clone(),
        |text| serde_json::from_str(text).unwrap_or_else(|_| value.clone()),
    )
}

fn retain_tail(rows: &mut Vec<SidePanelRow>) {
    if rows.len() > MAX_ROWS {
        rows.drain(..rows.len().saturating_sub(MAX_ROWS));
    }
}

fn sanitize(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(MAX_ROW_CHARS)
        .collect()
}
