//! MCP09 prompts and server instructions.
//!
//! Two contracts live here, and only one of them is about prompts.
//!
//! A **prompt** is a server-offered template, not a string: it declares
//! arguments, some of them required, and a Consumer fills them before asking
//! for the rendered messages. Discovery follows MCP06's generation discipline
//! exactly — a complete paginated `prompts/list` walk becomes one candidate
//! catalog, and only a complete, unraced candidate replaces the live one. A
//! malformed row rejects the whole generation rather than publishing a partial
//! prompt list.
//!
//! **Server instructions** are the free-text guidance a server returns from its
//! handshake. They are authored by whoever runs the server, they are destined
//! for a model's context, and the specification itself says they "MAY be added
//! to the system prompt". That makes them a prompt-injection surface, so
//! [`McpServerInstructions`] has no `Display`, no `Deref`, no `AsRef<str>` and a
//! redacted `Debug`: the only way to obtain a model-visible string is
//! [`McpServerInstructions::render_for_model`], which wraps the text in the core
//! [`UntrustedContentBoundary`] for [`heycode_core::UntrustedContentSource::Mcp`].
//! The source is not a parameter — a caller cannot choose the provenance of text
//! it did not author. The same treatment applies to fetched prompt content,
//! which is server-authored text on the same path.
//!
//! # Protocol revisions
//!
//! heycode's Streamable HTTP transport targets the legacy handshake era
//! (`2025-11-25`, `2025-06-18`); see [`crate::McpProtocolVersion`]. Every
//! `prompts/*` shape encoded here is identical in the legacy `2025-11-25`
//! revision and in the current `2026-07-28` revision — the latter only adds
//! `resultType`/`ttlMs`/`cacheScope` fields, which are read as unknown keys and
//! ignored. `instructions` moved container between the eras: it is
//! `InitializeResult.instructions` in `2025-11-25` and
//! `DiscoverResult.instructions` in `2026-07-28`, but sits at the top level of
//! the result object with the same type in both, so
//! [`McpPromptHandshake::from_handshake_result`] reads either.
//!
//! `2026-07-28` additionally permits a server to answer `prompts/get` with an
//! `InputRequiredResult` (multi round-trip requests). That shape carries no
//! `messages` array and is therefore refused loudly rather than mistaken for an
//! empty prompt.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::sync::Arc;

use heycode_core::UntrustedContentBoundary;
use tokio_util::sync::CancellationToken;

use crate::channel::{McpChannelError, McpRequestChannel};
use crate::generation::valid_segment;

/// `prompts/list` — `ListPromptsRequest.method`.
///
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#listing-prompts>
/// Identical in `2026-07-28`:
/// <https://modelcontextprotocol.io/specification/2026-07-28/server/prompts#listing-prompts>
const PROMPTS_LIST_METHOD: &str = "prompts/list";

/// `prompts/get` — `GetPromptRequest.method`.
///
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#getting-a-prompt>
/// Identical in `2026-07-28`:
/// <https://modelcontextprotocol.io/specification/2026-07-28/server/prompts#getting-a-prompt>
const PROMPTS_GET_METHOD: &str = "prompts/get";

/// `notifications/prompts/list_changed` — `PromptListChangedNotification.method`.
///
/// A transport marks its prompt [`crate::McpListChangeWatch`] when this arrives,
/// which is what lets a walk that spans a change be discarded instead of
/// published. Exported because the transport that must recognise the method
/// lives outside this module.
///
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#list-changed-notification>
/// Identical in `2026-07-28`, where delivery additionally requires a
/// `subscriptions/listen` stream with `promptsListChanged: true`:
/// <https://modelcontextprotocol.io/specification/2026-07-28/server/prompts#list-changed-notification>
pub const PROMPTS_LIST_CHANGED_NOTIFICATION: &str = "notifications/prompts/list_changed";

/// Cursor bound, matching MCP06's `tools/list` walk.
const MAX_CURSOR_BYTES: usize = 4 * 1024;
/// Whole-result bound for one `prompts/list` page, checked before any row is read.
const MAX_LIST_RESULT_BYTES: usize = 1024 * 1024;
/// Whole-result bound for one `prompts/get` answer, checked before any block is read.
const MAX_GET_RESULT_BYTES: usize = 1024 * 1024;
/// Display-name bound for a prompt or argument `title`.
const MAX_TITLE_BYTES: usize = 256;
/// Prose bound for a prompt `description`.
const MAX_PROMPT_DESCRIPTION_BYTES: usize = 4 * 1024;
/// Prose bound for a prompt argument `description`.
const MAX_ARGUMENT_DESCRIPTION_BYTES: usize = 2 * 1024;
/// Bound for one caller-supplied argument value.
const MAX_ARGUMENT_VALUE_BYTES: usize = 64 * 1024;
/// Bound for all caller-supplied argument values of one request together.
const MAX_TOTAL_ARGUMENT_BYTES: usize = 256 * 1024;
/// Bound for server instructions.
///
/// The same bound MCP06 applies to a tool description: both are server prose
/// whose destination is the model's context, so they answer to the same budget.
const MAX_INSTRUCTIONS_BYTES: usize = 16 * 1024;
/// Message-count bound for one `prompts/get` answer.
const MAX_PROMPT_MESSAGES: usize = 64;
/// Text bound for one prompt message.
const MAX_MESSAGE_TEXT_BYTES: usize = 64 * 1024;
/// Text bound for all messages of one prompt together.
const MAX_TOTAL_MESSAGE_BYTES: usize = 256 * 1024;

/// Stable MCP09 failure classes.
///
/// No caller-supplied argument value and no server body, header or endpoint may
/// enter this type. The two names it does carry — a declared argument name and a
/// prompt name — are server-declared segments that already passed
/// [`valid_segment`], and both are needed for the failure to be actionable.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum McpPromptError {
    /// The server has not proven a `prompts` capability, or nothing has been
    /// asked yet. `Unknown` and `Unsupported` both land here because neither is
    /// support.
    #[error("MCP server has not proven a prompts capability")]
    NotAdvertised,
    /// No prompt generation has been committed for this server.
    #[error("no MCP prompt generation has been committed")]
    NoGeneration,
    /// The live generation offers no prompt by that name.
    #[error("the live MCP prompt generation offers no prompt named `{prompt}`")]
    UnknownPrompt {
        /// Server-declared prompt name.
        prompt: String,
    },
    /// A required argument was not supplied.
    #[error("MCP prompt argument `{argument}` is required")]
    MissingRequiredArgument {
        /// Server-declared argument name.
        argument: String,
    },
    /// Arguments were supplied that the prompt does not declare. The supplied
    /// names are caller text and deliberately never reach this message; the
    /// declared names are on [`McpPromptDef::arguments`].
    #[error("{count} supplied argument(s) are not declared by this MCP prompt")]
    UnknownArguments {
        /// How many supplied arguments were undeclared.
        count: usize,
    },
    /// A caller-supplied argument value violated its bound. The value never
    /// enters the message.
    #[error("MCP prompt argument value is invalid: {requirement}")]
    ArgumentValue {
        /// Static requirement text.
        requirement: &'static str,
    },
    /// The server violated a closed protocol contract.
    #[error("MCP protocol contract failed: {requirement}")]
    Protocol {
        /// Static requirement text.
        requirement: &'static str,
    },
    /// A transport, timeout, cancellation, JSON-RPC or conflict failure from the
    /// shared request channel.
    #[error(transparent)]
    Channel(#[from] McpChannelError),
}

impl McpPromptError {
    const fn protocol(requirement: &'static str) -> Self {
        Self::Protocol { requirement }
    }

    const fn argument(requirement: &'static str) -> Self {
        Self::ArgumentValue { requirement }
    }
}

/// Tri-state evidence that a server offers prompts.
///
/// `Unknown` is reachable only before a handshake result has been read; parsing
/// a handshake always yields `Unsupported` or `Supported`. Nothing converts
/// `Unknown` into `Supported`, and every operation that needs the capability
/// treats `Unknown` exactly as it treats `Unsupported` — refusal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum McpPromptCapability {
    /// No handshake result has been observed. Never read as support.
    #[default]
    Unknown,
    /// A handshake completed and did not advertise `prompts`.
    Unsupported,
    /// A handshake advertised `prompts`.
    ///
    /// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#capabilities>
    Supported {
        /// Whether the server declared `prompts.listChanged`, i.e. whether it
        /// will emit `notifications/prompts/list_changed`.
        list_changed: bool,
    },
}

impl McpPromptCapability {
    /// Whether the server proved it offers prompts.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported { .. })
    }

    /// Whether a Consumer may rely on change notifications instead of polling.
    ///
    /// Only an explicit `listChanged: true` licenses that. An absent flag and an
    /// absent capability are both "no evidence", and no evidence is not a
    /// promise.
    #[must_use]
    pub const fn notifies_on_change(self) -> bool {
        matches!(self, Self::Supported { list_changed: true })
    }
}

/// Bounded, untrusted free-text guidance a server returned from its handshake.
///
/// The specification's own description of the field is the reason this type is
/// shaped the way it is: "This can be used by clients to improve the LLM's
/// understanding of available tools, resources, etc. It can be thought of like a
/// 'hint' to the model. For example, this information MAY be added to the system
/// prompt."
///
/// The text is authored by whoever operates the server. There is deliberately no
/// `Display`, no `Deref` and no `AsRef<str>`, so it cannot be formatted into a
/// prompt by accident; the accessor that yields the raw bytes is named
/// [`Self::untrusted_text`] so every call site reads as what it is; and the only
/// model-visible projection, [`Self::render_for_model`], cannot be called
/// without a core [`UntrustedContentBoundary`].
///
/// `Debug` reports the byte length only. A server controls this text completely,
/// and a derived `Debug` would let it reach a log — the same rule the crate
/// already applies to `HttpResponse` bodies.
#[derive(Clone, PartialEq, Eq)]
pub struct McpServerInstructions {
    text: String,
}

impl McpServerInstructions {
    /// Validate server instructions.
    ///
    /// # Errors
    /// Over [`MAX_INSTRUCTIONS_BYTES`], or carrying a control character other
    /// than newline, carriage return or tab. Oversized instructions are refused
    /// here rather than truncated: a truncated instruction block is a different
    /// instruction block, and shortening one silently would be the most useful
    /// possible gift to whoever wrote it.
    pub fn new(text: impl Into<String>) -> Result<Self, McpPromptError> {
        let text = text.into();
        check_prose(
            &text,
            MAX_INSTRUCTIONS_BYTES,
            "server instructions must be at most 16384 bytes of control-free text",
        )?;
        Ok(Self { text })
    }

    /// Length of the instructions in bytes.
    #[must_use]
    pub const fn byte_len(&self) -> usize {
        self.text.len()
    }

    /// The raw instruction text, for human display only.
    ///
    /// This is untrusted, server-authored text. Rendering it into anything a
    /// model reads must go through [`Self::render_for_model`] instead, so the
    /// data-only envelope travels with it.
    #[must_use]
    pub fn untrusted_text(&self) -> &str {
        &self.text
    }

    /// Wrap the instructions in the model-visible untrusted envelope.
    ///
    /// The source is [`heycode_core::UntrustedContentSource::Mcp`] and is not a parameter: a
    /// caller cannot choose the provenance of text it did not author, and a
    /// boundary naming the wrong source would tell the reader this came from
    /// somewhere it did not. This is the only way to obtain a model-visible
    /// string from this type.
    #[must_use]
    pub fn render_for_model(&self) -> String {
        UntrustedContentBoundary::mcp().render_for_model(&self.text)
    }
}

impl std::fmt::Debug for McpServerInstructions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpServerInstructions")
            .field("bytes", &self.text.len())
            .field("text", &"[REDACTED]")
            .finish()
    }
}

/// Tri-state record of what a server said about instructions.
///
/// "Never asked" and "asked, and the server has none" are different facts, and
/// collapsing them into `Option<String>` would make an unhandshaken server
/// indistinguishable from a terse one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum McpInstructions {
    /// No handshake result has been observed.
    #[default]
    Unknown,
    /// A handshake completed and carried no instructions.
    Absent,
    /// A handshake carried bounded, untrusted instructions.
    Present(McpServerInstructions),
}

impl McpInstructions {
    /// Whether a handshake result has been read at all.
    #[must_use]
    pub const fn was_asked(&self) -> bool {
        !matches!(self, Self::Unknown)
    }

    /// The instructions, when a handshake supplied some.
    #[must_use]
    pub const fn present(&self) -> Option<&McpServerInstructions> {
        match self {
            Self::Present(instructions) => Some(instructions),
            Self::Unknown | Self::Absent => None,
        }
    }
}

/// What MCP09 reads out of one handshake result.
///
/// Accepts either era's handshake answer: a legacy `InitializeResult`
/// (`2025-11-25`) or a modern `DiscoverResult` (`2026-07-28`). Both carry
/// `capabilities` and an optional top-level `instructions` string.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpPromptHandshake {
    prompts: McpPromptCapability,
    instructions: McpInstructions,
}

impl McpPromptHandshake {
    /// The state before anything has been asked: both facts `Unknown`.
    #[must_use]
    pub const fn unknown() -> Self {
        Self {
            prompts: McpPromptCapability::Unknown,
            instructions: McpInstructions::Unknown,
        }
    }

    /// Read prompt capability and server instructions from one handshake result.
    ///
    /// `capabilities` is required by both eras. An absent `prompts` capability
    /// yields [`McpPromptCapability::Unsupported`] — a fact, distinct from the
    /// `Unknown` of never having asked. An absent `instructions` field likewise
    /// yields [`McpInstructions::Absent`], not `Unknown`.
    ///
    /// # Errors
    /// A non-object result, an absent or non-object `capabilities`, a
    /// non-object `prompts` capability, a non-boolean `listChanged`, a
    /// non-string `instructions`, or instructions that violate their bound.
    ///
    /// <https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle#initialization>
    /// <https://modelcontextprotocol.io/specification/2026-07-28/server/discover>
    pub fn from_handshake_result(result: &serde_json::Value) -> Result<Self, McpPromptError> {
        let result = result.as_object().ok_or(McpPromptError::protocol(
            "handshake result must be an object",
        ))?;
        let capabilities = result
            .get("capabilities")
            .and_then(serde_json::Value::as_object)
            .ok_or(McpPromptError::protocol(
                "handshake result must carry a capabilities object",
            ))?;
        let prompts = match capabilities.get("prompts") {
            None | Some(serde_json::Value::Null) => McpPromptCapability::Unsupported,
            Some(serde_json::Value::Object(prompts)) => McpPromptCapability::Supported {
                list_changed: match prompts.get("listChanged") {
                    None | Some(serde_json::Value::Null) => false,
                    Some(serde_json::Value::Bool(flag)) => *flag,
                    Some(_) => {
                        return Err(McpPromptError::protocol(
                            "prompts.listChanged must be absent, null or a boolean",
                        ));
                    }
                },
            },
            Some(_) => {
                return Err(McpPromptError::protocol(
                    "prompts capability must be an object",
                ));
            }
        };
        let instructions = match result.get("instructions") {
            None | Some(serde_json::Value::Null) => McpInstructions::Absent,
            Some(serde_json::Value::String(text)) => {
                McpInstructions::Present(McpServerInstructions::new(text.clone())?)
            }
            Some(_) => {
                return Err(McpPromptError::protocol(
                    "instructions must be absent, null or a string",
                ));
            }
        };
        Ok(Self {
            prompts,
            instructions,
        })
    }

    /// Tri-state prompt capability evidence.
    #[must_use]
    pub const fn prompts(&self) -> McpPromptCapability {
        self.prompts
    }

    /// Tri-state server-instruction evidence.
    #[must_use]
    pub const fn instructions(&self) -> &McpInstructions {
        &self.instructions
    }
}

/// One argument a prompt declares.
///
/// `PromptArgument` extends `BaseMetadata`, so `name` is required and `title`,
/// `description` and `required` are optional; an absent `required` is `false`.
///
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#prompt>
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptArgumentDef {
    name: String,
    title: Option<String>,
    description: Option<String>,
    required: bool,
}

impl McpPromptArgumentDef {
    /// Programmatic argument name; the key used in a `prompts/get` request.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Optional human-readable display name.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Optional human-readable description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Whether the server requires this argument.
    #[must_use]
    pub const fn required(&self) -> bool {
        self.required
    }
}

/// One prompt template a server offers.
///
/// `Prompt` extends `BaseMetadata` and `Icons`; icons are display metadata this
/// build does not consume and are ignored rather than retained.
///
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#prompt>
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptDef {
    name: String,
    title: Option<String>,
    description: Option<String>,
    arguments: Vec<McpPromptArgumentDef>,
}

impl McpPromptDef {
    /// Programmatic prompt name; the `name` sent in a `prompts/get` request.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Optional human-readable display name.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Optional human-readable description.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Declared arguments, in the order the server listed them.
    #[must_use]
    pub fn arguments(&self) -> &[McpPromptArgumentDef] {
        &self.arguments
    }

    /// Bind caller-supplied values into a request that is valid by construction.
    ///
    /// Every rule is enforced here, before a request exists: a missing required
    /// argument and an undeclared argument are both caller mistakes, and
    /// discovering them from a server's `-32602` would mean the mistake had
    /// already left the process.
    ///
    /// Presence is what satisfies `required`; an empty string is a supplied
    /// value, because the specification says only that a required argument
    /// "must be provided" and inventing a non-empty rule would refuse prompts
    /// the server accepts.
    ///
    /// # Errors
    /// [`McpPromptError::MissingRequiredArgument`] naming the declared argument,
    /// [`McpPromptError::UnknownArguments`] counting the undeclared ones without
    /// echoing them, and [`McpPromptError::ArgumentValue`] for a value over
    /// 64 KiB, a request whose values exceed 256 KiB together, or a value
    /// carrying a control character other than newline, carriage return or tab.
    /// No caller-supplied byte enters any of these messages.
    pub fn bind(
        &self,
        values: BTreeMap<String, String>,
    ) -> Result<McpPromptRequest, McpPromptError> {
        let declared: BTreeSet<&str> = self
            .arguments
            .iter()
            .map(|argument| argument.name.as_str())
            .collect();
        let unknown = values
            .keys()
            .filter(|name| !declared.contains(name.as_str()))
            .count();
        if unknown > 0 {
            return Err(McpPromptError::UnknownArguments { count: unknown });
        }
        for argument in &self.arguments {
            if argument.required && !values.contains_key(&argument.name) {
                return Err(McpPromptError::MissingRequiredArgument {
                    argument: argument.name.clone(),
                });
            }
        }
        let mut total: usize = 0;
        for value in values.values() {
            if value.len() > MAX_ARGUMENT_VALUE_BYTES {
                return Err(McpPromptError::argument(
                    "an argument value must be at most 65536 bytes",
                ));
            }
            if has_forbidden_control(value) {
                return Err(McpPromptError::argument(
                    "an argument value must carry no control character other than \\n, \\r or \\t",
                ));
            }
            total = total.saturating_add(value.len());
        }
        if total > MAX_TOTAL_ARGUMENT_BYTES {
            return Err(McpPromptError::argument(
                "all argument values together must be at most 262144 bytes",
            ));
        }
        Ok(McpPromptRequest {
            name: self.name.clone(),
            arguments: values,
        })
    }
}

/// A `prompts/get` request that is valid by construction.
///
/// Minted only by [`McpPromptDef::bind`], so the prompt name is a server-declared
/// name and every argument is declared, bounded and control-free.
///
/// `Debug` lists argument names and redacts values: the values are caller text
/// heading for a remote server and may be anything at all, including a secret a
/// user pasted into a form.
#[derive(Clone, PartialEq, Eq)]
pub struct McpPromptRequest {
    name: String,
    arguments: BTreeMap<String, String>,
}

impl McpPromptRequest {
    /// Server-declared prompt name.
    #[must_use]
    pub fn prompt(&self) -> &str {
        &self.name
    }

    /// Supplied argument names, in deterministic order. Never the values.
    pub fn argument_names(&self) -> impl Iterator<Item = &str> {
        self.arguments.keys().map(String::as_str)
    }

    /// Exact `prompts/get` request parameters.
    ///
    /// `arguments` is `{ [key: string]: string }` and optional, so an empty map
    /// is omitted rather than sent as `{}`.
    ///
    /// <https://modelcontextprotocol.io/specification/2025-11-25/server/prompts#getting-a-prompt>
    #[must_use]
    pub fn params(&self) -> serde_json::Value {
        let mut params = serde_json::Map::new();
        params.insert(
            "name".to_owned(),
            serde_json::Value::String(self.name.clone()),
        );
        if !self.arguments.is_empty() {
            let arguments = self
                .arguments
                .iter()
                .map(|(name, value)| (name.clone(), serde_json::Value::String(value.clone())))
                .collect::<serde_json::Map<_, _>>();
            params.insert("arguments".to_owned(), serde_json::Value::Object(arguments));
        }
        serde_json::Value::Object(params)
    }
}

impl std::fmt::Debug for McpPromptRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpPromptRequest")
            .field("prompt", &self.name)
            .field("arguments", &self.arguments.keys().collect::<Vec<_>>())
            .field("values", &"[REDACTED]")
            .finish()
    }
}

/// Who a prompt message is attributed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpPromptRole {
    /// `"user"`.
    User,
    /// `"assistant"`.
    Assistant,
}

impl McpPromptRole {
    /// Exact wire spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            _ => None,
        }
    }
}

/// One message of a rendered prompt.
///
/// Only text content is admitted. `Debug` redacts the text for the same reason
/// [`McpServerInstructions`] does.
#[derive(Clone, PartialEq, Eq)]
pub struct McpPromptMessage {
    role: McpPromptRole,
    text: String,
}

impl McpPromptMessage {
    /// Attributed role.
    #[must_use]
    pub const fn role(&self) -> McpPromptRole {
        self.role
    }

    /// The raw message text, for human display only.
    ///
    /// This is untrusted, server-authored text. A model-visible projection must
    /// go through [`McpPromptFill::render_for_model`].
    #[must_use]
    pub fn untrusted_text(&self) -> &str {
        &self.text
    }
}

impl std::fmt::Debug for McpPromptMessage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpPromptMessage")
            .field("role", &self.role)
            .field("bytes", &self.text.len())
            .field("text", &"[REDACTED]")
            .finish()
    }
}

/// One server's answer to a `prompts/get` request.
///
/// The messages are server-authored text destined for a model context, so they
/// carry exactly the same discipline as [`McpServerInstructions`]: no `Display`,
/// a redacted `Debug`, and a model-visible projection only through an untrusted
/// boundary.
#[derive(Clone, PartialEq, Eq)]
pub struct McpPromptFill {
    description: Option<String>,
    messages: Vec<McpPromptMessage>,
}

impl McpPromptFill {
    /// Optional server description of the rendered prompt.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Rendered messages in server order.
    #[must_use]
    pub fn messages(&self) -> &[McpPromptMessage] {
        &self.messages
    }

    /// Deterministic model-visible projection inside the untrusted envelope.
    ///
    /// Only the messages are projected; the description describes the prompt to
    /// a human and is not part of what the model was asked to read. The source
    /// is [`heycode_core::UntrustedContentSource::Mcp`] for the same reason as
    /// [`McpServerInstructions::render_for_model`].
    #[must_use]
    pub fn render_for_model(&self) -> String {
        let body = self
            .messages
            .iter()
            .map(|message| format!("{}: {}", message.role.as_str(), message.text))
            .collect::<Vec<_>>()
            .join("\n\n");
        UntrustedContentBoundary::mcp().render_for_model(&body)
    }
}

impl std::fmt::Debug for McpPromptFill {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpPromptFill")
            .field("messages", &self.messages.len())
            .field("description", &"[REDACTED]")
            .finish()
    }
}

/// Explicit bounds applied to one paginated `prompts/list` walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpPromptListLimits {
    max_pages: NonZeroU32,
    max_page_prompts: NonZeroU32,
    max_prompts: NonZeroU32,
    max_arguments: NonZeroU32,
}

impl McpPromptListLimits {
    /// Replace the page budget for one walk.
    #[must_use]
    pub const fn with_max_pages(mut self, max_pages: NonZeroU32) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Replace the per-page row budget.
    #[must_use]
    pub const fn with_max_page_prompts(mut self, max_page_prompts: NonZeroU32) -> Self {
        self.max_page_prompts = max_page_prompts;
        self
    }

    /// Replace the total prompt budget for one generation.
    #[must_use]
    pub const fn with_max_prompts(mut self, max_prompts: NonZeroU32) -> Self {
        self.max_prompts = max_prompts;
        self
    }

    /// Replace the per-prompt argument budget.
    #[must_use]
    pub const fn with_max_arguments(mut self, max_arguments: NonZeroU32) -> Self {
        self.max_arguments = max_arguments;
        self
    }
}

impl Default for McpPromptListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroU32::new(64).unwrap_or(NonZeroU32::MIN),
            max_page_prompts: NonZeroU32::new(256).unwrap_or(NonZeroU32::MIN),
            max_prompts: NonZeroU32::new(1_024).unwrap_or(NonZeroU32::MIN),
            max_arguments: NonZeroU32::new(64).unwrap_or(NonZeroU32::MIN),
        }
    }
}

/// One complete, immutable prompt generation.
///
/// A catalog only ever exists for a walk that finished, was unraced and had
/// every row accepted. There is no partial catalog to observe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPromptCatalog {
    prompts: Vec<McpPromptDef>,
    count: u32,
}

impl McpPromptCatalog {
    /// Prompts in the order the server listed them across every page.
    #[must_use]
    pub fn prompts(&self) -> &[McpPromptDef] {
        &self.prompts
    }

    /// Whether the server listed no prompts at all. A distinct fact from never
    /// having asked, which is [`McpPromptGenerationOwner::catalog`] answering
    /// `None`.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Prompt count, for `McpContributionCounts::prompts`.
    #[must_use]
    pub const fn count(&self) -> u32 {
        self.count
    }

    /// Look one prompt up by its server-declared name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&McpPromptDef> {
        self.prompts.iter().find(|prompt| prompt.name == name)
    }
}

/// The single owner of one server's prompt generations.
///
/// Mirrors `McpToolGenerationOwner`: the walk runs while the previous catalog
/// stays live, and only a complete, unraced candidate replaces it. Any failure —
/// a malformed row, a budget, a repeated cursor, cancellation, or a
/// `notifications/prompts/list_changed` observed mid-walk — leaves the previous
/// catalog exactly as it was.
#[derive(Debug)]
pub struct McpPromptGenerationOwner {
    watch: crate::McpListChangeWatch,
    limits: McpPromptListLimits,
    /// Serializes candidate construction so two refreshes cannot interleave.
    swap: tokio::sync::Mutex<()>,
    /// Committed catalog, readable without entering the swap lane.
    committed: std::sync::Mutex<Option<Arc<McpPromptCatalog>>>,
}

impl McpPromptGenerationOwner {
    /// Bind one server's prompt change watch to its listing budget.
    ///
    /// The watch must be the one the transport marks on
    /// [`PROMPTS_LIST_CHANGED_NOTIFICATION`], and must not be shared with the
    /// tool watch: a tool list change is not a prompt list change, and sharing
    /// one epoch would discard sound generations.
    #[must_use]
    pub fn new(watch: crate::McpListChangeWatch, limits: McpPromptListLimits) -> Self {
        Self {
            watch,
            limits,
            swap: tokio::sync::Mutex::new(()),
            committed: std::sync::Mutex::new(None),
        }
    }

    /// The committed catalog, or `None` when none has ever been published.
    ///
    /// Never blocks on an in-flight refresh and never observes a partially
    /// assembled candidate.
    #[must_use]
    pub fn catalog(&self) -> Option<Arc<McpPromptCatalog>> {
        self.committed
            .lock()
            .ok()
            .and_then(|committed| committed.clone())
    }

    /// Walk the complete paginated prompt list and atomically replace the live
    /// catalog.
    ///
    /// Refuses before issuing any request unless the handshake proved the
    /// `prompts` capability: `Unknown` is not support, and asking anyway would
    /// turn "we never learned" into "we tried, so it must be there".
    ///
    /// # Errors
    /// [`McpPromptError::NotAdvertised`] without the capability;
    /// [`McpPromptError::Protocol`] for a malformed row, an exceeded budget, a
    /// repeated cursor or an oversized page; and
    /// [`McpPromptError::Channel`] for transport, timeout, cancellation,
    /// JSON-RPC failures and for a list change observed during the walk, which
    /// arrives as [`McpChannelError::Conflict`].
    pub async fn refresh(
        &self,
        channel: &dyn McpRequestChannel,
        handshake: &McpPromptHandshake,
        cancellation: &CancellationToken,
    ) -> Result<Arc<McpPromptCatalog>, McpPromptError> {
        if !handshake.prompts().is_supported() {
            return Err(McpPromptError::NotAdvertised);
        }
        let _lane = self.swap.lock().await;
        let epoch = self.watch.epoch();
        let catalog = walk_prompt_list(channel, self.limits, cancellation).await?;
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled.into());
        }
        if self.watch.epoch() != epoch {
            return Err(McpChannelError::Conflict.into());
        }
        let catalog = Arc::new(catalog);
        if let Ok(mut committed) = self.committed.lock() {
            *committed = Some(Arc::clone(&catalog));
        }
        Ok(catalog)
    }

    /// Drop the committed catalog.
    ///
    /// Used when a bounded reconnect exhausts: the registry stops claiming a
    /// generation, so the prompts it accounted for must stop being offered.
    /// Taking the swap lane means this can never tear an in-flight refresh.
    pub async fn retire(&self) {
        let _lane = self.swap.lock().await;
        if let Ok(mut committed) = self.committed.lock() {
            *committed = None;
        }
    }

    /// Fetch one prompt's rendered messages.
    ///
    /// The request is checked against the live catalog first: a prompt that is
    /// not in the committed generation is refused here rather than sent, so a
    /// stale UI selection cannot reach the server.
    ///
    /// # Errors
    /// [`McpPromptError::NoGeneration`] when nothing has been published,
    /// [`McpPromptError::UnknownPrompt`] when the live catalog does not offer it,
    /// [`McpPromptError::Protocol`] for a result that violates the closed
    /// `GetPromptResult` contract or its bounds, and
    /// [`McpPromptError::Channel`] for transport failures.
    pub async fn get(
        &self,
        channel: &dyn McpRequestChannel,
        request: &McpPromptRequest,
        cancellation: &CancellationToken,
    ) -> Result<McpPromptFill, McpPromptError> {
        let catalog = self.catalog().ok_or(McpPromptError::NoGeneration)?;
        if catalog.get(request.prompt()).is_none() {
            return Err(McpPromptError::UnknownPrompt {
                prompt: request.prompt().to_owned(),
            });
        }
        let result = channel
            .call(PROMPTS_GET_METHOD, request.params(), cancellation)
            .await?;
        parse_prompt_fill(&result)
    }
}

struct PromptPage {
    prompts: Vec<McpPromptDef>,
    next_cursor: Option<String>,
}

async fn walk_prompt_list(
    channel: &dyn McpRequestChannel,
    limits: McpPromptListLimits,
    cancellation: &CancellationToken,
) -> Result<McpPromptCatalog, McpPromptError> {
    let mut prompts: Vec<McpPromptDef> = Vec::new();
    let mut names: BTreeSet<String> = BTreeSet::new();
    let mut cursor: Option<String> = None;
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut pages: u32 = 0;
    loop {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled.into());
        }
        pages = pages.saturating_add(1);
        if pages > limits.max_pages.get() {
            return Err(McpPromptError::protocol(
                "prompt list exceeds the configured page budget",
            ));
        }
        let params = match &cursor {
            Some(cursor) => serde_json::json!({"cursor": cursor}),
            None => serde_json::json!({}),
        };
        let result = channel
            .call(PROMPTS_LIST_METHOD, params, cancellation)
            .await?;
        let page = parse_prompt_page(&result, limits)?;
        if prompts.len().saturating_add(page.prompts.len()) > limits.max_prompts.get() as usize {
            return Err(McpPromptError::protocol(
                "prompt list exceeds the configured prompt budget",
            ));
        }
        for prompt in page.prompts {
            if !names.insert(prompt.name.clone()) {
                return Err(McpPromptError::protocol(
                    "prompt list repeated a prompt name",
                ));
            }
            prompts.push(prompt);
        }
        // Absence or null ends the walk. An empty string is a valid opaque
        // cursor and means more results follow.
        let Some(next) = page.next_cursor else {
            let count = u32::try_from(prompts.len()).map_err(|_| {
                McpPromptError::protocol("prompt count exceeds the supported range")
            })?;
            return Ok(McpPromptCatalog { prompts, count });
        };
        if !visited.insert(next.clone()) {
            return Err(McpPromptError::protocol(
                "prompt list repeated a pagination cursor",
            ));
        }
        cursor = Some(next);
    }
}

fn parse_prompt_page(
    result: &serde_json::Value,
    limits: McpPromptListLimits,
) -> Result<PromptPage, McpPromptError> {
    check_result_size(
        result,
        MAX_LIST_RESULT_BYTES,
        "prompts/list result exceeds the 1048576-byte bound",
    )?;
    let result = result.as_object().ok_or(McpPromptError::protocol(
        "prompts/list result must be an object",
    ))?;
    let rows = result
        .get("prompts")
        .and_then(serde_json::Value::as_array)
        .ok_or(McpPromptError::protocol(
            "prompts/list result must contain a prompts array",
        ))?;
    if rows.len() > limits.max_page_prompts.get() as usize {
        return Err(McpPromptError::protocol(
            "prompts/list page exceeds the configured page-size budget",
        ));
    }
    let mut prompts = Vec::with_capacity(rows.len());
    for row in rows {
        prompts.push(parse_prompt(row, limits)?);
    }
    let next_cursor = match result.get("nextCursor") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(cursor)) if cursor.len() <= MAX_CURSOR_BYTES => {
            Some(cursor.clone())
        }
        Some(_) => {
            return Err(McpPromptError::protocol(
                "nextCursor must be absent, null or a bounded opaque string",
            ));
        }
    };
    Ok(PromptPage {
        prompts,
        next_cursor,
    })
}

fn parse_prompt(
    row: &serde_json::Value,
    limits: McpPromptListLimits,
) -> Result<McpPromptDef, McpPromptError> {
    let row = row.as_object().ok_or(McpPromptError::protocol(
        "prompt definition must be an object",
    ))?;
    let name =
        row.get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(McpPromptError::protocol(
                "prompt definition must carry a string name",
            ))?;
    // The same segment rule MCP06 applies to tool names. The specification's
    // own user-interaction model exposes prompts as slash commands, so a name
    // must be safe to place in a command id.
    if !valid_segment(name) {
        return Err(McpPromptError::protocol(
            "prompt name must be 1..=64 ASCII alphanumeric, `_` or `-` bytes",
        ));
    }
    let title = optional_line(
        row.get("title"),
        MAX_TITLE_BYTES,
        "prompt title must be absent, null or bounded one-line text",
    )?;
    let description = optional_prose(
        row.get("description"),
        MAX_PROMPT_DESCRIPTION_BYTES,
        "prompt description must be absent, null or at most 4096 bytes of control-free text",
    )?;
    let arguments = match row.get("arguments") {
        None | Some(serde_json::Value::Null) => Vec::new(),
        Some(serde_json::Value::Array(rows)) => {
            if rows.len() > limits.max_arguments.get() as usize {
                return Err(McpPromptError::protocol(
                    "prompt exceeds the configured argument budget",
                ));
            }
            let mut arguments = Vec::with_capacity(rows.len());
            let mut seen = BTreeSet::new();
            for row in rows {
                let argument = parse_argument(row)?;
                if !seen.insert(argument.name.clone()) {
                    return Err(McpPromptError::protocol("prompt repeated an argument name"));
                }
                arguments.push(argument);
            }
            arguments
        }
        Some(_) => {
            return Err(McpPromptError::protocol(
                "prompt arguments must be absent, null or an array",
            ));
        }
    };
    Ok(McpPromptDef {
        name: name.to_owned(),
        title,
        description,
        arguments,
    })
}

fn parse_argument(row: &serde_json::Value) -> Result<McpPromptArgumentDef, McpPromptError> {
    let row = row.as_object().ok_or(McpPromptError::protocol(
        "prompt argument must be an object",
    ))?;
    let name =
        row.get("name")
            .and_then(serde_json::Value::as_str)
            .ok_or(McpPromptError::protocol(
                "prompt argument must carry a string name",
            ))?;
    if !valid_segment(name) {
        return Err(McpPromptError::protocol(
            "prompt argument name must be 1..=64 ASCII alphanumeric, `_` or `-` bytes",
        ));
    }
    let title = optional_line(
        row.get("title"),
        MAX_TITLE_BYTES,
        "prompt argument title must be absent, null or bounded one-line text",
    )?;
    let description = optional_prose(
        row.get("description"),
        MAX_ARGUMENT_DESCRIPTION_BYTES,
        "prompt argument description must be absent, null or at most 2048 bytes of control-free text",
    )?;
    let required = match row.get("required") {
        None | Some(serde_json::Value::Null) => false,
        Some(serde_json::Value::Bool(required)) => *required,
        Some(_) => {
            return Err(McpPromptError::protocol(
                "prompt argument `required` must be absent, null or a boolean",
            ));
        }
    };
    Ok(McpPromptArgumentDef {
        name: name.to_owned(),
        title,
        description,
        required,
    })
}

fn parse_prompt_fill(result: &serde_json::Value) -> Result<McpPromptFill, McpPromptError> {
    check_result_size(
        result,
        MAX_GET_RESULT_BYTES,
        "prompts/get result exceeds the 1048576-byte bound",
    )?;
    let result = result.as_object().ok_or(McpPromptError::protocol(
        "prompts/get result must be an object",
    ))?;
    let description = optional_prose(
        result.get("description"),
        MAX_PROMPT_DESCRIPTION_BYTES,
        "prompts/get description must be absent, null or at most 4096 bytes of control-free text",
    )?;
    let rows = result
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .ok_or(McpPromptError::protocol(
            "prompts/get result must contain a messages array",
        ))?;
    if rows.len() > MAX_PROMPT_MESSAGES {
        return Err(McpPromptError::protocol(
            "prompts/get result exceeds the 64-message bound",
        ));
    }
    let mut messages = Vec::with_capacity(rows.len());
    let mut total: usize = 0;
    for row in rows {
        let message = parse_prompt_message(row)?;
        total = total.saturating_add(message.text.len());
        messages.push(message);
    }
    if total > MAX_TOTAL_MESSAGE_BYTES {
        return Err(McpPromptError::protocol(
            "prompts/get messages exceed the 262144-byte total bound",
        ));
    }
    Ok(McpPromptFill {
        description,
        messages,
    })
}

fn parse_prompt_message(row: &serde_json::Value) -> Result<McpPromptMessage, McpPromptError> {
    let row = row
        .as_object()
        .ok_or(McpPromptError::protocol("prompt message must be an object"))?;
    let role = row
        .get("role")
        .and_then(serde_json::Value::as_str)
        .and_then(McpPromptRole::parse)
        .ok_or(McpPromptError::protocol(
            "prompt message role must be `user` or `assistant`",
        ))?;
    let content = row
        .get("content")
        .and_then(serde_json::Value::as_object)
        .ok_or(McpPromptError::protocol(
            "prompt message content must be an object",
        ))?;
    // Image, audio, resource_link and embedded resource blocks are specified
    // content types this build cannot carry. Dropping one would hand the model
    // a prompt the server did not write, so an unsupported block refuses the
    // whole answer instead.
    match content.get("type").and_then(serde_json::Value::as_str) {
        Some("text") => {}
        _ => {
            return Err(McpPromptError::protocol(
                "prompt message content must be a `text` block; other content types are not supported",
            ));
        }
    }
    let text = content
        .get("text")
        .and_then(serde_json::Value::as_str)
        .ok_or(McpPromptError::protocol(
            "prompt text content must carry a string text field",
        ))?;
    check_prose(
        text,
        MAX_MESSAGE_TEXT_BYTES,
        "prompt message text must be at most 65536 bytes of control-free text",
    )?;
    Ok(McpPromptMessage {
        role,
        text: text.to_owned(),
    })
}

fn check_result_size(
    result: &serde_json::Value,
    max_bytes: usize,
    requirement: &'static str,
) -> Result<(), McpPromptError> {
    // Serialization failure cannot shrink a value, so an unmeasurable result is
    // treated as oversized rather than admitted unmeasured.
    if serde_json::to_vec(result).map_or(usize::MAX, |bytes| bytes.len()) > max_bytes {
        return Err(McpPromptError::protocol(requirement));
    }
    Ok(())
}

/// Control characters other than the ordinary whitespace prose legitimately
/// contains. Rejecting the rest keeps escape sequences out of a terminal render
/// and NUL out of anything downstream.
fn has_forbidden_control(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

fn check_prose(
    value: &str,
    max_bytes: usize,
    requirement: &'static str,
) -> Result<(), McpPromptError> {
    if value.len() > max_bytes || has_forbidden_control(value) {
        return Err(McpPromptError::protocol(requirement));
    }
    Ok(())
}

fn optional_prose(
    value: Option<&serde_json::Value>,
    max_bytes: usize,
    requirement: &'static str,
) -> Result<Option<String>, McpPromptError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => {
            check_prose(text, max_bytes, requirement)?;
            Ok(Some(text.clone()))
        }
        Some(_) => Err(McpPromptError::protocol(requirement)),
    }
}

fn optional_line(
    value: Option<&serde_json::Value>,
    max_bytes: usize,
    requirement: &'static str,
) -> Result<Option<String>, McpPromptError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => {
            if text.is_empty()
                || text.len() > max_bytes
                || text.trim() != text
                || text.chars().any(char::is_control)
            {
                return Err(McpPromptError::protocol(requirement));
            }
            Ok(Some(text.clone()))
        }
        Some(_) => Err(McpPromptError::protocol(requirement)),
    }
}
