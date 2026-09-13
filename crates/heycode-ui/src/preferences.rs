//! Transactional terminal presentation preferences for CMD10.

use std::sync::Arc;

use heycode_settings::{SettingsError, SettingsNamespace, SettingsService};

use crate::UiContributionId;
use crate::theme::DEFAULT_THEME_ID;

/// Vertical density of the persistent shell header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HeaderDensity {
    /// App/backend identity on the first row and the workspace on the second.
    #[default]
    Full,
    /// App, backend and shortened workspace on one row.
    Compact,
}

impl HeaderDensity {
    /// Stable Settings value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full => "full",
            Self::Compact => "compact",
        }
    }

    fn parse(value: &str) -> Result<Self, UiPreferencesError> {
        match value {
            "full" => Ok(Self::Full),
            "compact" => Ok(Self::Compact),
            _ => Err(UiPreferencesError::InvalidHeaderDensity),
        }
    }
}

/// Settings-backed shell chrome that is independent of theme and keymap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShellChromePreferences {
    header_density: HeaderDensity,
    header_pet: bool,
    footer_status: bool,
    footer_hints: bool,
}

impl ShellChromePreferences {
    /// Construct the complete closed shell preference set.
    #[must_use]
    pub const fn new(
        header_density: HeaderDensity,
        footer_status: bool,
        footer_hints: bool,
    ) -> Self {
        Self {
            header_density,
            header_pet: true,
            footer_status,
            footer_hints,
        }
    }

    /// Full two-row or compact one-row header.
    #[must_use]
    pub const fn header_density(self) -> HeaderDensity {
        self.header_density
    }

    /// Whether the compact terminal pet is shown beside the header identity.
    #[must_use]
    pub const fn header_pet(self) -> bool {
        self.header_pet
    }

    /// Replace the optional header pet choice.
    #[must_use]
    pub const fn with_header_pet(mut self, header_pet: bool) -> Self {
        self.header_pet = header_pet;
        self
    }

    /// Replace status-row visibility without resetting other shell choices.
    #[must_use]
    pub const fn with_footer_status(mut self, footer_status: bool) -> Self {
        self.footer_status = footer_status;
        self
    }

    /// Replace command-hint visibility without resetting other shell choices.
    #[must_use]
    pub const fn with_footer_hints(mut self, footer_hints: bool) -> Self {
        self.footer_hints = footer_hints;
        self
    }

    /// Whether the operational status row is visible below the composer.
    #[must_use]
    pub const fn footer_status(self) -> bool {
        self.footer_status
    }

    /// Whether the contextual command-hint row is visible below status.
    #[must_use]
    pub const fn footer_hints(self) -> bool {
        self.footer_hints
    }
}

impl Default for ShellChromePreferences {
    fn default() -> Self {
        Self::new(HeaderDensity::Full, true, true)
    }
}

/// Validated UI preferences resolved from the layered Settings service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiPreferences {
    theme_id: String,
    vim_mode: bool,
    focus_view: bool,
    shell: ShellChromePreferences,
    scroll_speed_quarters: u8,
    copy_full_response: bool,
}

impl UiPreferences {
    /// Validate one theme selection and Vim-mode state.
    ///
    /// Theme existence is checked against the live [`crate::UiRegistry`] by
    /// the Consumer; this boundary only admits a safe contribution id so a
    /// contributed theme can be selected without freezing today's catalog.
    ///
    /// # Errors
    /// Invalid theme contribution ids fail before Settings mutation.
    pub fn new(theme_id: impl Into<String>, vim_mode: bool) -> Result<Self, UiPreferencesError> {
        let theme_id = theme_id.into();
        UiContributionId::new(theme_id.clone()).map_err(|_| UiPreferencesError::InvalidThemeId)?;
        Ok(Self {
            theme_id,
            vim_mode,
            focus_view: false,
            shell: ShellChromePreferences::default(),
            scroll_speed_quarters: 4,
            copy_full_response: false,
        })
    }

    /// Selected live theme id.
    #[must_use]
    pub fn theme_id(&self) -> &str {
        &self.theme_id
    }

    /// Whether the composer uses Vim insert/normal behavior.
    #[must_use]
    pub const fn vim_mode(&self) -> bool {
        self.vim_mode
    }

    /// Whether the terminal starts in its compact current-turn transcript.
    #[must_use]
    pub const fn focus_view(&self) -> bool {
        self.focus_view
    }

    /// Whether `/copy` skips code-block selection and copies the complete answer.
    #[must_use]
    pub const fn copy_full_response(&self) -> bool {
        self.copy_full_response
    }

    /// Replace only the persisted full-response copy preference.
    #[must_use]
    pub const fn with_copy_full_response(mut self, enabled: bool) -> Self {
        self.copy_full_response = enabled;
        self
    }

    /// Persistent header/footer choices.
    #[must_use]
    pub const fn shell(&self) -> ShellChromePreferences {
        self.shell
    }

    /// Mouse-wheel multiplier applied by the fullscreen transcript.
    #[must_use]
    pub const fn scroll_speed(&self) -> f32 {
        self.scroll_speed_quarters as f32 / 4.0
    }

    /// Replace the shell choices while preserving theme and composer mode.
    #[must_use]
    pub const fn with_shell(mut self, shell: ShellChromePreferences) -> Self {
        self.shell = shell;
        self
    }

    /// Replace the validated mouse-wheel multiplier.
    ///
    /// # Errors
    /// Values outside the interactive 0.25 through 10 range are refused.
    pub fn with_scroll_speed(mut self, scroll_speed: f32) -> Result<Self, UiPreferencesError> {
        let quarters = scroll_speed * 4.0;
        if !scroll_speed.is_finite()
            || !(0.25..=10.0).contains(&scroll_speed)
            || quarters.fract() != 0.0
        {
            return Err(UiPreferencesError::InvalidScrollSpeed);
        }
        self.scroll_speed_quarters = quarters as u8;
        Ok(self)
    }

    fn with_theme_id(mut self, theme_id: String) -> Result<Self, UiPreferencesError> {
        UiContributionId::new(theme_id.clone()).map_err(|_| UiPreferencesError::InvalidThemeId)?;
        self.theme_id = theme_id;
        Ok(self)
    }

    const fn with_vim_mode(mut self, vim_mode: bool) -> Self {
        self.vim_mode = vim_mode;
        self
    }

    const fn with_focus_view(mut self, focus_view: bool) -> Self {
        self.focus_view = focus_view;
        self
    }
}

/// One preference snapshot paired with the exact CAS revision that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VersionedUiPreferences {
    /// Validated resolved values.
    pub preferences: UiPreferences,
    /// Exact Settings user-section revision.
    pub revision: u64,
}

/// UI preference load/commit failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UiPreferencesError {
    /// Theme id is not a safe UI contribution id.
    #[error("invalid theme id")]
    InvalidThemeId,
    /// Header density is outside the closed Settings vocabulary.
    #[error("header density must be `full` or `compact`")]
    InvalidHeaderDensity,
    /// Mouse wheel multiplier is outside the interactive range.
    #[error("scroll speed must be between 0.25 and 10 in 0.25 increments")]
    InvalidScrollSpeed,
    /// Settings refused the operation.
    #[error("UI preference settings unavailable: {message}")]
    Settings {
        /// Safe Settings diagnostic.
        message: String,
    },
}

/// Settings namespace for theme/Vim preferences.
///
/// # Errors
/// Static namespace validation failure.
pub fn settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new("ui-preferences")
}

/// Settings definition for live UI preferences.
///
/// # Errors
/// Static schema validation failure.
pub fn settings_definition() -> Result<heycode_settings::SettingsDefinition, SettingsError> {
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type": "object",
            "properties": {
                "theme": {"type": "string"},
                "vim_mode": {"type": "boolean"},
                "focus_view": {"type": "boolean"},
                "copy_full_response": {"type": "boolean"},
                "header_density": {"type": "string", "enum": ["full", "compact"]},
                "header_pet": {"type": "boolean"},
                "footer_status": {"type": "boolean"},
                "footer_hints": {"type": "boolean"},
                "scroll_speed": {"type": "number", "minimum": 0.25, "maximum": 10.0}
            }
        }),
        serde_json::json!({
            "theme": DEFAULT_THEME_ID,
            "vim_mode": false,
            "focus_view": false,
            "copy_full_response": false,
            "header_density": "full",
            "header_pet": true,
            "footer_status": true,
            "footer_hints": true,
            "scroll_speed": 1.0
        }),
        validate,
    )?
    .with_wire_exposure();
    Ok(heycode_settings::SettingsDefinition::new(
        settings_namespace()?,
        schema,
    ))
}

fn validate(value: &serde_json::Value) -> Result<(), String> {
    let theme = value
        .get("theme")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "theme must be a string".to_owned())?;
    let vim_mode = value
        .get("vim_mode")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "vim_mode must be a boolean".to_owned())?;
    let focus_view = value
        .get("focus_view")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "focus_view must be a boolean".to_owned())?;
    let copy_full_response = value
        .get("copy_full_response")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| "copy_full_response must be a boolean".to_owned())?;
    let shell = shell_from_value(value).map_err(|error| error.to_string())?;
    let scroll_speed = value
        .get("scroll_speed")
        .and_then(serde_json::Value::as_f64)
        .ok_or_else(|| "scroll_speed must be a number".to_owned())? as f32;
    UiPreferences::new(theme, vim_mode)
        .map(|preferences| {
            preferences
                .with_focus_view(focus_view)
                .with_copy_full_response(copy_full_response)
        })
        .and_then(|preferences| preferences.with_scroll_speed(scroll_speed))
        .map(|preferences| preferences.with_shell(shell))
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn shell_from_value(
    value: &serde_json::Value,
) -> Result<ShellChromePreferences, UiPreferencesError> {
    let header_density = value
        .get("header_density")
        .and_then(serde_json::Value::as_str)
        .ok_or(UiPreferencesError::InvalidHeaderDensity)
        .and_then(HeaderDensity::parse)?;
    let footer_status = value
        .get("footer_status")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| UiPreferencesError::Settings {
            message: "footer_status must be a boolean".to_owned(),
        })?;
    let header_pet = value
        .get("header_pet")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| UiPreferencesError::Settings {
            message: "header_pet must be a boolean".to_owned(),
        })?;
    let footer_hints = value
        .get("footer_hints")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| UiPreferencesError::Settings {
            message: "footer_hints must be a boolean".to_owned(),
        })?;
    Ok(
        ShellChromePreferences::new(header_density, footer_status, footer_hints)
            .with_header_pet(header_pet),
    )
}

/// Read and commit UI preferences through exact Settings revisions.
pub struct SettingsBackedUiPreferences {
    settings: Arc<SettingsService>,
}

impl std::fmt::Debug for SettingsBackedUiPreferences {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SettingsBackedUiPreferences")
    }
}

impl SettingsBackedUiPreferences {
    /// Bind to the composed Settings service.
    #[must_use]
    pub const fn new(settings: Arc<SettingsService>) -> Self {
        Self { settings }
    }

    /// Load the authoritative resolved values and exact user revision.
    ///
    /// # Errors
    /// Missing/malformed namespaces and Settings failures fail loud.
    pub fn load(&self) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let namespace = settings_namespace().map_err(settings_error)?;
        let snapshot = self
            .settings
            .get(&namespace)
            .map_err(settings_error)?
            .ok_or_else(|| UiPreferencesError::Settings {
                message: "namespace is not registered".to_owned(),
            })?;
        let theme = snapshot
            .resolved()
            .get("theme")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| UiPreferencesError::Settings {
                message: "theme is unavailable".to_owned(),
            })?;
        let vim_mode = snapshot
            .resolved()
            .get("vim_mode")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| UiPreferencesError::Settings {
                message: "vim_mode is unavailable".to_owned(),
            })?;
        let focus_view = snapshot
            .resolved()
            .get("focus_view")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| UiPreferencesError::Settings {
                message: "focus_view is unavailable".to_owned(),
            })?;
        let copy_full_response = snapshot
            .resolved()
            .get("copy_full_response")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| UiPreferencesError::Settings {
                message: "copy_full_response is unavailable".to_owned(),
            })?;
        let shell = shell_from_value(snapshot.resolved())?;
        let scroll_speed = snapshot
            .resolved()
            .get("scroll_speed")
            .and_then(serde_json::Value::as_f64)
            .ok_or_else(|| UiPreferencesError::Settings {
                message: "scroll_speed is unavailable".to_owned(),
            })? as f32;
        Ok(VersionedUiPreferences {
            preferences: UiPreferences::new(theme, vim_mode)?
                .with_focus_view(focus_view)
                .with_copy_full_response(copy_full_response)
                .with_scroll_speed(scroll_speed)?
                .with_shell(shell),
            revision: snapshot.revision(),
        })
    }

    /// Persist one complete user section at an exact previously loaded revision.
    ///
    /// # Errors
    /// A stale revision, invalid preference or persistence failure publishes
    /// no new state.
    pub fn store(
        &self,
        preferences: &UiPreferences,
        expected_revision: u64,
    ) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let namespace = settings_namespace().map_err(settings_error)?;
        self.settings
            .replace_user(
                &namespace,
                serde_json::json!({
                    "theme": preferences.theme_id(),
                    "vim_mode": preferences.vim_mode(),
                    "focus_view": preferences.focus_view(),
                    "copy_full_response": preferences.copy_full_response(),
                    "header_density": preferences.shell().header_density().as_str(),
                    "header_pet": preferences.shell().header_pet(),
                    "footer_status": preferences.shell().footer_status(),
                    "footer_hints": preferences.shell().footer_hints(),
                    "scroll_speed": preferences.scroll_speed()
                }),
                Some(expected_revision),
            )
            .map_err(settings_error)?;
        self.load()
    }

    /// Persist a theme without resetting header/footer choices.
    ///
    /// # Errors
    /// Invalid ids, stale revisions and Settings failures publish no state.
    pub fn store_theme(
        &self,
        theme_id: impl Into<String>,
        expected_revision: u64,
    ) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let current = self.load()?;
        if current.revision != expected_revision {
            return self.store(&current.preferences, expected_revision);
        }
        let next = current.preferences.with_theme_id(theme_id.into())?;
        self.store(&next, expected_revision)
    }

    /// Persist Vim mode without resetting header/footer choices.
    ///
    /// # Errors
    /// Stale revisions and Settings failures publish no state.
    pub fn store_vim_mode(
        &self,
        vim_mode: bool,
        expected_revision: u64,
    ) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let current = self.load()?;
        if current.revision != expected_revision {
            return self.store(&current.preferences, expected_revision);
        }
        self.store(
            &current.preferences.with_vim_mode(vim_mode),
            expected_revision,
        )
    }

    /// Persist focus view without resetting the other terminal preferences.
    ///
    /// # Errors
    /// Stale revisions and Settings failures publish no state.
    pub fn store_focus_view(
        &self,
        focus_view: bool,
        expected_revision: u64,
    ) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let current = self.load()?;
        if current.revision != expected_revision {
            return self.store(&current.preferences, expected_revision);
        }
        self.store(
            &current.preferences.with_focus_view(focus_view),
            expected_revision,
        )
    }

    /// Persist shell footer choices without resetting theme, Vim, or wheel speed.
    ///
    /// # Errors
    /// Stale revisions and Settings failures publish no state.
    pub fn store_shell(
        &self,
        shell: ShellChromePreferences,
        expected_revision: u64,
    ) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let current = self.load()?;
        if current.revision != expected_revision {
            return self.store(&current.preferences, expected_revision);
        }
        self.store(&current.preferences.with_shell(shell), expected_revision)
    }

    /// Persist the validated mouse-wheel multiplier without resetting other choices.
    ///
    /// # Errors
    /// Invalid speed, stale revisions and Settings failures publish no state.
    pub fn store_scroll_speed(
        &self,
        scroll_speed: f32,
        expected_revision: u64,
    ) -> Result<VersionedUiPreferences, UiPreferencesError> {
        let current = self.load()?;
        if current.revision != expected_revision {
            return self.store(&current.preferences, expected_revision);
        }
        let next = current.preferences.with_scroll_speed(scroll_speed)?;
        self.store(&next, expected_revision)
    }
}

fn settings_error(error: impl ToString) -> UiPreferencesError {
    UiPreferencesError::Settings {
        message: error.to_string(),
    }
}
