//! What a telemetry event is allowed to be.
//!
//! Q08 (`heycode-live-artifact`) settled the shape for recorded-run data: metadata
//! and outcome are always safe, everything else is withheld until someone opts
//! in. Telemetry is the same asymmetry taken one step further — there is no
//! opt-in, because an event that leaves the machine has no bounded audience.
//! So instead of a content gate this module removes the ability to hold content
//! at all.
//!
//! Three closed vocabularies do that work:
//!
//! * [`TelemetryEventName`] — *what happened*. A free-text event name is the
//!   easiest way for a prompt to end up in telemetry (`"turn: <the prompt>"`).
//! * [`Dimension`] — *which axis*. Keys leak exactly as readily as values; an
//!   open key set admits `("user_prompt", …)`.
//! * [`Label`] — the one place text enters an event, and it must survive two
//!   independent screens before it can.
//!
//! The two label screens are deliberately not one, for the reason GOTCHAS #154
//! records. The character set refuses prose: no whitespace, no punctuation a
//! sentence needs, 64 bytes. The credential screen refuses secrets, and it is
//! the shared `heycode_settings::screen_text_for_credentials` rather than a second
//! copy of what a credential looks like. Neither subsumes the other — an API
//! key is a perfectly well-formed identifier, and a sentence is not credential
//! material — so removing either one lets real material through.
//!
//! Be honest about the reach of that. The character set stops prose *as anyone
//! writes it*; it does not stop a caller who deliberately hyphen-encodes a
//! short sentence, and no screen over free text could. What closes that gap is
//! upstream: every [`Dimension`] that exists names a registry identifier, so
//! there is no axis a call site could be tempted to put a sentence on.

use std::time::Duration;

use heycode_settings::WireExposureFault;
use serde::{Deserialize, Serialize};

/// Schema version of the telemetry event format.
///
/// Bumped whenever a reader could misinterpret an older event. A reader that
/// does not recognize the version must refuse it rather than guess.
pub const TELEMETRY_SCHEMA_VERSION: u32 = 2;

/// Longest label this crate will hold, in bytes.
///
/// Registry identifiers are short. The cap is what makes "a paragraph of user
/// text cannot be a label" true by size as well as by character set.
pub const LABEL_MAX_BYTES: usize = 64;

/// Most dimensions one event can carry.
///
/// This is a consequence of the types rather than a limit anyone enforces: the
/// key set is closed and an axis may be set once, so an event is bounded at one
/// label per [`Dimension`] with no length check to forget. Published for a
/// consumer sizing a buffer.
pub const MAX_DIMENSIONS: usize = Dimension::ALL.len();

/// Why a telemetry value could not be constructed.
///
/// A closed set, so a fault can never carry the offending text into an error,
/// a log or a diagnostic — the same rule S15's [`WireExposureFault`] and Q08's
/// `ArtifactFault` keep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TelemetryFault {
    /// The text contains recognizable credential material.
    CredentialMaterial,
    /// The text is empty, over the size cap, or contains a character no
    /// registry identifier uses.
    MalformedLabel,
    /// The same dimension was supplied twice.
    DuplicateDimension,
    /// The same resource attribute key was supplied twice.
    DuplicateAttribute,
    /// Aggregate metric count was zero.
    InvalidCount,
    /// The export endpoint is not a bounded absolute `http`/`https` address
    /// with an unencoded path and no query.
    MalformedEndpoint,
    /// The export endpoint carries credential material — userinfo before the
    /// host, or a recognized secret anywhere in the address.
    EndpointCarriesCredential,
}

impl std::fmt::Display for TelemetryFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CredentialMaterial => "the text contains recognizable credential material",
            Self::MalformedLabel => "the text is not a bounded registry identifier",
            Self::DuplicateDimension => "the dimension was already supplied",
            Self::DuplicateAttribute => "the resource attribute was already supplied",
            Self::InvalidCount => "the telemetry count must be positive",
            Self::MalformedEndpoint => "the endpoint is not a bounded absolute http address",
            Self::EndpointCarriesCredential => "the endpoint carries credential material",
        })
    }
}

impl std::error::Error for TelemetryFault {}

impl From<WireExposureFault> for TelemetryFault {
    fn from(_: WireExposureFault) -> Self {
        // The settings fault is dropped rather than wrapped, exactly as Q08
        // drops it: it is a closed set today, but re-exporting it here would
        // let a future variant that carries detail reach a telemetry event.
        Self::CredentialMaterial
    }
}

/// What happened, from a closed set.
///
/// Adding a name is a deliberate act with a compiler-checked cost: [`Self::ALL`]
/// and [`Self::index`] must agree, and the local counter array is sized from
/// them. That is the point — an event name is a published vocabulary, not a
/// format string a call site fills in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TelemetryEventName {
    /// A session was opened.
    SessionStarted,
    /// A turn ran to settlement, whatever its outcome.
    TurnCompleted,
    /// A tool was executed.
    ToolInvoked,
    /// A provider request failed.
    RequestFailed,
    /// A human-only command was dispatched.
    CommandInvoked,
    /// One durable provider request header committed.
    ProviderRequest,
    /// One durable compaction settlement committed.
    CompactionCompleted,
    /// One durable provider cache observation committed.
    CacheObserved,
}

impl TelemetryEventName {
    /// Every name, in stable order. The local counter array is sized from this.
    pub const ALL: [Self; 8] = [
        Self::SessionStarted,
        Self::TurnCompleted,
        Self::ToolInvoked,
        Self::RequestFailed,
        Self::CommandInvoked,
        Self::ProviderRequest,
        Self::CompactionCompleted,
        Self::CacheObserved,
    ];

    /// How many names exist.
    pub const COUNT: usize = Self::ALL.len();

    /// Position of this name in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::SessionStarted => 0,
            Self::TurnCompleted => 1,
            Self::ToolInvoked => 2,
            Self::RequestFailed => 3,
            Self::CommandInvoked => 4,
            Self::ProviderRequest => 5,
            Self::CompactionCompleted => 6,
            Self::CacheObserved => 7,
        }
    }

    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SessionStarted => "session_started",
            Self::TurnCompleted => "turn_completed",
            Self::ToolInvoked => "tool_invoked",
            Self::RequestFailed => "request_failed",
            Self::CommandInvoked => "command_invoked",
            Self::ProviderRequest => "provider_request",
            Self::CompactionCompleted => "compaction_completed",
            Self::CacheObserved => "cache_observed",
        }
    }
}

impl std::fmt::Display for TelemetryEventName {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Which axis a label describes, from a closed set.
///
/// Every variant names something that came out of a registry — a provider id,
/// a model id, a tool name — never something a person typed. There is no
/// `Detail`, `Message` or `Note` variant, and that absence is the contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Dimension {
    /// Inference provider registry id.
    Provider,
    /// Model id as the catalog publishes it.
    Model,
    /// Agent runtime id.
    Runtime,
    /// Registered tool name.
    Tool,
    /// Human-only command id.
    Command,
    /// Closed outcome classification the emitting layer already assigned.
    Outcome,
    /// Durable request purpose.
    Purpose,
    /// Session creation/lineage source.
    Lineage,
    /// Local, provider-exact or provider-aggregate execution plane.
    Execution,
    /// Compaction strategy/kind.
    Compaction,
    /// Cache activity class.
    Cache,
}

impl Dimension {
    /// Every dimension, in stable order.
    pub const ALL: [Self; 11] = [
        Self::Provider,
        Self::Model,
        Self::Runtime,
        Self::Tool,
        Self::Command,
        Self::Outcome,
        Self::Purpose,
        Self::Lineage,
        Self::Execution,
        Self::Compaction,
        Self::Cache,
    ];

    /// Stable diagnostic identifier.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Model => "model",
            Self::Runtime => "runtime",
            Self::Tool => "tool",
            Self::Command => "command",
            Self::Outcome => "outcome",
            Self::Purpose => "purpose",
            Self::Lineage => "lineage",
            Self::Execution => "execution",
            Self::Compaction => "compaction",
            Self::Cache => "cache",
        }
    }
}

impl std::fmt::Display for Dimension {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A bounded registry identifier that has passed the credential screen.
///
/// There is no way to build one except through [`Label::new`], and — unlike a
/// value that only validates on the way in — no way to deserialize one either:
/// serde routes through the same constructor, so a hand-written JSON file
/// cannot smuggle prose or a secret into a `Label` through the back door.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Label(String);

impl Label {
    /// Validate and screen `value`.
    ///
    /// # Errors
    /// [`TelemetryFault::MalformedLabel`] when the text is empty, longer than
    /// [`LABEL_MAX_BYTES`], or uses a character outside
    /// `A-Z a-z 0-9 - _ . : /`, or has the `://` structure of a URL.
    /// [`TelemetryFault::CredentialMaterial`] when it matches a format the
    /// shared settings screen recognizes as a secret.
    pub fn new(value: impl Into<String>) -> Result<Self, TelemetryFault> {
        let value = value.into();
        if value.is_empty() || value.len() > LABEL_MAX_BYTES {
            return Err(TelemetryFault::MalformedLabel);
        }
        let identifier = value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        });
        if !identifier {
            return Err(TelemetryFault::MalformedLabel);
        }
        screen_for_credentials(&value)?;
        if value.contains("://") {
            return Err(TelemetryFault::MalformedLabel);
        }
        Ok(Self(value))
    }

    /// The validated identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Label {
    type Error = TelemetryFault;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<Label> for String {
    fn from(label: Label) -> Self {
        label.0
    }
}

impl std::fmt::Display for Label {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Screen text for credential material with S15's shared recognizers, having
/// first split it on the delimiters those recognizers cannot split on
/// themselves.
///
/// **This exists because of a real gap, not for tidiness.**
/// `heycode_settings::screen_text_for_credentials` splits on whitespace and the
/// punctuation that surrounds a value in JSON — quotes, brackets, commas — and
/// then asks whether each token *starts with* a published issuer prefix. A URL
/// contains none of those separators, so `https://c.example.com/?t=sk-ant-…` is
/// one token that starts with `https`, and every prefix check misses it. The
/// `Label` character set admits `/` and `:`, so that string is a perfectly
/// well-formed label — and TEL02 believed the credential screen was catching it.
///
/// The fix is to hand the screen the tokens it needs rather than to write a
/// second recognizer: there is still exactly one list of what a credential looks
/// like, and it is still S15's (GOTCHAS #154). Only URL structure delimiters are
/// split on; `.` deliberately is not, because a JSON Web Token is three
/// dot-separated segments and splitting there would blind the recognizer that
/// finds it.
pub(crate) fn screen_for_credentials(text: &str) -> Result<(), TelemetryFault> {
    heycode_settings::screen_text_for_credentials(text)?;
    let split = text.replace(['/', ':', '?', '#', '@', '&', '='], " ");
    heycode_settings::screen_text_for_credentials(&split)?;
    Ok(())
}

/// One thing worth counting, and the axes it happened along.
///
/// Numbers, instants and closed vocabulary. There is no field a caller could
/// put a prompt, a file path, a tool argument or a response body in.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "TelemetryEventWire")]
pub struct TelemetryEvent {
    schema_version: u32,
    name: TelemetryEventName,
    at_unix_ms: u64,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    dimensions: Vec<(Dimension, Label)>,
    #[serde(default = "one")]
    count: u64,
    #[serde(skip_serializing_if = "Option::is_none", default)]
    duration_ms: Option<u64>,
}

#[derive(Deserialize)]
struct TelemetryEventWire {
    schema_version: u32,
    name: TelemetryEventName,
    at_unix_ms: u64,
    #[serde(default)]
    dimensions: Vec<(Dimension, Label)>,
    #[serde(default = "one")]
    count: u64,
    #[serde(default)]
    duration_ms: Option<u64>,
}

const fn one() -> u64 {
    1
}

#[derive(Debug)]
enum TelemetryEventReadFault {
    UnreadableSchema(u32),
    DuplicateDimension,
    InvalidCount,
}

impl std::fmt::Display for TelemetryEventReadFault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnreadableSchema(version) => write!(
                formatter,
                "unreadable telemetry schema version {version}; this build reads {TELEMETRY_SCHEMA_VERSION}"
            ),
            Self::DuplicateDimension => formatter.write_str("telemetry event repeats a dimension"),
            Self::InvalidCount => formatter.write_str("telemetry event count must be positive"),
        }
    }
}

impl TryFrom<TelemetryEventWire> for TelemetryEvent {
    type Error = TelemetryEventReadFault;

    fn try_from(wire: TelemetryEventWire) -> Result<Self, Self::Error> {
        if wire.schema_version == 0 || wire.schema_version > TELEMETRY_SCHEMA_VERSION {
            return Err(TelemetryEventReadFault::UnreadableSchema(
                wire.schema_version,
            ));
        }
        let mut seen: Vec<Dimension> = Vec::with_capacity(wire.dimensions.len());
        for (axis, _) in &wire.dimensions {
            if seen.contains(axis) {
                return Err(TelemetryEventReadFault::DuplicateDimension);
            }
            seen.push(*axis);
        }
        if wire.count == 0 {
            return Err(TelemetryEventReadFault::InvalidCount);
        }
        Ok(Self {
            schema_version: wire.schema_version,
            name: wire.name,
            at_unix_ms: wire.at_unix_ms,
            dimensions: wire.dimensions,
            count: wire.count,
            duration_ms: wire.duration_ms,
        })
    }
}

impl TelemetryEvent {
    /// One occurrence of `name` at `at_unix_ms`, with no dimensions yet.
    ///
    /// The timestamp is supplied rather than read, so this crate needs no clock
    /// and an event is reproducible in a test.
    #[must_use]
    pub const fn new(name: TelemetryEventName, at_unix_ms: u64) -> Self {
        Self {
            schema_version: TELEMETRY_SCHEMA_VERSION,
            name,
            at_unix_ms,
            dimensions: Vec::new(),
            count: 1,
            duration_ms: None,
        }
    }

    /// Attach one axis.
    ///
    /// There is no separate length check: refusing a repeat over a closed key
    /// set already bounds the event at [`MAX_DIMENSIONS`], and a cap that
    /// cannot be reached is protection in appearance only (GOTCHAS #152).
    ///
    /// # Errors
    /// [`TelemetryFault::DuplicateDimension`] when the axis is already set —
    /// silently overwriting would let a later call change what an earlier one
    /// recorded.
    pub fn with_dimension(
        mut self,
        dimension: Dimension,
        label: Label,
    ) -> Result<Self, TelemetryFault> {
        if self.dimensions.iter().any(|(axis, _)| *axis == dimension) {
            return Err(TelemetryFault::DuplicateDimension);
        }
        self.dimensions.push((dimension, label));
        Ok(self)
    }

    /// Attach one positive aggregate count.
    ///
    /// # Errors
    /// Zero is not an observation and is rejected.
    pub fn with_count(mut self, count: u64) -> Result<Self, TelemetryFault> {
        if count == 0 {
            return Err(TelemetryFault::InvalidCount);
        }
        self.count = count;
        Ok(self)
    }

    /// Attach a measured duration, saturating rather than wrapping.
    #[must_use]
    pub fn with_duration(mut self, duration: Duration) -> Self {
        self.duration_ms = Some(u64::try_from(duration.as_millis()).unwrap_or(u64::MAX));
        self
    }

    /// Schema version this event was built at.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// What happened.
    #[must_use]
    pub const fn name(&self) -> TelemetryEventName {
        self.name
    }

    /// When it happened.
    #[must_use]
    pub const fn at_unix_ms(&self) -> u64 {
        self.at_unix_ms
    }

    /// Attached axes, in the order they were supplied.
    #[must_use]
    pub fn dimensions(&self) -> &[(Dimension, Label)] {
        &self.dimensions
    }

    /// Positive number of observations represented by this event.
    #[must_use]
    pub const fn count(&self) -> u64 {
        self.count
    }

    /// The label on `dimension`, when one was attached.
    #[must_use]
    pub fn dimension(&self, dimension: Dimension) -> Option<&Label> {
        self.dimensions
            .iter()
            .find(|(axis, _)| *axis == dimension)
            .map(|(_, label)| label)
    }

    /// Measured duration, when the emitting layer measured one.
    #[must_use]
    pub const fn duration_ms(&self) -> Option<u64> {
        self.duration_ms
    }

    /// Read an event, re-checking every invariant construction enforced.
    ///
    /// Deserialization itself routes through the same schema and duplicate-axis
    /// checks, so callers cannot bypass this gate by invoking `serde_json`
    /// directly. This helper is the named public read boundary over that same
    /// constructor.
    ///
    /// # Errors
    /// A parse failure, an unreadable schema version, or a dimension list that
    /// repeats an axis — each reported as a parse failure so no caller can act
    /// on a field it does not understand. Refusing a repeat over a closed key
    /// set is also what bounds the list length, so there is no second check.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    /// A credential that is also a well-formed identifier, so it can only be
    /// caught by the credential screen and never by the character set.
    const SECRET: &str = "sk-ant-api03-0123456789abcdef";

    #[test]
    fn a_label_rejects_prose_because_a_sentence_is_not_an_identifier() {
        assert_eq!(
            Label::new("summarize the private key in this file"),
            Err(TelemetryFault::MalformedLabel)
        );
    }

    #[test]
    fn bodies_prompts_and_urls_have_no_label_shape() {
        for text in [
            r#"{"error":"provider response body"}"#,
            "summarize the private prompt",
            "https://public.example/models/model-id",
        ] {
            assert_eq!(
                Label::new(text),
                Err(TelemetryFault::MalformedLabel),
                "{text} entered the only outbound text type"
            );
        }
    }

    #[test]
    fn a_label_rejects_a_credential_that_the_character_set_would_have_allowed() {
        assert!(
            SECRET
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/')),
            "the fixture must be reachable through the character set, \
             or this test proves nothing about the credential screen"
        );
        assert_eq!(
            Label::new(SECRET),
            Err(TelemetryFault::CredentialMaterial),
            "the shared settings screen must refuse it"
        );
    }

    #[test]
    fn a_label_rejects_a_credential_hidden_behind_url_delimiters() {
        // The shared screen splits on whitespace and JSON punctuation, so a URL
        // reaches it as one token beginning with `https` and every issuer-prefix
        // check misses it. Without the extra split each of these is a
        // well-formed label carrying a live key.
        //
        // Every case is *inside* the label character set, and the two assertions
        // below say so: `?`, `=` and `@` are already refused by the character
        // set and would prove nothing here, while `/` and `:` are admitted — and
        // `/` is exactly the shape of a model id, `vendor/name`.
        for text in [
            "https://sk-ant-api03-0123456789.c.io/v1",
            "openrouter/sk-ant-api03-0123456789abc",
            "registry:ghp_0123456789abcdefghij0",
            "models/AIzaSyD01234567890123456789012345678",
        ] {
            assert!(
                text.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')),
                "{text} must reach the screen, not be stopped by the character set"
            );
            assert!(
                text.len() <= LABEL_MAX_BYTES,
                "{text} must be refused on its content, not its length"
            );
            assert_eq!(
                Label::new(text),
                Err(TelemetryFault::CredentialMaterial),
                "{text} reached a label"
            );
        }
    }

    #[test]
    fn the_url_split_is_what_refuses_a_credential_the_shared_screen_reads_as_one_token() {
        // The load-bearing half of `screen_for_credentials`. If the first
        // assertion ever fails, `heycode-settings` has learned to split URL
        // delimiters itself and the extra pass here is redundant — delete it
        // then, not before.
        const HIDDEN: &str = "https://c.io/?t=sk-ant-api03-0123456789";
        assert!(
            heycode_settings::screen_text_for_credentials(HIDDEN).is_ok(),
            "the shared screen sees one token beginning with `https`, so no \
             issuer-prefix check fires; that is the gap this wrapper closes"
        );
        assert_eq!(
            screen_for_credentials(HIDDEN),
            Err(TelemetryFault::CredentialMaterial),
            "splitting on URL delimiters is what makes the shared recognizers work"
        );
    }

    #[test]
    fn splitting_for_the_screen_does_not_blind_the_recognizers_that_need_dots() {
        // `.` is deliberately not a split character: a JSON Web Token is three
        // dot-separated segments, and splitting there would leave the recognizer
        // three fragments and no token to match.
        assert_eq!(
            Label::new("eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2Q"),
            Err(TelemetryFault::CredentialMaterial)
        );
        assert!(
            Label::new("z-ai/glm-5.3-flash").is_ok(),
            "an ordinary registry identifier must still pass"
        );
        assert!(Label::new("agent_runtime:native").is_ok());
    }

    #[test]
    fn a_label_rejects_an_embedded_bearer_token_shape() {
        assert_eq!(
            Label::new("eyJhbGciOiJIUzI1.eyJzdWIiOiIxMjM0.SflKxwRJSMeKKF2Q"),
            Err(TelemetryFault::CredentialMaterial)
        );
    }

    #[test]
    fn a_label_rejects_empty_and_oversized_text() {
        assert_eq!(Label::new(""), Err(TelemetryFault::MalformedLabel));
        let long = "a".repeat(LABEL_MAX_BYTES + 1);
        assert_eq!(Label::new(long), Err(TelemetryFault::MalformedLabel));
        assert!(Label::new("a".repeat(LABEL_MAX_BYTES)).is_ok());
    }

    #[test]
    fn a_label_accepts_a_real_registry_identifier() {
        let label = Label::new("z-ai/glm-5.3-flash").unwrap();
        assert_eq!(label.as_str(), "z-ai/glm-5.3-flash");
        assert!(Label::new("agent_runtime:native").is_ok());
    }

    #[test]
    fn a_label_cannot_be_deserialized_past_its_constructor() {
        let prose = serde_json::from_str::<Label>("\"a whole sentence of user text\"");
        assert!(
            prose.is_err(),
            "deserialization must route through Label::new"
        );
        let secret = serde_json::from_str::<Label>(&format!("\"{SECRET}\""));
        assert!(secret.is_err(), "the credential screen must run on read");
    }

    #[test]
    fn an_event_name_index_matches_its_position_in_all() {
        for (position, name) in TelemetryEventName::ALL.into_iter().enumerate() {
            assert_eq!(name.index(), position, "{name} is misindexed");
        }
        assert_eq!(TelemetryEventName::COUNT, TelemetryEventName::ALL.len());
    }

    #[test]
    fn event_name_and_dimension_identifiers_are_unique() {
        let mut names: Vec<&str> = TelemetryEventName::ALL
            .iter()
            .map(|name| name.as_str())
            .collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count, "duplicate event name identifier");

        let mut axes: Vec<&str> = Dimension::ALL.iter().map(|axis| axis.as_str()).collect();
        axes.sort_unstable();
        let count = axes.len();
        axes.dedup();
        assert_eq!(axes.len(), count, "duplicate dimension identifier");
    }

    #[test]
    fn an_event_refuses_a_repeated_dimension_rather_than_overwriting_it() {
        let event = TelemetryEvent::new(TelemetryEventName::TurnCompleted, 1)
            .with_dimension(Dimension::Model, Label::new("first").unwrap())
            .unwrap();
        assert_eq!(
            event
                .clone()
                .with_dimension(Dimension::Model, Label::new("second").unwrap()),
            Err(TelemetryFault::DuplicateDimension)
        );
        assert_eq!(
            event.dimension(Dimension::Model).map(Label::as_str),
            Some("first")
        );
    }

    #[test]
    fn an_event_is_bounded_at_one_label_per_axis_by_the_closed_key_set() {
        let mut event = TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1);
        for axis in Dimension::ALL {
            event = event
                .with_dimension(axis, Label::new(axis.as_str()).unwrap())
                .unwrap();
        }
        assert_eq!(event.dimensions().len(), MAX_DIMENSIONS);
        // Every axis that exists is taken, so there is no further call that
        // could grow this event — the bound is the type, not a length check.
        for axis in Dimension::ALL {
            assert_eq!(
                event
                    .clone()
                    .with_dimension(axis, Label::new("more").unwrap()),
                Err(TelemetryFault::DuplicateDimension),
                "{axis} must not be attachable twice"
            );
        }
    }

    #[test]
    fn a_duration_saturates_rather_than_wrapping() {
        // `Duration::MAX` is the wrong witness: its millisecond count is
        // 1000·2^64 − 1, whose low 64 bits are already `u64::MAX`, so a
        // truncating cast returns the right answer by accident. One second
        // short of it does not, which is what makes this test load-bearing.
        let event = TelemetryEvent::new(TelemetryEventName::TurnCompleted, 1)
            .with_duration(Duration::new(u64::MAX, 0));
        assert_eq!(event.duration_ms(), Some(u64::MAX));
        let outer =
            TelemetryEvent::new(TelemetryEventName::TurnCompleted, 1).with_duration(Duration::MAX);
        assert_eq!(outer.duration_ms(), Some(u64::MAX));
        let measured = TelemetryEvent::new(TelemetryEventName::TurnCompleted, 1)
            .with_duration(Duration::from_millis(1_250));
        assert_eq!(measured.duration_ms(), Some(1_250));
    }

    #[test]
    fn an_event_round_trips_through_json() {
        let event = TelemetryEvent::new(TelemetryEventName::RequestFailed, 1_700_000_000_000)
            .with_dimension(Dimension::Provider, Label::new("openrouter").unwrap())
            .unwrap()
            .with_duration(Duration::from_millis(42));
        let json = serde_json::to_string(&event).unwrap();
        assert_eq!(TelemetryEvent::from_json(&json).unwrap(), event);
        assert_eq!(event.schema_version(), TELEMETRY_SCHEMA_VERSION);
        assert_eq!(event.at_unix_ms(), 1_700_000_000_000);
    }

    #[test]
    fn aggregate_count_is_positive_and_v1_defaults_to_one() {
        let event = TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1)
            .with_count(3)
            .unwrap();
        assert_eq!(event.count(), 3);
        assert!(
            TelemetryEvent::new(TelemetryEventName::ToolInvoked, 1)
                .with_count(0)
                .is_err()
        );
        let v1 = r#"{"schema_version":1,"name":"tool_invoked","at_unix_ms":1}"#;
        assert_eq!(TelemetryEvent::from_json(v1).unwrap().count(), 1);
    }

    #[test]
    fn reading_refuses_an_unknown_schema_version() {
        let json = r#"{"schema_version":3,"name":"turn_completed","at_unix_ms":1}"#;
        let error = TelemetryEvent::from_json(json).unwrap_err().to_string();
        assert!(error.contains('3'), "{error}");
        assert!(
            serde_json::from_str::<TelemetryEvent>(json).is_err(),
            "direct deserialization must not bypass the schema gate"
        );
    }

    #[test]
    fn direct_deserialization_refuses_a_repeated_dimension() {
        let repeated = r#"{"schema_version":1,"name":"tool_invoked","at_unix_ms":1,
            "dimensions":[["model","a"],["model","b"]]}"#;
        assert!(
            serde_json::from_str::<TelemetryEvent>(repeated).is_err(),
            "Deserialize is a constructor and must enforce the closed-axis bound"
        );
        assert!(TelemetryEvent::from_json(repeated).is_err());
    }

    #[test]
    fn a_fault_never_carries_the_offending_text() {
        for fault in [
            TelemetryFault::CredentialMaterial,
            TelemetryFault::MalformedLabel,
            TelemetryFault::DuplicateDimension,
            TelemetryFault::DuplicateAttribute,
            TelemetryFault::MalformedEndpoint,
            TelemetryFault::EndpointCarriesCredential,
        ] {
            let rendered = format!("{fault}");
            assert!(!rendered.is_empty());
            assert!(!rendered.contains(SECRET), "{rendered}");
        }
        assert_eq!(
            TelemetryFault::from(WireExposureFault::SecretShapedKey),
            TelemetryFault::CredentialMaterial,
            "every settings fault collapses to one telemetry fault"
        );
    }
}
