//! What a retained health observation is allowed to be.
//!
//! A support bundle is a file a user sends to someone else, so the question
//! this module answers is not "what would be useful to keep" but "what is
//! provably safe to hand over". The answer is the same shape S15 and Q08
//! reached: closed vocabularies, numbers, and exactly one place text enters.
//!
//! **Evidence bodies are structurally absent.** A live [`DoctorReport`] can
//! carry a [`heycode_doctor::DoctorEvidence`], and inside it a composition
//! diagnostic's free-text `message`, a config-migration `path` and a
//! `backup_path`. None of that is retained here: an entry keeps only a closed
//! [`HealthEvidenceKind`] tag saying which kind of evidence the live report
//! carried. That is not squeamishness about those particular fields — it is
//! that a history is read for *trend* ("which check failed, when, and how
//! long did it take"), and the live report is where the body belongs. Removing
//! the field removes the leak path with it.
//!
//! **[`HealthLabel`] is the one place text enters**, and it screens on
//! construction *and* on deserialization. Both matter. A doctor check id is
//! validated by `heycode-doctor` to be lowercase ASCII with `.` and `-` — which
//! `sk-ant-api03-0123456789abcdef` satisfies exactly — so screening on the way
//! in is not decoration. And a value that only validates on the way in is the
//! hole a `#[serde(transparent)]` newtype leaves open: the retained file is
//! read back after a restart, so the read path is a real entry point.

use std::time::Duration;

use heycode_doctor::{DoctorCheckResult, DoctorEvidence, DoctorRun, DoctorStatus, DoctorSummary};
use serde::{Deserialize, Serialize};

/// Current serialized health-history schema.
///
/// A file whose header names a higher version is refused rather than guessed
/// at, and never overwritten.
pub const HEALTH_HISTORY_SCHEMA_VERSION: u32 = 1;

/// Hard cap on retained entries. Enforced on every load and every record.
pub const MAX_ENTRIES: usize = 64;

/// Hard cap on the serialized history document, in bytes.
///
/// Both this and [`MAX_ENTRIES`] are reachable: a maximal entry is roughly
/// 70 KiB (see [`ENTRY_BYTES_CEILING`]), so 64 of them would be far over this
/// budget and the byte cap binds first; ordinary entries are a few hundred
/// bytes, so the entry cap binds first. Neither is decoration.
pub const MAX_BYTES: usize = 256 * 1024;

/// Most recent unhealthy entries held back from ordinary eviction.
///
/// Plain oldest-first eviction has a failure mode worth naming: a burst of
/// healthy runs pushes out the one failing run that explains the problem the
/// user is reporting. Protecting the most recent unhealthy entries is a
/// *preference*, never an exemption — when every retained entry is protected
/// the oldest is evicted anyway, because the cap is hard.
pub const PROTECTED_UNHEALTHY: usize = 16;

/// Most check rows one entry retains.
pub const MAX_CHECKS_PER_ENTRY: usize = 64;

/// Longest text any retained label holds, in bytes.
pub const LABEL_MAX_BYTES: usize = 512;

/// A check at or over this wall time renders even when it passed.
///
/// GOTCHAS #155: a host that takes 11-23s to first-execute a newly written
/// binary reports a good binary as unavailable under a 5s budget. A passing
/// check that took 20 seconds is the evidence that distinguishes "slow once"
/// from "broken", and a render that only showed failures would hide it.
pub const SLOW_CHECK_MS: u64 = 1_000;

/// Text substituted for a label that named credential material.
///
/// Shared with S15 so one placeholder means one thing everywhere.
pub const REDACTED_LABEL: &str = heycode_settings::REDACTED_PLACEHOLDER;

/// Text substituted for a label longer than [`LABEL_MAX_BYTES`].
///
/// Replaced whole rather than truncated: a truncated identifier still carries
/// its first 512 bytes, and the reason a label is oversized in the first place
/// is that something unbounded reached it.
pub const OVERSIZED_LABEL: &str = "[OVERSIZED]";

/// Upper bound on one serialized entry, derived from the per-field caps.
///
/// This exists to make the byte bound *total*: eviction never removes the last
/// remaining entry, so the file cap holds only if one entry can never exceed
/// it. The const assertion below is the proof, and
/// `a_maximal_entry_stays_under_the_entry_ceiling` is what keeps the
/// arithmetic honest against the real encoder.
pub const ENTRY_BYTES_CEILING: usize =
    ENTRY_ENVELOPE_BYTES + MAX_CHECKS_PER_ENTRY * CHECK_ROW_BYTES;

/// Fixed per-entry JSON overhead: field names, the summary object, numbers.
const ENTRY_ENVELOPE_BYTES: usize = 512 + LABEL_MAX_BYTES;

/// Per-check JSON overhead: two labels, a status word, two numbers, punctuation.
const CHECK_ROW_BYTES: usize = 2 * LABEL_MAX_BYTES + 128;

const _: () = assert!(ENTRY_BYTES_CEILING < MAX_BYTES);
const _: () = assert!(PROTECTED_UNHEALTHY < MAX_ENTRIES);

/// A bounded string that has passed the credential screen.
///
/// There is no constructor that skips the screen and no deserialization route
/// around it: serde converts from `String` through [`HealthLabel::new`], so a
/// hand-edited history file cannot smuggle a secret back in on read. Screening
/// substitutes rather than refuses, because a diagnostic that refuses to load
/// is a diagnostic nobody has — and the substitution is visible in the render,
/// so nothing is lost silently.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(from = "String", into = "String")]
pub struct HealthLabel(String);

impl HealthLabel {
    /// Screen `value` and hold the result.
    ///
    /// Size is checked first so an unbounded string is never handed to the
    /// recognizers, and an oversized value is replaced whole — which redacts
    /// whatever it contained as a side effect.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        let value = value.into();
        if value.len() > LABEL_MAX_BYTES {
            return Self(OVERSIZED_LABEL.to_owned());
        }
        if screen(&value).is_err() {
            return Self(REDACTED_LABEL.to_owned());
        }
        Self(value)
    }

    /// The screened text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for HealthLabel {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

impl From<HealthLabel> for String {
    fn from(label: HealthLabel) -> Self {
        label.0
    }
}

impl std::fmt::Display for HealthLabel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Screen text for credential material with S15's recognizers.
///
/// The second pass is not a second recognizer — there is still exactly one
/// list of what a credential looks like and it is S15's (GOTCHAS #154). It is
/// a *tokenizer*: `heycode_settings::screen_text_for_credentials` splits on
/// whitespace and JSON punctuation, so a URL arrives as a single token
/// beginning with `https` and every issuer-prefix check misses it. Splitting
/// the URL structure delimiters hands the shared screen tokens it can read.
/// `.` is deliberately not split, because a JSON Web Token is three
/// dot-separated segments and splitting there blinds the recognizer that finds
/// it.
///
/// `heycode-telemetry` closes the same gap with the same one-line split, and its
/// copy is `pub(crate)`. That duplication is worth removing by promoting the
/// wrapper into `heycode-settings` beside the recognizers it feeds.
fn screen(text: &str) -> Result<(), heycode_settings::WireExposureFault> {
    heycode_settings::screen_text_for_credentials(text)?;
    heycode_settings::screen_text_for_credentials(
        &text.replace(['/', ':', '?', '#', '@', '&', '='], " "),
    )
}

/// Which kind of evidence the live report carried, without its body.
///
/// Matched exhaustively against [`DoctorEvidence`], so a new evidence variant
/// is a compile error here rather than an untagged row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HealthEvidenceKind {
    /// The live report carried K08's composition report.
    Composition,
    /// The live report carried redacted config-migration evidence.
    ConfigMigration,
}

impl HealthEvidenceKind {
    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Composition => "composition",
            Self::ConfigMigration => "config_migration",
        }
    }

    fn of(evidence: &DoctorEvidence) -> Self {
        match evidence {
            DoctorEvidence::Composition { .. } => Self::Composition,
            DoctorEvidence::ConfigMigration { .. } => Self::ConfigMigration,
        }
    }
}

/// One check's outcome inside one retained run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheckRecord {
    /// Screened check id.
    pub id: HealthLabel,
    /// Outcome severity, in `heycode-doctor`'s vocabulary.
    pub status: DoctorStatus,
    /// Screened stable result code.
    pub code: HealthLabel,
    /// Measured wall time of this check.
    pub duration_ms: u64,
    /// Which evidence the live report carried, never the evidence itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence: Option<HealthEvidenceKind>,
}

impl HealthCheckRecord {
    fn from_result(result: &DoctorCheckResult, duration: Duration) -> Self {
        Self {
            id: HealthLabel::new(result.id.as_str()),
            status: result.status,
            code: HealthLabel::new(result.code.as_str()),
            duration_ms: millis(duration),
            evidence: result.evidence.as_ref().map(HealthEvidenceKind::of),
        }
    }

    /// Whether this row is worth rendering on its own line.
    #[must_use]
    pub const fn is_noteworthy(&self) -> bool {
        !matches!(self.status, DoctorStatus::Pass) || self.duration_ms >= SLOW_CHECK_MS
    }
}

/// One retained doctor run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthEntry {
    /// Wall-clock instant the run was recorded, supplied by the caller.
    ///
    /// Ordering in the file is authoritative, not this value: a clock can move
    /// backwards and a history that re-sorted itself on read would reorder the
    /// evidence.
    pub at_unix_ms: u64,
    /// Screened product build that recorded the run.
    pub build: HealthLabel,
    /// True when no check failed or was skipped. Warnings remain healthy.
    pub healthy: bool,
    /// Aggregate counts over *every* check the run produced, including rows
    /// this entry did not retain.
    pub summary: DoctorSummary,
    /// Measured wall time of the whole run.
    pub duration_ms: u64,
    /// Retained check rows.
    pub checks: Vec<HealthCheckRecord>,
    /// Check rows the per-entry bound dropped.
    pub omitted_checks: u32,
}

impl HealthEntry {
    /// Project one completed run into a retained entry.
    ///
    /// `build` is supplied rather than read from this crate's version so the
    /// value is a caller's decision and a test can plant one, exactly as
    /// `TelemetryEvent` takes its timestamp instead of reading a clock.
    ///
    /// When a run has more than [`MAX_CHECKS_PER_ENTRY`] checks the rows that
    /// are kept are chosen the same way entries are: the ones that carry the
    /// diagnostic signal first. Non-passing rows are retained ahead of passing
    /// ones, each group in report order, and `summary` still counts them all.
    #[must_use]
    pub fn from_run(build: &str, run: &DoctorRun, at_unix_ms: u64, duration: Duration) -> Self {
        let mut rows: Vec<HealthCheckRecord> = run
            .report
            .checks
            .iter()
            .map(|result| {
                let measured = run
                    .timings
                    .iter()
                    .find(|timing| timing.id == result.id)
                    .map_or(Duration::ZERO, |timing| timing.duration);
                HealthCheckRecord::from_result(result, measured)
            })
            .collect();
        let omitted =
            u32::try_from(rows.len().saturating_sub(MAX_CHECKS_PER_ENTRY)).unwrap_or(u32::MAX);
        if rows.len() > MAX_CHECKS_PER_ENTRY {
            let (noteworthy, ordinary): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|row| !matches!(row.status, DoctorStatus::Pass));
            rows = noteworthy
                .into_iter()
                .chain(ordinary)
                .take(MAX_CHECKS_PER_ENTRY)
                .collect();
        }
        Self {
            at_unix_ms,
            build: HealthLabel::new(build),
            healthy: run.report.healthy,
            summary: run.report.summary,
            duration_ms: millis(duration),
            checks: rows,
            omitted_checks: omitted,
        }
    }

    /// Read one retained entry, re-checking the bound construction enforced.
    ///
    /// Serde restores fields; it does not re-run [`Self::from_run`]. So the
    /// per-entry check bound is checked again here rather than assumed — the
    /// same reason [`HealthLabel`] deserializes through its constructor, and
    /// the same reason the byte bound can treat one entry as always fitting.
    ///
    /// # Errors
    /// A parse failure, or a check list longer than [`MAX_CHECKS_PER_ENTRY`],
    /// each reported as a parse failure so no caller acts on a row it does not
    /// understand.
    pub fn from_json(line: &str) -> Result<Self, serde_json::Error> {
        let entry: Self = serde_json::from_str(line)?;
        if entry.checks.len() > MAX_CHECKS_PER_ENTRY {
            return Err(serde::de::Error::custom(format!(
                "health entry retains {} checks; the bound is {MAX_CHECKS_PER_ENTRY}",
                entry.checks.len()
            )));
        }
        Ok(entry)
    }

    /// Deterministic one-line summary of this run.
    #[must_use]
    pub fn render_line(&self) -> String {
        format!(
            "at={} build={} {} ({} passed, {} warnings, {} failed, {} skipped) in {}ms",
            self.at_unix_ms,
            self.build,
            if self.healthy { "healthy" } else { "unhealthy" },
            self.summary.passed,
            self.summary.warnings,
            self.summary.failed,
            self.summary.skipped,
            self.duration_ms,
        )
    }
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}
