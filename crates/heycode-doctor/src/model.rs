//! Stable redacted doctor wire and human-report vocabulary.

use serde::{Deserialize, Serialize};

use crate::DoctorError;

/// Current serialized doctor-report schema.
pub const DOCTOR_REPORT_SCHEMA_VERSION: u32 = 1;

/// Validated stable id for one contributed check.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DoctorCheckId(String);

impl DoctorCheckId {
    /// Validate a lowercase dotted/kebab diagnostic id.
    ///
    /// # Errors
    /// Invalid, empty or oversized ids return [`DoctorError::InvalidIdentifier`].
    pub fn new(value: impl Into<String>) -> Result<Self, DoctorError> {
        let value = value.into();
        validate_identifier(&value, "check id")?;
        Ok(Self(value))
    }

    /// Stable wire/display representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for DoctorCheckId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Validated stable result code.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DoctorCode(String);

impl DoctorCode {
    pub(crate) fn new(value: impl Into<String>) -> Result<Self, DoctorError> {
        let value = value.into();
        validate_identifier(&value, "result code")?;
        Ok(Self(value))
    }

    /// Stable wire/display representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Severity/outcome of one check.
///
/// Readable as well as writable: a durable retained diagnostic history is
/// still this vocabulary after a restart, and a second copy of it would be a
/// second thing to keep in step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DoctorStatus {
    /// Check completed and the property holds.
    Pass,
    /// Check completed with a non-blocking concern.
    Warning,
    /// Check found a blocking or invalid condition.
    Failure,
    /// Check was not run, usually because the operation was cancelled.
    Skipped,
}

impl DoctorStatus {
    /// Stable wire/display identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warning => "warning",
            Self::Failure => "failure",
            Self::Skipped => "skipped",
        }
    }
}

/// Typed runtime evidence. Variants are deliberately closed to prevent an
/// arbitrary string/JSON escape hatch around the redaction contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctorEvidence {
    /// K08's already-redacted, side-effect-free composition report.
    Composition {
        /// Complete dry graph result.
        report: heycode_core::CompositionReport,
    },
    /// Redacted semantic config-migration state. Raw source/rendered bytes are
    /// structurally absent.
    ConfigMigration {
        /// Safe migration projection.
        report: ConfigMigrationEvidence,
    },
}

/// Safe config version classification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigVersionEvidence {
    /// No persisted version marker.
    Unversioned,
    /// Explicit older version.
    Older {
        /// Observed version.
        version: u32,
    },
    /// Explicit current version.
    Current {
        /// Observed version.
        version: u32,
    },
    /// Explicit newer version.
    Newer {
        /// Observed version.
        version: u32,
    },
}

/// Safe config-migration disposition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigMigrationDispositionEvidence {
    /// Home-owned document was atomically migrated with an exact backup.
    HomeApplied {
        /// Backup path; never file contents.
        backup_path: String,
    },
    /// Explicit/project-owned document was not modified automatically.
    UserOwnedPending,
}

/// One semantic config change with no raw document/value surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ConfigMigrationChangeEvidence {
    /// Rename legacy unconditional `auto` approval to `full_access`.
    RenameLegacyAutoApproval,
    /// Advance the root schema marker.
    SetSchemaVersion {
        /// Prior marker; `None` was unversioned.
        from: Option<u32>,
        /// New marker.
        to: u32,
    },
    /// Replace a historical generated plugin snapshot with the built-in profile.
    UseBuiltinProfile {
        /// Recognized frozen rows.
        frozen_plugins: Vec<String>,
        /// Newly activated built-in rows.
        activated_plugins: Vec<String>,
    },
    /// Materialize a newly explicit plugin dependency.
    AddRequiredProfilePlugin {
        /// Inserted plugin.
        plugin: String,
        /// Existing dependent plugin.
        required_by: String,
    },
    /// Move one authorization-flow contribution to its provider plugin.
    MoveAuthorizationFlowToProviderPlugin {
        /// Stable flow id.
        flow: String,
        /// Historical shared plugin.
        from_plugin: String,
        /// Provider-owned plugin.
        to_plugin: String,
    },
    /// Retire native secret storage in favor of the heycode home credential file.
    UseHomeCredentialStore,
    /// Replace only the retired setup-generated DeepSeek default.
    ReplaceRetiredDeepSeekDefault {
        /// Retired id.
        from: String,
        /// Current id.
        to: String,
    },
}

/// Redacted migration evidence shared by human/JSON doctor projections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigMigrationEvidence {
    /// Source path only; raw bytes are never present.
    pub path: String,
    /// Prior version classification.
    pub from: ConfigVersionEvidence,
    /// Supported target schema.
    pub to: u32,
    /// Applied vs pending ownership decision.
    pub disposition: ConfigMigrationDispositionEvidence,
    /// Semantic changes only.
    pub changes: Vec<ConfigMigrationChangeEvidence>,
}

/// Outcome returned by a check before the registry assigns its authoritative id.
#[derive(Debug, Clone)]
pub struct DoctorOutcome {
    status: DoctorStatus,
    code: DoctorCode,
    summary: &'static str,
    repair: Option<&'static str>,
    evidence: Option<DoctorEvidence>,
}

impl DoctorOutcome {
    /// Construct a passing outcome.
    ///
    /// # Errors
    /// Invalid codes or static summaries fail at construction.
    pub fn pass(code: &'static str, summary: &'static str) -> Result<Self, DoctorError> {
        Self::new(DoctorStatus::Pass, code, summary)
    }

    /// Construct a warning outcome.
    ///
    /// # Errors
    /// Invalid codes or static summaries fail at construction.
    pub fn warning(code: &'static str, summary: &'static str) -> Result<Self, DoctorError> {
        Self::new(DoctorStatus::Warning, code, summary)
    }

    /// Construct a failing outcome.
    ///
    /// # Errors
    /// Invalid codes or static summaries fail at construction.
    pub fn failure(code: &'static str, summary: &'static str) -> Result<Self, DoctorError> {
        Self::new(DoctorStatus::Failure, code, summary)
    }

    fn new(
        status: DoctorStatus,
        code: &'static str,
        summary: &'static str,
    ) -> Result<Self, DoctorError> {
        validate_static_text(summary, "summary")?;
        Ok(Self {
            status,
            code: DoctorCode::new(code)?,
            summary,
            repair: None,
            evidence: None,
        })
    }

    /// Attach compile-time-static repair guidance.
    ///
    /// # Errors
    /// Empty, untrimmed, oversized or control-bearing text fails loud.
    pub fn with_repair(mut self, repair: &'static str) -> Result<Self, DoctorError> {
        validate_static_text(repair, "repair")?;
        self.repair = Some(repair);
        Ok(self)
    }

    /// Attach the typed K08 composition report.
    #[must_use]
    pub fn with_composition(mut self, report: heycode_core::CompositionReport) -> Self {
        self.evidence = Some(DoctorEvidence::Composition { report });
        self
    }

    /// Attach typed redacted config-migration evidence.
    #[must_use]
    pub fn with_config_migration(mut self, report: ConfigMigrationEvidence) -> Self {
        self.evidence = Some(DoctorEvidence::ConfigMigration { report });
        self
    }

    pub(crate) fn cancelled() -> Self {
        Self {
            status: DoctorStatus::Skipped,
            code: DoctorCode("doctor.cancelled".to_owned()),
            summary: "Check skipped because doctor was cancelled.",
            repair: Some("Run doctor again when cancellation is cleared."),
            evidence: None,
        }
    }

    pub(crate) fn panicked() -> Self {
        Self {
            status: DoctorStatus::Failure,
            code: DoctorCode("doctor.check-panicked".to_owned()),
            summary: "A doctor check panicked and was contained.",
            repair: Some("Disable or update the owning plugin, then run doctor again."),
            evidence: None,
        }
    }

    pub(crate) fn invalid_contract() -> Self {
        Self {
            status: DoctorStatus::Failure,
            code: DoctorCode("doctor.invalid-check-contract".to_owned()),
            summary: "A doctor check returned an invalid structured outcome.",
            repair: Some("Disable or update the owning plugin, then run doctor again."),
            evidence: None,
        }
    }

    pub(crate) fn into_result(self, id: DoctorCheckId) -> DoctorCheckResult {
        DoctorCheckResult {
            id,
            status: self.status,
            code: self.code,
            summary: self.summary.to_owned(),
            repair: self.repair.map(str::to_owned),
            evidence: self.evidence,
        }
    }
}

/// One authoritative check result in a report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheckResult {
    /// Registry-owned check id.
    pub id: DoctorCheckId,
    /// Outcome severity.
    pub status: DoctorStatus,
    /// Stable machine code.
    pub code: DoctorCode,
    /// Compile-time-static safe summary copied into the owned report.
    pub summary: String,
    /// Optional compile-time-static safe repair guidance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repair: Option<String>,
    /// Optional typed runtime evidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<DoctorEvidence>,
}

/// Aggregate result counts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoctorSummary {
    /// Passing checks.
    pub passed: usize,
    /// Non-blocking warnings.
    pub warnings: usize,
    /// Failing checks.
    pub failed: usize,
    /// Checks not executed.
    pub skipped: usize,
}

/// Stable complete doctor report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    /// Wire schema version.
    pub schema_version: u32,
    /// True when no check failed or was skipped. Warnings remain healthy.
    pub healthy: bool,
    /// Aggregate counts.
    pub summary: DoctorSummary,
    /// Deterministic registration-order results.
    pub checks: Vec<DoctorCheckResult>,
}

impl DoctorReport {
    pub(crate) fn from_checks(checks: Vec<DoctorCheckResult>) -> Self {
        let mut summary = DoctorSummary::default();
        for check in &checks {
            match check.status {
                DoctorStatus::Pass => summary.passed += 1,
                DoctorStatus::Warning => summary.warnings += 1,
                DoctorStatus::Failure => summary.failed += 1,
                DoctorStatus::Skipped => summary.skipped += 1,
            }
        }
        Self {
            schema_version: DOCTOR_REPORT_SCHEMA_VERSION,
            healthy: summary.failed == 0 && summary.skipped == 0,
            summary,
            checks,
        }
    }

    /// Render a deterministic human form containing the same safe fields as JSON.
    #[must_use]
    pub fn render_human(&self) -> String {
        let mut lines = vec![format!(
            "doctor: {} ({} passed, {} warnings, {} failed, {} skipped)",
            if self.healthy { "healthy" } else { "unhealthy" },
            self.summary.passed,
            self.summary.warnings,
            self.summary.failed,
            self.summary.skipped
        )];
        for check in &self.checks {
            lines.push(format!(
                "[{}] {} ({}): {}",
                check.status.as_str(),
                check.id,
                check.code.as_str(),
                check.summary
            ));
            if let Some(repair) = &check.repair {
                lines.push(format!("  repair: {repair}"));
            }
            if let Some(DoctorEvidence::Composition { report }) = &check.evidence {
                lines.extend(
                    report
                        .render_human()
                        .lines()
                        .map(|line| format!("  {line}")),
                );
            }
            if let Some(DoctorEvidence::ConfigMigration { report }) = &check.evidence {
                lines.push(format!(
                    "  migration: {} -> {} path={}",
                    config_version_label(&report.from),
                    report.to,
                    report.path.escape_default()
                ));
                lines.push(format!(
                    "  disposition: {}",
                    match report.disposition {
                        ConfigMigrationDispositionEvidence::HomeApplied { .. } => "home_applied",
                        ConfigMigrationDispositionEvidence::UserOwnedPending => {
                            "user_owned_pending"
                        }
                    }
                ));
            }
        }
        lines.join("\n")
    }
}

fn config_version_label(version: &ConfigVersionEvidence) -> String {
    match version {
        ConfigVersionEvidence::Unversioned => "unversioned".to_owned(),
        ConfigVersionEvidence::Older { version } => format!("older:{version}"),
        ConfigVersionEvidence::Current { version } => format!("current:{version}"),
        ConfigVersionEvidence::Newer { version } => format!("newer:{version}"),
    }
}

fn validate_identifier(value: &str, kind: &'static str) -> Result<(), DoctorError> {
    let bytes = value.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(*byte, b'.' | b'-')
        })
        && !value.contains("..")
        && !value.contains("--")
        && !value.contains(".-")
        && !value.contains("-.");
    if valid {
        Ok(())
    } else {
        Err(DoctorError::InvalidIdentifier { kind })
    }
}

fn validate_static_text(value: &str, kind: &'static str) -> Result<(), DoctorError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        Err(DoctorError::InvalidStaticText { kind })
    } else {
        Ok(())
    }
}
