//! Validated human-command discovery and scheduling metadata.

/// Command metadata validation failure.
#[derive(Debug, thiserror::Error)]
pub enum CommandMetadataError {
    /// Id/source/argument grammar violation.
    #[error("invalid command {field}; expected lowercase kebab-case")]
    InvalidId {
        /// Rejected metadata field class.
        field: &'static str,
    },
    /// Display text violated trim/control/size rules.
    #[error("invalid command {field}; expected trimmed control-free text")]
    InvalidText {
        /// Rejected metadata field class.
        field: &'static str,
    },
    /// Argument names repeated or required/variadic order was ambiguous.
    #[error(
        "invalid command arguments; names must be unique, required precedes optional, variadic is last"
    )]
    InvalidArguments,
}

/// When a command may execute relative to an active agent turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandTiming {
    /// Safe to execute while a turn continues.
    Immediate,
    /// Runs after the active turn settles.
    Queued,
    /// Requires confirmation/cancellation of the active turn.
    Interrupting,
    /// Schedules durable domain state and may send logged model input.
    ModelScheduling,
}

impl CommandTiming {
    /// Stable diagnostic/UI id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Immediate => "immediate",
            Self::Queued => "queued",
            Self::Interrupting => "interrupting",
            Self::ModelScheduling => "model_scheduling",
        }
    }
}

/// Plugin attribution for one command descriptor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSource {
    plugin: String,
}

impl CommandSource {
    /// Construct from the owning plugin id.
    ///
    /// # Errors
    /// Malformed ids fail loud.
    pub fn from_plugin(plugin: impl Into<String>) -> Result<Self, CommandMetadataError> {
        let plugin = plugin.into();
        validate_id(&plugin, "source plugin")?;
        Ok(Self { plugin })
    }

    /// Owning plugin id.
    #[must_use]
    pub fn plugin(&self) -> &str {
        &self.plugin
    }
}

/// One ordered command argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandArgument {
    name: String,
    description: String,
    required: bool,
    variadic: bool,
}

impl CommandArgument {
    /// Required argument.
    ///
    /// # Errors
    /// Invalid name/description fails loud.
    pub fn required(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<Self, CommandMetadataError> {
        Self::new(name.into(), description.into(), true)
    }

    /// Optional argument.
    ///
    /// # Errors
    /// Invalid name/description fails loud.
    pub fn optional(
        name: impl Into<String>,
        description: impl Into<String>,
    ) -> Result<Self, CommandMetadataError> {
        Self::new(name.into(), description.into(), false)
    }

    fn new(
        name: String,
        description: String,
        required: bool,
    ) -> Result<Self, CommandMetadataError> {
        validate_id(&name, "argument id")?;
        validate_text(&description, "argument description")?;
        Ok(Self {
            name,
            description,
            required,
            variadic: false,
        })
    }

    /// Mark this final argument as consuming the remaining text/tokens.
    #[must_use]
    pub fn variadic(mut self) -> Self {
        self.variadic = true;
        self
    }

    /// Stable argument id.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human description.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Whether omission is invalid.
    #[must_use]
    pub const fn is_required(&self) -> bool {
        self.required
    }

    /// Whether this final argument consumes the remainder.
    #[must_use]
    pub const fn is_variadic(&self) -> bool {
        self.variadic
    }

    fn synopsis(&self) -> String {
        let suffix = if self.variadic { "..." } else { "" };
        if self.required {
            format!("<{}{suffix}>", self.name)
        } else {
            format!("[{}{suffix}]", self.name)
        }
    }
}

/// Stable discovery row for one command implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandDescriptor {
    id: String,
    description: String,
    arguments: Vec<CommandArgument>,
    timing: CommandTiming,
    source: CommandSource,
    shortcut: Option<String>,
}

impl CommandDescriptor {
    /// Validate complete command metadata.
    ///
    /// # Errors
    /// Invalid ids/text, duplicate argument names, required-after-optional or
    /// non-final variadic arguments fail loud.
    pub fn new(
        id: impl Into<String>,
        description: impl Into<String>,
        arguments: Vec<CommandArgument>,
        timing: CommandTiming,
        source: CommandSource,
    ) -> Result<Self, CommandMetadataError> {
        let id = id.into();
        let description = description.into();
        validate_id(&id, "id")?;
        validate_text(&description, "description")?;
        let mut names = std::collections::BTreeSet::new();
        let mut optional_seen = false;
        for (index, argument) in arguments.iter().enumerate() {
            if !names.insert(argument.name.as_str())
                || (optional_seen && argument.required)
                || (argument.variadic && index + 1 != arguments.len())
            {
                return Err(CommandMetadataError::InvalidArguments);
            }
            optional_seen |= !argument.required;
        }
        Ok(Self {
            id,
            description,
            arguments,
            timing,
            source,
            shortcut: None,
        })
    }

    /// Attach a visible keyboard shortcut.
    ///
    /// # Errors
    /// Invalid display text fails loud.
    pub fn with_shortcut(
        mut self,
        shortcut: impl Into<String>,
    ) -> Result<Self, CommandMetadataError> {
        let shortcut = shortcut.into();
        validate_text(&shortcut, "shortcut")?;
        self.shortcut = Some(shortcut);
        Ok(self)
    }

    /// Slash id without `/`.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Accepted compatibility names for this command, without `/`.
    ///
    /// The table is intentionally central: aliases are part of the product
    /// command language rather than ad-hoc alternate commands contributed by
    /// individual plugins. Only aliases whose heycode behavior is a truthful
    /// local equivalent belong here.
    #[must_use]
    pub fn aliases(&self) -> &'static [&'static str] {
        compatibility_aliases(&self.id)
    }

    /// Whether `name` is this descriptor's canonical id or an accepted alias.
    #[must_use]
    pub(crate) fn matches_name(&self, name: &str) -> bool {
        self.id == name || self.aliases().contains(&name)
    }

    /// One-line purpose.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Ordered argument metadata.
    #[must_use]
    pub fn arguments(&self) -> &[CommandArgument] {
        &self.arguments
    }

    /// Active-turn scheduling class.
    #[must_use]
    pub const fn timing(&self) -> CommandTiming {
        self.timing
    }

    /// Owning plugin.
    #[must_use]
    pub fn source(&self) -> &CommandSource {
        &self.source
    }

    /// Optional visible shortcut.
    #[must_use]
    pub fn shortcut(&self) -> Option<&str> {
        self.shortcut.as_deref()
    }

    /// `/name` plus structured argument placeholders.
    #[must_use]
    pub fn synopsis(&self) -> String {
        let arguments = self
            .arguments
            .iter()
            .map(CommandArgument::synopsis)
            .collect::<Vec<_>>()
            .join(" ");
        if arguments.is_empty() {
            format!("/{}", self.id)
        } else {
            format!("/{} {arguments}", self.id)
        }
    }
}

/// Product-level command aliases. Keep canonical command ids in plugin
/// inventories and registry listings; discovery and dispatch project these
/// alternate spellings from the descriptor.
fn compatibility_aliases(id: &str) -> &'static [&'static str] {
    match id {
        // Claude's primary name is `/clear`; heycode's durable session owner uses
        // `/new`, but both start a fresh resumable conversation.
        "new" => &["clear", "reset"],
        // heycode keeps the familiar terminal spelling as its canonical id.
        "quit" => &["exit"],
        // Same local operation under the public Claude command spellings.
        "usage" => &["cost"],
        "keymap" => &["keybindings"],
        "list-agents" => &["peers"],
        "connect" => &["login"],
        "permissions" => &["allowed-tools"],
        "plugins" => &["plugin"],
        "resume" => &["continue"],
        "rename" => &["name"],
        "rewind" => &["checkpoint", "undo"],
        "schedule" => &["routines"],
        "background" => &["bg"],
        "tasks" => &["bashes"],
        _ => &[],
    }
}

/// Dynamic availability projected without hiding unavailable commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandAvailability {
    reason: Option<String>,
}

impl CommandAvailability {
    /// Available command.
    #[must_use]
    pub const fn available() -> Self {
        Self { reason: None }
    }

    /// Unavailable with a visible repair/prerequisite reason.
    ///
    /// # Errors
    /// Invalid reason text fails loud.
    pub fn unavailable(reason: impl Into<String>) -> Result<Self, CommandMetadataError> {
        let reason = reason.into();
        validate_text(&reason, "availability reason")?;
        Ok(Self {
            reason: Some(reason),
        })
    }

    /// Whether execution may be offered.
    #[must_use]
    pub fn is_available(&self) -> bool {
        self.reason.is_none()
    }

    /// Visible reason when unavailable.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }
}

impl Default for CommandAvailability {
    fn default() -> Self {
        Self::available()
    }
}

/// Registry discovery projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandCatalogEntry {
    /// Static descriptor.
    pub descriptor: CommandDescriptor,
    /// Dynamic availability at snapshot time.
    pub availability: CommandAvailability,
}

impl CommandCatalogEntry {
    /// Plain-text help from the same descriptor and availability snapshot.
    #[must_use]
    pub fn help_line(&self) -> String {
        let aliases = if self.descriptor.aliases().is_empty() {
            String::new()
        } else {
            format!(
                " (aliases: {})",
                self.descriptor
                    .aliases()
                    .iter()
                    .map(|alias| format!("/{alias}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let unavailable = self
            .availability
            .reason()
            .map_or_else(String::new, |reason| format!(" [unavailable: {reason}]"));
        format!(
            "{}{aliases} — {}{unavailable}",
            self.descriptor.synopsis(),
            self.descriptor.description()
        )
    }
}

fn validate_id(value: &str, field: &'static str) -> Result<(), CommandMetadataError> {
    let bytes = value.as_bytes();
    if bytes.first().is_some_and(u8::is_ascii_lowercase)
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'-')
        && value.len() <= 128
    {
        Ok(())
    } else {
        Err(CommandMetadataError::InvalidId { field })
    }
}

fn validate_text(value: &str, field: &'static str) -> Result<(), CommandMetadataError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > 512
        || value.chars().any(char::is_control)
    {
        Err(CommandMetadataError::InvalidText { field })
    } else {
        Ok(())
    }
}
