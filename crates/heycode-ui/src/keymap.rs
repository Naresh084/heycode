//! U20 keymap: named actions, canonical chords, and durable overrides.
//!
//! A keymap is a value. The chord grammar, the default bindings, the conflict
//! rule and the persisted document shape all live here, in a crate that knows
//! nothing about crossterm — so every rule is assertable without a terminal and
//! `heycode-tui` only has to translate its own key events into a [`KeyChord`].
//!
//! ## Two rules worth stating plainly
//!
//! **A conflict is reported, never resolved.** Two actions on one chord is a
//! user error: whichever the implementation happened to pick, the other action
//! silently stops working. [`Keymap::resolve`] returns
//! [`KeymapError::Conflict`] naming the chord and both actions. There is
//! deliberately no precedence rule for a caller to rely on.
//!
//! **A chord has exactly one spelling.** `Ctrl+P`, `ctrl+p` and `CTRL+P` parse
//! to the same [`KeyChord`], which renders back as `ctrl+p`. Without that, one
//! binding could occupy two map entries and a conflict would go undetected.

use std::collections::BTreeMap;

use heycode_settings::{SettingsError, SettingsNamespace};

/// A key without its modifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyName {
    /// A printable character. Stored lowercase for ASCII letters.
    Char(char),
    /// Return/Enter.
    Enter,
    /// Escape.
    Esc,
    /// Tab.
    Tab,
    /// Shift-Tab as reported by terminals that distinguish it.
    BackTab,
    /// Backspace.
    Backspace,
    /// Forward delete.
    Delete,
    /// Insert.
    Insert,
    /// Cursor up.
    Up,
    /// Cursor down.
    Down,
    /// Cursor left.
    Left,
    /// Cursor right.
    Right,
    /// Home.
    Home,
    /// End.
    End,
    /// Page up.
    PageUp,
    /// Page down.
    PageDown,
    /// Function key `n`, 1–12.
    Function(u8),
}

/// Highest function key the grammar accepts.
pub const MAX_FUNCTION_KEY: u8 = 12;

/// The spelling used for `'+'`, which cannot be written literally because `+`
/// separates modifiers.
pub const PLUS_KEY_NAME: &str = "plus";

/// The spelling used for `' '`.
pub const SPACE_KEY_NAME: &str = "space";

const NAMED_KEYS: [(&str, KeyName); 15] = [
    ("enter", KeyName::Enter),
    ("esc", KeyName::Esc),
    ("tab", KeyName::Tab),
    ("backtab", KeyName::BackTab),
    ("backspace", KeyName::Backspace),
    ("delete", KeyName::Delete),
    ("insert", KeyName::Insert),
    ("up", KeyName::Up),
    ("down", KeyName::Down),
    ("left", KeyName::Left),
    ("right", KeyName::Right),
    ("home", KeyName::Home),
    ("end", KeyName::End),
    ("pageup", KeyName::PageUp),
    ("pagedown", KeyName::PageDown),
];

impl std::fmt::Display for KeyName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Char('+') => formatter.write_str(PLUS_KEY_NAME),
            Self::Char(' ') => formatter.write_str(SPACE_KEY_NAME),
            Self::Char(character) => write!(formatter, "{character}"),
            Self::Function(number) => write!(formatter, "f{number}"),
            other => {
                let name = NAMED_KEYS
                    .iter()
                    .find(|(_, key)| key == other)
                    .map_or("", |(name, _)| name);
                formatter.write_str(name)
            }
        }
    }
}

/// Modifier flags in canonical order.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Modifiers {
    /// Control.
    pub ctrl: bool,
    /// Alt/Meta.
    pub alt: bool,
    /// Shift.
    pub shift: bool,
}

impl Modifiers {
    /// No modifiers.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// Control only.
    #[must_use]
    pub const fn ctrl() -> Self {
        Self {
            ctrl: true,
            alt: false,
            shift: false,
        }
    }

    /// Alt only.
    #[must_use]
    pub const fn alt() -> Self {
        Self {
            ctrl: false,
            alt: true,
            shift: false,
        }
    }
}

/// One key plus its modifiers, with exactly one spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct KeyChord {
    key: KeyName,
    modifiers: Modifiers,
}

impl KeyChord {
    /// Build a chord, normalizing an uppercase ASCII letter into
    /// lowercase-plus-shift so the two spellings cannot both exist.
    #[must_use]
    pub const fn new(key: KeyName, modifiers: Modifiers) -> Self {
        let (key, modifiers) = match key {
            KeyName::Char(character) if character.is_ascii_uppercase() => (
                KeyName::Char(character.to_ascii_lowercase()),
                Modifiers {
                    ctrl: modifiers.ctrl,
                    alt: modifiers.alt,
                    shift: true,
                },
            ),
            other => (other, modifiers),
        };
        Self { key, modifiers }
    }

    /// The key.
    #[must_use]
    pub const fn key(&self) -> KeyName {
        self.key
    }

    /// The modifiers.
    #[must_use]
    pub const fn modifiers(&self) -> Modifiers {
        self.modifiers
    }

    /// Parse the canonical `[ctrl+][alt+][shift+]<key>` grammar.
    ///
    /// Modifier names are case-insensitive and may appear in any order; the
    /// result always renders back in canonical order.
    ///
    /// # Errors
    /// [`KeymapError::InvalidChord`] for an unknown key name, an unknown or
    /// repeated modifier, an out-of-range function key, or empty text.
    pub fn parse(text: &str) -> Result<Self, KeymapError> {
        let invalid = || KeymapError::InvalidChord {
            chord: text.to_owned(),
        };
        // No separate empty/whitespace guard: empty text yields an empty key
        // segment and leading or trailing space makes a segment match neither
        // a modifier name nor a key name, so both already fail below. A second
        // check that cannot change any outcome is a rule no test can
        // distinguish (GOTCHAS #160) — a mutation removing it survived, which
        // is how it was found.
        let mut modifiers = Modifiers::none();
        let mut segments = text.split('+').peekable();
        let mut key_text = None;
        while let Some(segment) = segments.next() {
            let lowered = segment.to_ascii_lowercase();
            if segments.peek().is_none() {
                key_text = Some(segment.to_owned());
                break;
            }
            let slot = match lowered.as_str() {
                "ctrl" => &mut modifiers.ctrl,
                "alt" => &mut modifiers.alt,
                "shift" => &mut modifiers.shift,
                _ => return Err(invalid()),
            };
            if *slot {
                return Err(invalid());
            }
            *slot = true;
        }
        let key_text = key_text.ok_or_else(invalid)?;
        let key = parse_key_name(&key_text).ok_or_else(invalid)?;
        Ok(Self::new(key, modifiers))
    }
}

fn parse_key_name(text: &str) -> Option<KeyName> {
    let lowered = text.to_ascii_lowercase();
    if lowered == PLUS_KEY_NAME {
        return Some(KeyName::Char('+'));
    }
    if lowered == SPACE_KEY_NAME {
        return Some(KeyName::Char(' '));
    }
    if let Some((_, key)) = NAMED_KEYS.iter().find(|(name, _)| *name == lowered) {
        return Some(*key);
    }
    if let Some(digits) = lowered.strip_prefix('f')
        && !digits.is_empty()
        && digits.chars().all(|character| character.is_ascii_digit())
    {
        let number: u8 = digits.parse().ok()?;
        return (1..=MAX_FUNCTION_KEY)
            .contains(&number)
            .then_some(KeyName::Function(number));
    }
    let mut characters = text.chars();
    let character = characters.next()?;
    if characters.next().is_some() || character.is_control() || character.is_whitespace() {
        return None;
    }
    Some(KeyName::Char(character))
}

impl std::fmt::Display for KeyChord {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.modifiers.ctrl {
            formatter.write_str("ctrl+")?;
        }
        if self.modifiers.alt {
            formatter.write_str("alt+")?;
        }
        if self.modifiers.shift {
            formatter.write_str("shift+")?;
        }
        write!(formatter, "{}", self.key)
    }
}

/// Something the terminal UI can be asked to do with one keystroke.
///
/// Closed: adding a thirteenth action must stop [`KeymapAction::default_chord`]
/// and [`KeymapAction::as_str`] from compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeymapAction {
    /// Send the composer contents.
    Submit,
    /// Queue the composer contents as a follow-up after active work.
    QueueFollowUp,
    /// Interrupt the active turn.
    Interrupt,
    /// Quit the session.
    Quit,
    /// Toggle reasoning visibility.
    ToggleReasoning,
    /// Show or hide the detailed transcript, including retained tool output.
    ToggleTranscript,
    /// Open the command palette.
    CommandPalette,
    /// Cycle Diff → Jobs → Agents → closed side panels.
    CycleSidePanel,
    /// Expand or collapse the persistent task console.
    ToggleTasks,
    /// Open or collapse the native workflow workspace.
    ToggleWorkflows,
    /// Scroll a half page towards older content.
    ScrollHalfPageUp,
    /// Scroll a half page towards newer content.
    ScrollHalfPageDown,
    /// Scroll a full page towards older content.
    ScrollPageUp,
    /// Scroll a full page towards newer content.
    ScrollPageDown,
    /// Jump to the oldest content.
    ScrollToOldest,
    /// Jump to the newest content.
    ScrollToNewest,
    /// Insert a line break into the composer without sending.
    InsertNewline,
}

/// Number of [`KeymapAction`] variants.
pub const ACTION_COUNT: usize = 17;

impl KeymapAction {
    /// Every action, in declaration order.
    pub const ALL: [Self; ACTION_COUNT] = [
        Self::Submit,
        Self::QueueFollowUp,
        Self::Interrupt,
        Self::Quit,
        Self::ToggleReasoning,
        Self::ToggleTranscript,
        Self::CommandPalette,
        Self::CycleSidePanel,
        Self::ToggleTasks,
        Self::ToggleWorkflows,
        Self::ScrollHalfPageUp,
        Self::ScrollHalfPageDown,
        Self::ScrollPageUp,
        Self::ScrollPageDown,
        Self::ScrollToOldest,
        Self::ScrollToNewest,
        Self::InsertNewline,
    ];

    /// Stable persisted id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Submit => "submit",
            Self::QueueFollowUp => "queue-follow-up",
            Self::Interrupt => "interrupt",
            Self::Quit => "quit",
            Self::ToggleReasoning => "toggle-reasoning",
            Self::ToggleTranscript => "toggle-transcript",
            Self::CommandPalette => "command-palette",
            Self::CycleSidePanel => "cycle-side-panel",
            Self::ToggleTasks => "toggle-tasks",
            Self::ToggleWorkflows => "toggle-workflows",
            Self::ScrollHalfPageUp => "scroll-half-page-up",
            Self::ScrollHalfPageDown => "scroll-half-page-down",
            Self::ScrollPageUp => "scroll-page-up",
            Self::ScrollPageDown => "scroll-page-down",
            Self::ScrollToOldest => "scroll-to-oldest",
            Self::ScrollToNewest => "scroll-to-newest",
            Self::InsertNewline => "insert-newline",
        }
    }

    /// Parse a persisted action id.
    ///
    /// # Errors
    /// [`KeymapError::UnknownAction`] rather than a silent skip: an id this
    /// build does not know is a document written for a different build, and
    /// dropping it would quietly restore the default binding.
    pub fn parse(text: &str) -> Result<Self, KeymapError> {
        // Preserve an existing user's custom Ctrl+O replacement when the
        // former compaction-only action gains whole-transcript behavior.
        if text == "toggle-compaction" {
            return Ok(Self::ToggleTranscript);
        }
        Self::ALL
            .into_iter()
            .find(|action| action.as_str() == text)
            .ok_or_else(|| KeymapError::UnknownAction {
                action: text.to_owned(),
            })
    }

    /// The shipped binding, matching AGENTS.md §8.
    #[must_use]
    pub const fn default_chord(self) -> KeyChord {
        let (key, modifiers) = match self {
            Self::Submit => (KeyName::Enter, Modifiers::none()),
            Self::QueueFollowUp => (KeyName::Tab, Modifiers::none()),
            Self::Interrupt => (KeyName::Esc, Modifiers::none()),
            Self::Quit => (KeyName::Char('c'), Modifiers::ctrl()),
            Self::ToggleReasoning => (KeyName::Char('r'), Modifiers::ctrl()),
            Self::ToggleTranscript => (KeyName::Char('o'), Modifiers::ctrl()),
            Self::CommandPalette => (KeyName::Char('p'), Modifiers::ctrl()),
            Self::CycleSidePanel => (KeyName::Char('b'), Modifiers::ctrl()),
            Self::ToggleTasks => (KeyName::Char('t'), Modifiers::ctrl()),
            Self::ToggleWorkflows => (KeyName::Char('w'), Modifiers::alt()),
            Self::ScrollHalfPageUp => (KeyName::Char('u'), Modifiers::ctrl()),
            Self::ScrollHalfPageDown => (KeyName::Char('d'), Modifiers::ctrl()),
            Self::ScrollPageUp => (KeyName::PageUp, Modifiers::none()),
            Self::ScrollPageDown => (KeyName::PageDown, Modifiers::none()),
            Self::ScrollToOldest => (KeyName::Home, Modifiers::none()),
            Self::ScrollToNewest => (KeyName::End, Modifiers::none()),
            // Alt+Enter is the one chord every terminal delivers distinctly;
            // `\` + Enter and Shift+Enter (where the terminal reports it) are
            // handled by the composer as well.
            Self::InsertNewline => (KeyName::Enter, Modifiers::alt()),
        };
        KeyChord::new(key, modifiers)
    }
}

/// Keymap construction and persistence failures.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeymapError {
    /// A persisted action id this build does not define.
    #[error("unknown keymap action `{action}`")]
    UnknownAction {
        /// The offending id.
        action: String,
    },
    /// Text that is not a chord in the documented grammar.
    #[error("`{chord}` is not a key chord; expected [ctrl+][alt+][shift+]<key>")]
    InvalidChord {
        /// The offending text.
        chord: String,
    },
    /// One chord bound to two actions.
    #[error("`{chord}` is bound to both `{first}` and `{second}`")]
    Conflict {
        /// The contested chord, canonically spelled.
        chord: String,
        /// The earlier action in declaration order.
        first: &'static str,
        /// The later action in declaration order.
        second: &'static str,
    },
    /// The whole-session host consumes this chord before the TUI can see it.
    #[error("`{chord}` is reserved by the session host for detach")]
    ReservedChord {
        /// The host-owned chord, canonically spelled.
        chord: String,
    },
    /// The settings layer refused the document.
    #[error("keymap settings unavailable: {message}")]
    Settings {
        /// Safe settings diagnostic.
        message: String,
    },
}

/// Every action's live binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    bindings: BTreeMap<KeymapAction, KeyChord>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self::defaults()
    }
}

impl Keymap {
    /// The shipped bindings with nothing overridden.
    #[must_use]
    pub fn defaults() -> Self {
        Self {
            bindings: KeymapAction::ALL
                .into_iter()
                .map(|action| (action, action.default_chord()))
                .collect(),
        }
    }

    /// Apply user overrides over the defaults.
    ///
    /// # Errors
    /// [`KeymapError::Conflict`] when any chord ends up bound twice. This
    /// includes an override that collides with a *default* the user did not
    /// touch, which is the case a precedence rule would hide.
    pub fn resolve(overrides: &BTreeMap<KeymapAction, KeyChord>) -> Result<Self, KeymapError> {
        let mut bindings = BTreeMap::new();
        for action in KeymapAction::ALL {
            let chord = overrides
                .get(&action)
                .copied()
                .unwrap_or_else(|| action.default_chord());
            if chord
                == KeyChord::new(
                    KeyName::Char(']'),
                    Modifiers {
                        ctrl: true,
                        alt: false,
                        shift: false,
                    },
                )
            {
                return Err(KeymapError::ReservedChord {
                    chord: chord.to_string(),
                });
            }
            bindings.insert(action, chord);
        }
        // Declaration order, so the two named actions in a conflict are stable
        // regardless of how the overrides were iterated.
        let mut claimed: BTreeMap<KeyChord, KeymapAction> = BTreeMap::new();
        for action in KeymapAction::ALL {
            let Some(chord) = bindings.get(&action).copied() else {
                continue;
            };
            if let Some(existing) = claimed.get(&chord) {
                return Err(KeymapError::Conflict {
                    chord: chord.to_string(),
                    first: existing.as_str(),
                    second: action.as_str(),
                });
            }
            claimed.insert(chord, action);
        }
        Ok(Self { bindings })
    }

    /// The live chord for one action.
    #[must_use]
    pub fn chord(&self, action: KeymapAction) -> KeyChord {
        self.bindings
            .get(&action)
            .copied()
            .unwrap_or_else(|| action.default_chord())
    }

    /// The action a chord triggers, if any.
    #[must_use]
    pub fn action(&self, chord: KeyChord) -> Option<KeymapAction> {
        self.bindings
            .iter()
            .find(|(_, bound)| **bound == chord)
            .map(|(action, _)| *action)
    }

    /// Only the bindings that differ from the shipped defaults.
    ///
    /// Persisting just these keeps a user's document from freezing today's
    /// defaults, which is how a generated snapshot becomes a capability freeze
    /// (GOTCHAS #23).
    #[must_use]
    pub fn overrides(&self) -> BTreeMap<KeymapAction, KeyChord> {
        KeymapAction::ALL
            .into_iter()
            .filter_map(|action| {
                let chord = self.chord(action);
                (chord != action.default_chord()).then_some((action, chord))
            })
            .collect()
    }
}

/// Keymap settings namespace.
///
/// # Errors
/// Static namespace validation failure.
pub fn settings_namespace() -> Result<SettingsNamespace, SettingsError> {
    SettingsNamespace::new("keymap")
}

/// The settings definition a keymap persists into.
///
/// # Errors
/// Static schema validation failure.
pub fn settings_definition() -> Result<heycode_settings::SettingsDefinition, SettingsError> {
    let namespace = settings_namespace()?;
    let schema = heycode_settings::SettingsSchema::new(
        serde_json::json!({
            "type": "object",
            "properties": {
                "bindings": {
                    "type": "object",
                    "additionalProperties": {"type": "string"}
                }
            }
        }),
        serde_json::json!({"bindings": {}}),
        validate_bindings_section,
    )?
    // Action ids and key names, and nothing else. There is no field here a
    // credential could occupy, so local inspection boundaries may project it.
    .with_wire_exposure();
    Ok(heycode_settings::SettingsDefinition::new(namespace, schema))
}

fn validate_bindings_section(value: &serde_json::Value) -> Result<(), String> {
    let bindings = value
        .get("bindings")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "bindings must be an object".to_owned())?;
    let mut overrides = BTreeMap::new();
    for (action, chord) in bindings {
        let action = KeymapAction::parse(action).map_err(|error| error.to_string())?;
        let chord = chord
            .as_str()
            .ok_or_else(|| format!("binding for `{}` must be a string", action.as_str()))?;
        overrides.insert(action, KeyChord::parse(chord).map_err(|e| e.to_string())?);
    }
    // A conflicting document is refused here rather than at read time: the
    // settings layer keeps the last good generation, so a bad external edit
    // never becomes the live keymap.
    Keymap::resolve(&overrides).map_err(|error| error.to_string())?;
    Ok(())
}

/// Read and write the keymap through the layered settings stack.
pub struct SettingsBackedKeymap {
    settings: std::sync::Arc<heycode_settings::SettingsService>,
}

impl std::fmt::Debug for SettingsBackedKeymap {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SettingsBackedKeymap")
    }
}

impl SettingsBackedKeymap {
    /// Bind to a settings service.
    #[must_use]
    pub const fn new(settings: std::sync::Arc<heycode_settings::SettingsService>) -> Self {
        Self { settings }
    }

    /// The live keymap: defaults overlaid with persisted overrides.
    ///
    /// # Errors
    /// An unregistered namespace yields the defaults; a malformed or
    /// conflicting stored document fails loud rather than being skipped, so a
    /// user never silently loses a binding they wrote.
    pub fn load(&self) -> Result<Keymap, KeymapError> {
        self.load_versioned().map(|(keymap, _)| keymap)
    }

    /// Load the live keymap with the exact Settings user-section revision.
    ///
    /// # Errors
    /// Same contract as [`Self::load`].
    pub fn load_versioned(&self) -> Result<(Keymap, u64), KeymapError> {
        let namespace = settings_namespace().map_err(|error| KeymapError::Settings {
            message: error.to_string(),
        })?;
        let Some(snapshot) =
            self.settings
                .get(&namespace)
                .map_err(|error| KeymapError::Settings {
                    message: error.to_string(),
                })?
        else {
            return Ok((Keymap::defaults(), 0));
        };
        let revision = snapshot.revision();
        let Some(bindings) = snapshot
            .resolved()
            .get("bindings")
            .and_then(serde_json::Value::as_object)
        else {
            return Ok((Keymap::defaults(), revision));
        };
        let mut overrides = BTreeMap::new();
        for (action, chord) in bindings {
            let action = KeymapAction::parse(action)?;
            let chord = chord.as_str().ok_or_else(|| KeymapError::InvalidChord {
                chord: chord.to_string(),
            })?;
            overrides.insert(action, KeyChord::parse(chord)?);
        }
        Keymap::resolve(&overrides).map(|keymap| (keymap, revision))
    }

    /// Persist a keymap's overrides, replacing whatever was stored.
    ///
    /// # Errors
    /// A settings write failure, or a keymap that does not resolve.
    pub fn store(&self, keymap: &Keymap) -> Result<(), KeymapError> {
        let (_, revision) = self.load_versioned()?;
        self.store_at(keymap, revision)
    }

    /// Persist a keymap at one exact previously loaded revision.
    ///
    /// # Errors
    /// A stale revision, invalid keymap or Settings failure publishes no state.
    pub fn store_at(&self, keymap: &Keymap, expected_revision: u64) -> Result<(), KeymapError> {
        let namespace = settings_namespace().map_err(|error| KeymapError::Settings {
            message: error.to_string(),
        })?;
        let mut bindings = serde_json::Map::new();
        for (action, chord) in keymap.overrides() {
            bindings.insert(
                action.as_str().to_owned(),
                serde_json::Value::String(chord.to_string()),
            );
        }
        self.settings
            .replace_user(
                &namespace,
                serde_json::json!({"bindings": serde_json::Value::Object(bindings)}),
                Some(expected_revision),
            )
            .map(|_| ())
            .map_err(|error| KeymapError::Settings {
                message: error.to_string(),
            })
    }
}

/// Register the keymap settings namespace.
#[must_use]
pub fn keymap_plugin() -> Box<dyn heycode_core::Plugin> {
    struct KeymapPlugin;

    impl heycode_core::Plugin for KeymapPlugin {
        fn name(&self) -> &'static str {
            "keymap"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_settings::SERVICE_SETTINGS]
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::SettingsNamespace,
                "keymap",
            )]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let settings = context
                .get::<heycode_settings::SettingsService>(heycode_settings::SERVICE_SETTINGS)
                .ok_or_else(|| heycode_core::CoreError::other("settings missing"))?;
            settings
                .register(
                    context,
                    settings_definition()
                        .map_err(|error| heycode_core::CoreError::other(error.to_string()))?,
                )
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            Ok(())
        }
    }

    Box::new(KeymapPlugin)
}
