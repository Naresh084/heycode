//! Persisted skill visibility and catalog ordering.

use std::collections::BTreeSet;
use std::sync::{Arc, RwLock};

use heycode_settings::{
    SettingsDefinition, SettingsError, SettingsNamespace, SettingsSchema, SettingsService,
    SettingsSnapshot,
};

use crate::SkillRecord;

const MAX_DISABLED_SKILLS: usize = 4_096;
const MAX_SKILL_NAME_BYTES: usize = 128;

/// Stable user-selected ordering for the skills catalog.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SkillSort {
    /// Compare canonical skill names.
    #[default]
    Name,
    /// Compare estimated catalog token cost, largest first, then names.
    Tokens,
    /// Group by admitted source, then compare canonical names.
    Source,
}

/// Effective human/model admission selected for one skill.
///
/// Source-declared `disable-model-invocation` remains a ceiling: such a skill
/// can only be [`Self::UserOnly`] or [`Self::Off`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAdmission {
    /// The model sees the name and description; model and human may load it.
    On,
    /// The model sees only the name; model and human may still load it.
    NameOnly,
    /// The model cannot discover or load it; explicit `/skill` remains allowed.
    UserOnly,
    /// Neither model nor human invocation is admitted.
    Off,
}

impl SkillAdmission {
    /// Stable UI and Settings label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::On => "on",
            Self::NameOnly => "name-only",
            Self::UserOnly => "user-only",
            Self::Off => "off",
        }
    }

    /// Claude-compatible cycle for a model-invocable skill.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::On => Self::NameOnly,
            Self::NameOnly => Self::UserOnly,
            Self::UserOnly => Self::Off,
            Self::Off => Self::On,
        }
    }

    /// Whether explicit human invocation remains admitted.
    #[must_use]
    pub const fn enabled(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Whether the model may discover and load the skill.
    #[must_use]
    pub const fn model_invocable(self) -> bool {
        matches!(self, Self::On | Self::NameOnly)
    }

    /// Whether the model catalog may include the description.
    #[must_use]
    pub const fn description_visible(self) -> bool {
        matches!(self, Self::On)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct AdmissionOverrides {
    pub(crate) name_only: BTreeSet<String>,
    pub(crate) user_only: BTreeSet<String>,
    pub(crate) disabled: BTreeSet<String>,
}

impl AdmissionOverrides {
    pub(crate) fn remove_identities<'a>(&mut self, names: impl IntoIterator<Item = &'a str>) {
        for name in names {
            self.name_only.remove(name);
            self.user_only.remove(name);
            self.disabled.remove(name);
        }
    }

    pub(crate) fn insert(&mut self, name: String, admission: SkillAdmission) {
        match admission {
            SkillAdmission::On => {}
            SkillAdmission::NameOnly => {
                self.name_only.insert(name);
            }
            SkillAdmission::UserOnly => {
                self.user_only.insert(name);
            }
            SkillAdmission::Off => {
                self.disabled.insert(name);
            }
        }
    }
}

impl SkillSort {
    /// Stable Settings value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Tokens => "tokens",
            Self::Source => "source",
        }
    }

    /// The next ordering selected by Claude's `t` binding.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Name => Self::Tokens,
            Self::Tokens | Self::Source => Self::Name,
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "name" => Ok(Self::Name),
            "tokens" => Ok(Self::Tokens),
            "source" => Ok(Self::Source),
            _ => Err("sort must be `name`, `tokens`, or `source`".to_owned()),
        }
    }
}

/// One source-attributed skill and its effective persisted admission state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillCatalogRecord {
    record: SkillRecord,
    admission: SkillAdmission,
}

impl SkillCatalogRecord {
    pub(crate) const fn new(record: SkillRecord, admission: SkillAdmission) -> Self {
        Self { record, admission }
    }

    /// Immutable skill and source metadata.
    #[must_use]
    pub const fn record(&self) -> &SkillRecord {
        &self.record
    }

    /// Estimated full catalog entry cost, independent of its admission state.
    /// Uses the same character estimate displayed by the interactive catalog;
    /// this is not a provider tokenizer measurement or retained-context total.
    #[must_use]
    pub fn estimated_catalog_tokens(&self) -> usize {
        self.record
            .skill
            .name
            .chars()
            .count()
            .saturating_add(self.record.skill.description.chars().count())
            .saturating_add(5)
            .div_ceil(4)
    }

    /// Whether human and model dispatch may currently admit this skill.
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.admission.enabled()
    }

    /// Current four-state admission used by the catalog and dispatch points.
    #[must_use]
    pub const fn admission(&self) -> SkillAdmission {
        self.admission
    }

    /// Whether this row may enter a model request without explicit human use.
    #[must_use]
    pub const fn model_invocable(&self) -> bool {
        self.admission.model_invocable()
    }
}

/// Immutable catalog plus both generations required for safe mutations.
///
/// Callers pass this exact value back to [`crate::SkillSet::set_enabled`] or
/// [`crate::SkillSet::set_sort`]. The embedded Settings snapshot is deliberately
/// opaque so a same-revision provider reload still makes the mutation stale.
#[derive(Clone)]
pub struct SkillCatalogSnapshot {
    pub(crate) records: Vec<SkillCatalogRecord>,
    pub(crate) generation: u64,
    pub(crate) sort: SkillSort,
    pub(crate) admission_overrides: AdmissionOverrides,
    pub(crate) settings_snapshot: Option<Arc<SettingsSnapshot>>,
}

impl std::fmt::Debug for SkillCatalogSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SkillCatalogSnapshot")
            .field("records", &self.records)
            .field("generation", &self.generation)
            .field("sort", &self.sort)
            .field("name_only_count", &self.admission_overrides.name_only.len())
            .field("user_only_count", &self.admission_overrides.user_only.len())
            .field("disabled_count", &self.admission_overrides.disabled.len())
            .field(
                "settings_revision",
                &self.settings_snapshot.as_ref().map(|row| row.revision()),
            )
            .finish()
    }
}

impl SkillCatalogSnapshot {
    /// Current source-attributed rows in the persisted ordering.
    #[must_use]
    pub fn records(&self) -> &[SkillCatalogRecord] {
        &self.records
    }

    /// Exact successful skill-registry generation.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Persisted catalog ordering.
    #[must_use]
    pub const fn sort(&self) -> SkillSort {
        self.sort
    }

    /// Exact raw user-section revision, when the registry is Settings-backed.
    #[must_use]
    pub fn settings_revision(&self) -> Option<u64> {
        self.settings_snapshot.as_ref().map(|row| row.revision())
    }
}

/// Persisted preference mutation failures. No skill body or settings value is
/// included in diagnostics.
#[derive(Debug, thiserror::Error)]
pub enum SkillPreferenceError {
    /// The skill registry changed after the panel captured its rows.
    #[error("skills changed since this view opened; reopen /skills and try again")]
    StaleGeneration,
    /// The selected row is no longer present in the expected generation.
    #[error("skill `{name}` is no longer available")]
    UnknownSkill {
        /// Safe selected name.
        name: String,
    },
    /// A source-declared user-only skill cannot be widened by preferences.
    #[error("skill `{name}` is restricted to user-only invocation by its source")]
    SourceRestricted {
        /// Canonical safe name.
        name: String,
    },
    /// The registry was not composed with a durable Settings provider.
    #[error("skill preferences are unavailable because settings are not writable")]
    PersistenceUnavailable,
    /// Registry synchronization failed.
    #[error("skill preferences are unavailable")]
    Unavailable,
    /// Settings rejected or could not persist the complete preference section.
    #[error("skill preferences were not changed: {message}")]
    Settings {
        /// Safe Settings diagnostic.
        message: String,
    },
}

#[derive(Clone)]
pub(crate) struct PreferencesBinding {
    pub(crate) settings: Arc<SettingsService>,
    pub(crate) namespace: SettingsNamespace,
}

#[derive(Default)]
pub(crate) struct PreferenceState {
    pub(crate) admission_overrides: AdmissionOverrides,
    pub(crate) sort: SkillSort,
    pub(crate) settings_snapshot: Option<Arc<SettingsSnapshot>>,
}

pub(crate) fn detached_preferences() -> Arc<RwLock<PreferenceState>> {
    Arc::new(RwLock::new(PreferenceState::default()))
}

/// Canonical live namespace for skill admission and list ordering.
///
/// # Errors
/// Static namespace validation failure.
pub fn settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new("skills-preferences")
}

/// Settings definition for persisted skill choices.
///
/// # Errors
/// Static schema validation failure.
pub fn settings_definition() -> Result<SettingsDefinition, SettingsError> {
    let schema = SettingsSchema::new(
        serde_json::json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "disabled": {
                    "type": "array",
                    "maxItems": MAX_DISABLED_SKILLS,
                    "uniqueItems": true,
                    "items": {"type": "string", "maxLength": MAX_SKILL_NAME_BYTES}
                },
                "name_only": {
                    "type": "array",
                    "maxItems": MAX_DISABLED_SKILLS,
                    "uniqueItems": true,
                    "items": {"type": "string", "maxLength": MAX_SKILL_NAME_BYTES}
                },
                "user_only": {
                    "type": "array",
                    "maxItems": MAX_DISABLED_SKILLS,
                    "uniqueItems": true,
                    "items": {"type": "string", "maxLength": MAX_SKILL_NAME_BYTES}
                },
                "sort": {"type": "string", "enum": ["name", "tokens", "source"]}
            }
        }),
        serde_json::json!({
            "disabled": [],
            "name_only": [],
            "user_only": [],
            "sort": "name"
        }),
        validate,
    )?
    .with_wire_exposure();
    Ok(SettingsDefinition::new(settings_namespace()?, schema))
}

pub(crate) fn parse_snapshot(
    snapshot: Arc<SettingsSnapshot>,
) -> Result<PreferenceState, SkillPreferenceError> {
    let (admission_overrides, sort) =
        parse_value(snapshot.resolved()).map_err(|message| SkillPreferenceError::Settings {
            message: format!("invalid resolved settings: {message}"),
        })?;
    Ok(PreferenceState {
        admission_overrides,
        sort,
        settings_snapshot: Some(snapshot),
    })
}

pub(crate) fn parse_value(
    value: &serde_json::Value,
) -> Result<(AdmissionOverrides, SkillSort), String> {
    validate(value)?;
    let admission_overrides = AdmissionOverrides {
        name_only: names(value, "name_only")?,
        user_only: names(value, "user_only")?,
        disabled: names(value, "disabled")?,
    };
    let sort = SkillSort::parse(
        value
            .get("sort")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "sort must be a string".to_owned())?,
    )?;
    Ok((admission_overrides, sort))
}

pub(crate) fn user_value(admission: &AdmissionOverrides, sort: SkillSort) -> serde_json::Value {
    serde_json::json!({
        "disabled": admission.disabled.iter().collect::<Vec<_>>(),
        "name_only": admission.name_only.iter().collect::<Vec<_>>(),
        "user_only": admission.user_only.iter().collect::<Vec<_>>(),
        "sort": sort.as_str()
    })
}

fn validate(value: &serde_json::Value) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "skills preferences must be an object".to_owned())?;
    if object
        .keys()
        .any(|key| key != "disabled" && key != "name_only" && key != "user_only" && key != "sort")
    {
        return Err("skills preferences contain an unknown field".to_owned());
    }
    let fields = ["disabled", "name_only", "user_only"];
    let mut all = BTreeSet::new();
    let mut total = 0_usize;
    for field in fields {
        let values = object
            .get(field)
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| format!("{field} must be an array"))?;
        total = total.saturating_add(values.len());
        let mut unique = BTreeSet::new();
        for name in values {
            let name = name
                .as_str()
                .ok_or_else(|| format!("{field} names must be strings"))?;
            if !valid_name(name) {
                return Err(format!("{field} contains an invalid skill name"));
            }
            if !unique.insert(name) {
                return Err(format!("{field} names must be unique"));
            }
            if !all.insert(name) {
                return Err("a skill name cannot have multiple preference states".to_owned());
            }
        }
    }
    if total > MAX_DISABLED_SKILLS {
        return Err(format!(
            "skill state overrides contain more than {MAX_DISABLED_SKILLS} names"
        ));
    }
    SkillSort::parse(
        object
            .get("sort")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| "sort must be a string".to_owned())?,
    )?;
    Ok(())
}

fn names(value: &serde_json::Value, field: &str) -> Result<BTreeSet<String>, String> {
    value
        .get(field)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("{field} must be an array"))?
        .iter()
        .map(|name| {
            name.as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("{field} names must be strings"))
        })
        .collect()
}

fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_SKILL_NAME_BYTES
        && name.trim() == name
        && !name.chars().any(char::is_control)
        && !name.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::validate;

    #[test]
    fn admission_override_lists_must_be_pairwise_disjoint() {
        let overlapping = serde_json::json!({
            "disabled": ["review"],
            "name_only": ["review"],
            "user_only": [],
            "sort": "name"
        });

        assert_eq!(
            validate(&overlapping).err().as_deref(),
            Some("a skill name cannot have multiple preference states")
        );
    }
}
