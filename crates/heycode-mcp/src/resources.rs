//! MCP08 resources and subscriptions.
//!
//! Four operations, one lifecycle owner. `resources/list` is walked as an
//! atomic generation exactly the way MCP06 walks `tools/list`: the paginated
//! candidate is assembled while the previous generation stays committed, a
//! malformed row rejects the whole candidate rather than publishing a partial
//! list, and a `notifications/resources/list_changed` observed during the walk
//! discards it. `resources/read` is bounded and refuses an oversized payload
//! before normalizing anything.
//!
//! The subscription half is where the failure modes live, so all three are
//! modelled explicitly:
//!
//! * A subscription is a **registration**, owned by a [`Context`] effect. When
//!   the owning plugin rolls back or the context shuts down, the routing entry
//!   disappears and the observer stops being called. A subscription that
//!   outlives its owner is not merely a leak — it keeps delivering.
//! * A `notifications/resources/updated` for a URI nobody watches is **dropped
//!   safely**. [`McpResourceRegistry::observe_notification`] returns no
//!   `Result` at all, so an unmatched or malformed notification structurally
//!   cannot fail a connection: the server may simply be racing an unsubscribe.
//! * A server may drop a subscription without saying so. That is projected as
//!   [`McpSubscriptionState::Lapsed`] — evidence that the committed generation
//!   no longer lists the URI — never guessed.
//!
//! # Protocol revisions
//!
//! This module targets the **legacy** revisions heycode's transports speak,
//! `2025-11-25` and `2025-06-18` (see [`crate::McpProtocolVersion`]). Both
//! define the identical resource surface: `resources/list`, `resources/read`,
//! `resources/subscribe`, `resources/unsubscribe`,
//! `notifications/resources/updated` and
//! `notifications/resources/list_changed`.
//! <https://modelcontextprotocol.io/specification/2025-11-25/server/resources>
//!
//! The current `2026-07-28` revision **removed** the `resources/subscribe` and
//! `resources/unsubscribe` requests outright, replacing them with a
//! `subscriptions/listen` stream carrying a `resourceSubscriptions` filter, and
//! moved resource-not-found from `-32002` to `-32602`. That is a different
//! subscription contract, not a newer dialect of this one; supporting it means
//! adding the stream, not editing these constants.
//! <https://modelcontextprotocol.io/specification/2026-07-28/server/resources>

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, Weak};

use base64::Engine as _;
use heycode_core::{Context, UntrustedContentBoundary};
use tokio_util::sync::CancellationToken;

use crate::channel::{McpChannelError, McpRequestChannel};
use crate::generation::McpListChangeWatch;

/// `resources/list` — paginated listing.
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/resources#listing-resources>
const METHOD_LIST: &str = "resources/list";
/// `resources/read` — read one URI.
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/resources#reading-resources>
const METHOD_READ: &str = "resources/read";
/// `resources/subscribe` — legacy-era per-URI subscription request. Removed in
/// `2026-07-28` in favour of `subscriptions/listen`.
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/resources#subscriptions>
const METHOD_SUBSCRIBE: &str = "resources/subscribe";
/// `resources/unsubscribe` — cancels a prior `resources/subscribe`.
/// <https://github.com/modelcontextprotocol/modelcontextprotocol/blob/main/schema/2025-11-25/schema.ts>
const METHOD_UNSUBSCRIBE: &str = "resources/unsubscribe";

/// `notifications/resources/updated` — one watched resource changed.
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/resources#subscriptions>
pub const NOTIFICATION_RESOURCE_UPDATED: &str = "notifications/resources/updated";
/// `notifications/resources/list_changed` — the resource list changed.
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/resources#list-changed-notification>
pub const NOTIFICATION_RESOURCE_LIST_CHANGED: &str = "notifications/resources/list_changed";

/// Pagination cursors are opaque tokens; MCP06 uses the same bound.
const MAX_CURSOR_BYTES: usize = 4 * 1024;
/// Resource URIs are RFC 3986 identifiers, not documents.
const MAX_URI_BYTES: usize = 2 * 1024;
/// `name`/`title`/`mimeType` are single-line display metadata.
const MAX_DISPLAY_BYTES: usize = 1_024;
/// Matches MCP06's tool-description bound.
const MAX_DESCRIPTION_BYTES: usize = 16 * 1024;
/// One `contents` entry of a `resources/read` result.
const MAX_CONTENT_BYTES: usize = 1024 * 1024;
/// Every `contents` entry of one `resources/read` result, summed.
const MAX_READ_BYTES: usize = 4 * 1024 * 1024;
/// Entries in one `resources/read` result.
const MAX_READ_CONTENTS: usize = 256;
/// Live subscriptions one server may hold.
const MAX_SUBSCRIPTIONS: usize = 256;

/// Tri-state evidence for one optional resource sub-capability.
///
/// The same semantics as `heycode_llm::CapabilitySupport`, spelled locally because
/// `heycode-mcp` does not depend on `heycode-llm` and the dependency table forbids
/// adding one for a three-variant enum. `Unknown` means no `initialize` result
/// has been observed; it is never inferred into `Supported`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpResourceSupport {
    /// The server explicitly advertised the feature.
    Supported,
    /// The server explicitly did not advertise the feature.
    Unsupported,
    /// No `initialize` evidence has been observed.
    Unknown,
}

impl McpResourceSupport {
    /// Preserve unknown rather than coercing it to false.
    #[must_use]
    pub const fn as_bool(self) -> Option<bool> {
        match self {
            Self::Supported => Some(true),
            Self::Unsupported => Some(false),
            Self::Unknown => None,
        }
    }

    /// True only for explicit support.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }

    const fn from_flag(present: bool) -> Self {
        if present {
            Self::Supported
        } else {
            Self::Unsupported
        }
    }
}

/// What one `initialize` result advertised under `capabilities.resources`.
///
/// A server that supports resources **MUST** declare the capability, and the
/// specification's own example annotates `"resources": {}` as "Neither feature
/// supported" — so a present object with an absent `subscribe` is explicit
/// evidence of non-support, while a wholly absent `resources` object makes the
/// sub-features moot rather than merely unproven.
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/resources#capabilities>
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpResourceCapability {
    resources: McpResourceSupport,
    subscribe: McpResourceSupport,
    list_changed: McpResourceSupport,
}

impl McpResourceCapability {
    /// No `initialize` result observed yet. Every field is `Unknown`.
    pub const UNKNOWN: Self = Self {
        resources: McpResourceSupport::Unknown,
        subscribe: McpResourceSupport::Unknown,
        list_changed: McpResourceSupport::Unknown,
    };

    /// Read `capabilities.resources` out of one `initialize` result.
    ///
    /// # Errors
    /// A non-object result, a non-object `capabilities`, a non-object
    /// `resources`, or a non-boolean `subscribe`/`listChanged`. A server that
    /// spells a flag as anything but a boolean has violated a closed contract,
    /// and guessing which way it meant it is how an unsupported feature becomes
    /// a supported claim.
    pub fn from_initialize_result(result: &serde_json::Value) -> Result<Self, McpChannelError> {
        let result = result.as_object().ok_or(McpChannelError::protocol(
            "initialize result must be an object",
        ))?;
        let capabilities = result
            .get("capabilities")
            .and_then(serde_json::Value::as_object)
            .ok_or(McpChannelError::protocol(
                "initialize result is missing capabilities",
            ))?;
        let Some(resources) = capabilities.get("resources") else {
            return Ok(Self {
                resources: McpResourceSupport::Unsupported,
                subscribe: McpResourceSupport::Unsupported,
                list_changed: McpResourceSupport::Unsupported,
            });
        };
        let resources = resources.as_object().ok_or(McpChannelError::protocol(
            "initialize capability must be an object",
        ))?;
        Ok(Self {
            resources: McpResourceSupport::Supported,
            subscribe: McpResourceSupport::from_flag(capability_flag(resources, "subscribe")?),
            list_changed: McpResourceSupport::from_flag(capability_flag(resources, "listChanged")?),
        })
    }

    /// Whether the server advertised resources at all.
    #[must_use]
    pub const fn resources(self) -> McpResourceSupport {
        self.resources
    }

    /// Whether the server advertised per-resource subscriptions.
    #[must_use]
    pub const fn subscribe(self) -> McpResourceSupport {
        self.subscribe
    }

    /// Whether the server advertised `notifications/resources/list_changed`.
    #[must_use]
    pub const fn list_changed(self) -> McpResourceSupport {
        self.list_changed
    }
}

fn capability_flag(
    resources: &serde_json::Map<String, serde_json::Value>,
    name: &'static str,
) -> Result<bool, McpChannelError> {
    match resources.get(name) {
        None => Ok(false),
        Some(serde_json::Value::Bool(flag)) => Ok(*flag),
        Some(_) => Err(McpChannelError::protocol(
            "resources capability flags must be booleans",
        )),
    }
}

/// Explicit bounds applied to one paginated `resources/list` walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpResourceListLimits {
    max_pages: NonZeroU32,
    max_page_resources: NonZeroU32,
    max_resources: NonZeroU32,
}

impl McpResourceListLimits {
    /// Replace the page budget for one walk.
    #[must_use]
    pub const fn with_max_pages(mut self, max_pages: NonZeroU32) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Replace the per-page row budget.
    #[must_use]
    pub const fn with_max_page_resources(mut self, max_page_resources: NonZeroU32) -> Self {
        self.max_page_resources = max_page_resources;
        self
    }

    /// Replace the total resource budget for one generation.
    #[must_use]
    pub const fn with_max_resources(mut self, max_resources: NonZeroU32) -> Self {
        self.max_resources = max_resources;
        self
    }
}

impl Default for McpResourceListLimits {
    fn default() -> Self {
        Self {
            max_pages: NonZeroU32::new(64).unwrap_or(NonZeroU32::MIN),
            max_page_resources: NonZeroU32::new(512).unwrap_or(NonZeroU32::MIN),
            max_resources: NonZeroU32::new(2_048).unwrap_or(NonZeroU32::MIN),
        }
    }
}

/// One resource advertised by a server.
///
/// Every display field is validated bounded, trimmed, control-free text before
/// it reaches this type. A terminal panel renders these strings directly, so a
/// name carrying `\u{1b}[2J` would be a control-sequence injection into the
/// host's own UI — a malformed row therefore rejects the whole generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResourceDef {
    uri: String,
    name: String,
    title: Option<String>,
    description: Option<String>,
    mime_type: Option<String>,
    size: Option<u64>,
}

impl McpResourceDef {
    /// Unique resource identifier.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Programmatic name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Human-readable display name, when the server supplied one.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Model-facing description, when the server supplied one.
    #[must_use]
    pub fn description(&self) -> Option<&str> {
        self.description.as_deref()
    }

    /// Declared MIME type, when the server supplied one.
    #[must_use]
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// Declared raw size in bytes, when the server supplied one.
    #[must_use]
    pub const fn size(&self) -> Option<u64> {
        self.size
    }
}

/// One complete, unraced `resources/list` walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResourceGeneration {
    number: u64,
    committed_at_ms: u64,
    resources: Vec<McpResourceDef>,
}

impl McpResourceGeneration {
    /// Monotonic generation number; advances only at the swap commit point.
    #[must_use]
    pub const fn number(&self) -> u64 {
        self.number
    }

    /// Caller-observed commit timestamp.
    #[must_use]
    pub const fn committed_at_ms(&self) -> u64 {
        self.committed_at_ms
    }

    /// The complete listing, in server order.
    #[must_use]
    pub fn resources(&self) -> &[McpResourceDef] {
        &self.resources
    }
}

/// One entry of a `resources/read` result.
///
/// `Debug` is written by hand and prints no body. Resource content is
/// attacker-influenced data from an external server; a derived `Debug` would
/// let it reach a log through nothing more than a `{:?}` in a caller — the same
/// reasoning that gives `HttpResponse` a hand-written `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct McpResourceContent {
    uri: String,
    mime_type: Option<String>,
    body: McpResourceBody,
}

impl McpResourceContent {
    /// URI this entry describes; it may name a sub-resource of the request.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Declared MIME type, when the server supplied one.
    #[must_use]
    pub fn mime_type(&self) -> Option<&str> {
        self.mime_type.as_deref()
    }

    /// Untrusted body. Wrap text through [`UntrustedContentBoundary`] before it
    /// reaches a model.
    #[must_use]
    pub const fn body(&self) -> &McpResourceBody {
        &self.body
    }
}

impl std::fmt::Debug for McpResourceContent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResourceContent")
            .field("uri", &self.uri)
            .field("mime_type", &self.mime_type)
            .field("body", &self.body)
            .finish()
    }
}

/// Text or binary resource content.
///
/// `Debug` reports only a kind and a length, never bytes.
#[derive(Clone, PartialEq, Eq)]
pub enum McpResourceBody {
    /// `text` content, decoded UTF-8.
    Text(String),
    /// `blob` content, base64-decoded to raw bytes.
    Blob(Vec<u8>),
}

impl McpResourceBody {
    /// Text body, when this entry is textual.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            Self::Blob(_) => None,
        }
    }

    /// Decoded binary body, when this entry is binary.
    #[must_use]
    pub fn blob(&self) -> Option<&[u8]> {
        match self {
            Self::Text(_) => None,
            Self::Blob(bytes) => Some(bytes),
        }
    }

    /// Length of the decoded body in bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Text(text) => text.len(),
            Self::Blob(bytes) => bytes.len(),
        }
    }

    /// Whether the decoded body is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl std::fmt::Debug for McpResourceBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = match self {
            Self::Text(_) => "text",
            Self::Blob(_) => "blob",
        };
        write!(formatter, "McpResourceBody::{kind}({} bytes)", self.len())
    }
}

/// One complete `resources/read` result.
///
/// `Debug` is hand-written for the same reason as [`McpResourceContent`].
#[derive(Clone, PartialEq, Eq)]
pub struct McpResourceRead {
    uri: String,
    contents: Vec<McpResourceContent>,
}

impl McpResourceRead {
    /// The URI that was requested.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Every returned entry. A server may answer one request with several.
    #[must_use]
    pub fn contents(&self) -> &[McpResourceContent] {
        &self.contents
    }

    /// Total decoded body bytes across every entry.
    #[must_use]
    pub fn body_bytes(&self) -> usize {
        self.contents.iter().map(|content| content.body.len()).sum()
    }

    /// Render the textual bodies as one model-visible block inside `boundary`.
    ///
    /// The boundary is a **parameter**, not a constant minted here: core's
    /// [`UntrustedContentBoundary`] currently names only the public web as a
    /// data-only source, and labelling MCP resource content `UNTRUSTED WEB
    /// CONTENT` would be a false provenance claim. An MCP source belongs to the
    /// row that adds it to core; until then the caller states the provenance it
    /// can honestly assert.
    ///
    /// Binary entries contribute a deterministic byte-count placeholder rather
    /// than being silently dropped, and the placeholder contains no server text.
    #[must_use]
    pub fn render_for_model(&self, boundary: UntrustedContentBoundary) -> String {
        let joined = self
            .contents
            .iter()
            .map(|content| match &content.body {
                McpResourceBody::Text(text) => text.clone(),
                McpResourceBody::Blob(bytes) => {
                    format!("[binary resource content omitted: {} bytes]", bytes.len())
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        boundary.render_for_model(&joined)
    }
}

impl std::fmt::Debug for McpResourceRead {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResourceRead")
            .field("uri", &self.uri)
            .field("contents", &self.contents)
            .finish()
    }
}

/// One delivered `notifications/resources/updated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResourceUpdate {
    uri: String,
    observed_at_ms: u64,
}

impl McpResourceUpdate {
    /// URI the server reported as changed.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Caller-observed arrival timestamp.
    #[must_use]
    pub const fn observed_at_ms(&self) -> u64 {
        self.observed_at_ms
    }
}

/// What [`McpResourceRegistry::observe_notification`] did with one message.
///
/// There is deliberately no error arm and no `Result`: a notification arriving
/// for a resource nobody watches is the ordinary outcome of a server racing an
/// unsubscribe, and a malformed notification is the server's bug, not a reason
/// to tear down a working connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum McpNotificationOutcome {
    /// Not a resource notification; some other observer may own it.
    Ignored,
    /// `notifications/resources/list_changed`: the list epoch advanced, so any
    /// walk spanning this point is torn.
    ListChanged,
    /// `notifications/resources/updated` reached this many live subscriptions.
    Delivered {
        /// Live subscriptions the update was delivered to. Never zero.
        subscriptions: usize,
    },
    /// `notifications/resources/updated` for a URI with no live subscription.
    Unmatched,
    /// A resource notification whose params violated the schema.
    Malformed,
}

/// Whether the committed listing still backs a live subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSubscriptionState {
    /// The committed generation lists the subscribed URI.
    Active,
    /// The committed generation no longer lists it: the server dropped the
    /// resource without saying so. Delivery continues — the server remains
    /// authoritative about updates — but the row is no longer backed.
    Lapsed,
}

/// One live subscription, projected for inspection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSubscriptionRow {
    uri: String,
    state: McpSubscriptionState,
    updates: u64,
    last_update_ms: Option<u64>,
}

impl McpSubscriptionRow {
    /// Subscribed URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Whether the committed listing still backs this subscription.
    #[must_use]
    pub const fn state(&self) -> McpSubscriptionState {
        self.state
    }

    /// Updates delivered to this subscription.
    #[must_use]
    pub const fn updates(&self) -> u64 {
        self.updates
    }

    /// Arrival timestamp of the most recent delivered update.
    #[must_use]
    pub const fn last_update_ms(&self) -> Option<u64> {
        self.last_update_ms
    }
}

/// Everything a UI needs to render one server's resources, in one value.
///
/// Read from committed state only: an in-flight refresh is invisible here, so a
/// list that changes mid-read can never produce a torn view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpResourceInspection {
    capability: McpResourceCapability,
    generation: Option<Arc<McpResourceGeneration>>,
    subscriptions: Vec<McpSubscriptionRow>,
    dropped_updates: u64,
    list_change_epoch: u64,
}

impl McpResourceInspection {
    /// What `initialize` advertised. `Unknown` until a handshake is observed.
    #[must_use]
    pub const fn capability(&self) -> McpResourceCapability {
        self.capability
    }

    /// The last committed listing, when one exists.
    ///
    /// `None` with a `Supported` capability means heycode asked and has no answer
    /// yet — which is a different statement from "this server has none".
    #[must_use]
    pub fn generation(&self) -> Option<&McpResourceGeneration> {
        self.generation.as_deref()
    }

    /// Live subscriptions, sorted by URI.
    #[must_use]
    pub fn subscriptions(&self) -> &[McpSubscriptionRow] {
        &self.subscriptions
    }

    /// Updates that arrived for a URI with no live subscription, plus malformed
    /// resource notifications. A nonzero count is normal after an unsubscribe.
    #[must_use]
    pub const fn dropped_updates(&self) -> u64 {
        self.dropped_updates
    }

    /// Observed `notifications/resources/list_changed` count. A change while a
    /// walk is in flight discards that walk.
    #[must_use]
    pub const fn list_change_epoch(&self) -> u64 {
        self.list_change_epoch
    }
}

type Observer = Arc<dyn Fn(&McpResourceUpdate) + Send + Sync>;

struct SubscriptionEntry {
    token: Arc<()>,
    observer: Observer,
    updates: u64,
    last_update_ms: Option<u64>,
}

#[derive(Default)]
struct RegistryState {
    capability: Option<McpResourceCapability>,
    channel: Option<Arc<dyn McpRequestChannel>>,
    generation: Option<Arc<McpResourceGeneration>>,
    next_generation: u64,
    subscriptions: BTreeMap<String, SubscriptionEntry>,
    dropped_updates: u64,
}

struct RegistryInner {
    limits: McpResourceListLimits,
    watch: McpListChangeWatch,
    /// The swap lane. Held for a whole refresh so two walks cannot interleave;
    /// never held while `state` is locked.
    refresh: tokio::sync::Mutex<()>,
    state: Mutex<RegistryState>,
}

/// The single lifecycle owner of one server's resources and subscriptions.
///
/// Dropping the registry ends delivery: subscription entries live here, and a
/// notification cannot reach an observer through a registry that is gone.
pub struct McpResourceRegistry {
    inner: Arc<RegistryInner>,
}

impl std::fmt::Debug for McpResourceRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResourceRegistry")
            .field("limits", &self.inner.limits)
            .finish_non_exhaustive()
    }
}

impl Default for McpResourceRegistry {
    fn default() -> Self {
        Self::new(McpResourceListLimits::default())
    }
}

impl McpResourceRegistry {
    /// Build a registry with explicit listing bounds.
    ///
    /// The list-change epoch is private and advanced only by
    /// [`Self::observe_notification`]. It is deliberately not the tools watch:
    /// sharing one epoch would make a `tools/list_changed` discard a resource
    /// walk that nothing had invalidated.
    #[must_use]
    pub fn new(limits: McpResourceListLimits) -> Self {
        Self {
            inner: Arc::new(RegistryInner {
                limits,
                watch: McpListChangeWatch::new(),
                refresh: tokio::sync::Mutex::new(()),
                state: Mutex::new(RegistryState::default()),
            }),
        }
    }

    /// Walk the complete paginated resource list and atomically replace the
    /// committed generation.
    ///
    /// `capability` is `initialize` evidence, not walk evidence, so it is
    /// recorded before the walk: a server that advertises resources and then
    /// fails to list them must still inspect as "advertises resources, no
    /// listing", never as "no resources".
    ///
    /// The walk runs against the previous committed generation, which stays
    /// readable throughout. Only a complete, unraced candidate replaces it.
    ///
    /// # Errors
    /// Transport, timeout and cancellation from the channel; a JSON-RPC error
    /// mid-walk; a protocol or budget violation, including any malformed row,
    /// which rejects the whole candidate; a capability that does not prove
    /// resource support; and [`McpChannelError::Conflict`] when a
    /// `list_changed` notification lands during the walk.
    pub async fn refresh(
        &self,
        channel: Arc<dyn McpRequestChannel>,
        capability: McpResourceCapability,
        cancellation: &CancellationToken,
    ) -> Result<Arc<McpResourceGeneration>, McpChannelError> {
        let _lane = self.inner.refresh.lock().await;
        self.record_capability(capability)?;
        if !capability.resources().is_supported() {
            return Err(McpChannelError::protocol(
                "server did not advertise the resources capability",
            ));
        }
        let epoch = self.inner.watch.epoch();
        let resources =
            walk_resource_list(channel.as_ref(), self.inner.limits, cancellation).await?;
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        if self.inner.watch.epoch() != epoch {
            return Err(McpChannelError::Conflict);
        }
        self.commit(channel, resources)
    }

    /// Read one resource URI.
    ///
    /// # Errors
    /// An invalid URI; no connection bound by a prior successful
    /// [`Self::refresh`]; transport/timeout/cancellation; a JSON-RPC error such
    /// as the legacy `-32002` resource-not-found; and any protocol or size
    /// violation. An oversized payload is refused, never truncated.
    pub async fn read(
        &self,
        uri: &str,
        cancellation: &CancellationToken,
    ) -> Result<McpResourceRead, McpChannelError> {
        let uri = validate_uri(uri)?;
        let channel = self.channel()?;
        let result = channel
            .call(METHOD_READ, serde_json::json!({ "uri": uri }), cancellation)
            .await?;
        let contents = parse_read_result(&result)?;
        Ok(McpResourceRead { uri, contents })
    }

    /// Subscribe to `notifications/resources/updated` for one URI.
    ///
    /// The registration is owned by `context`: rollback or shutdown removes the
    /// routing entry and `observer` stops being called. The returned handle is
    /// a view onto that registration, not a second owner, so dropping it leaves
    /// the subscription live and disposing the context ends it either way.
    ///
    /// `observer` runs on the thread that observed the notification, outside
    /// every registry lock, so it may call back into this registry. A panicking
    /// observer is contained and its peers still run, matching `EventBus`.
    ///
    /// # Errors
    /// An invalid URI; a capability that is not explicitly `Supported` —
    /// `Unknown` never becomes supported; no bound connection; a duplicate live
    /// subscription or an exhausted subscription budget
    /// ([`McpChannelError::Conflict`]); and any transport or JSON-RPC failure
    /// of `resources/subscribe`. Nothing is registered unless the server
    /// acknowledged.
    pub async fn subscribe(
        &self,
        context: &Context,
        uri: &str,
        observer: impl Fn(&McpResourceUpdate) + Send + Sync + 'static,
        cancellation: &CancellationToken,
    ) -> Result<McpResourceSubscription, McpChannelError> {
        let uri = validate_uri(uri)?;
        let channel = {
            let state = self.state()?;
            match state.capability {
                Some(capability) if capability.subscribe().is_supported() => {}
                _ => {
                    return Err(McpChannelError::protocol(
                        "server did not advertise resource subscriptions",
                    ));
                }
            }
            if state.subscriptions.contains_key(&uri) {
                return Err(McpChannelError::Conflict);
            }
            if state.subscriptions.len() >= MAX_SUBSCRIPTIONS {
                return Err(McpChannelError::protocol(
                    "resource subscriptions exceed the configured budget",
                ));
            }
            state.channel.clone().ok_or(McpChannelError::protocol(
                "resource subscription requires a connection bound by a successful listing",
            ))?
        };

        let _acknowledged = channel
            .call(
                METHOD_SUBSCRIBE,
                serde_json::json!({ "uri": uri }),
                cancellation,
            )
            .await?;

        let token = Arc::new(());
        {
            let mut state = self.state()?;
            if state.subscriptions.contains_key(&uri) {
                return Err(McpChannelError::Conflict);
            }
            state.subscriptions.insert(
                uri.clone(),
                SubscriptionEntry {
                    token: Arc::clone(&token),
                    observer: Arc::new(observer),
                    updates: 0,
                    last_update_ms: None,
                },
            );
        }
        let registration = SubscriptionRegistration {
            inner: Arc::downgrade(&self.inner),
            uri: uri.clone(),
            token: Arc::clone(&token),
        };
        context.effect(move || drop(registration));
        Ok(McpResourceSubscription {
            inner: Arc::downgrade(&self.inner),
            uri,
            token,
        })
    }

    /// Route one server-to-client notification.
    ///
    /// Returns what happened; it cannot fail. A `list_changed` advances the
    /// epoch whether or not the server advertised `listChanged`, because
    /// treating an undeclared notification as noise would publish a torn walk,
    /// while honouring it only discards a candidate.
    pub fn observe_notification(&self, message: &serde_json::Value) -> McpNotificationOutcome {
        let method = message.get("method").and_then(serde_json::Value::as_str);
        match method {
            Some(NOTIFICATION_RESOURCE_LIST_CHANGED) => {
                self.inner.watch.mark_changed();
                McpNotificationOutcome::ListChanged
            }
            Some(NOTIFICATION_RESOURCE_UPDATED) => self.deliver_update(message),
            _ => McpNotificationOutcome::Ignored,
        }
    }

    /// Project committed state for a UI.
    #[must_use]
    pub fn inspect(&self) -> McpResourceInspection {
        let Ok(state) = self.inner.state.lock() else {
            return McpResourceInspection {
                capability: McpResourceCapability::UNKNOWN,
                generation: None,
                subscriptions: Vec::new(),
                dropped_updates: 0,
                list_change_epoch: self.inner.watch.epoch(),
            };
        };
        let listed: BTreeSet<&str> = state.generation.as_ref().map_or_else(BTreeSet::new, |g| {
            g.resources.iter().map(|row| row.uri.as_str()).collect()
        });
        let subscriptions = state
            .subscriptions
            .iter()
            .map(|(uri, entry)| McpSubscriptionRow {
                uri: uri.clone(),
                // A subscription cannot exist without a committed listing —
                // subscribing needs the connection that a listing binds — so
                // there is no third "nothing is known" arm to model.
                state: if listed.contains(uri.as_str()) {
                    McpSubscriptionState::Active
                } else {
                    McpSubscriptionState::Lapsed
                },
                updates: entry.updates,
                last_update_ms: entry.last_update_ms,
            })
            .collect();
        McpResourceInspection {
            capability: state.capability.unwrap_or(McpResourceCapability::UNKNOWN),
            generation: state.generation.clone(),
            subscriptions,
            dropped_updates: state.dropped_updates,
            list_change_epoch: self.inner.watch.epoch(),
        }
    }

    fn deliver_update(&self, message: &serde_json::Value) -> McpNotificationOutcome {
        let uri = message
            .get("params")
            .and_then(|params| params.get("uri"))
            .and_then(serde_json::Value::as_str)
            .map(validate_uri);
        let Some(Ok(uri)) = uri else {
            self.note_dropped();
            return McpNotificationOutcome::Malformed;
        };
        let observed_at_ms = crate::unix_time_ms();
        let observers = {
            let Ok(mut state) = self.inner.state.lock() else {
                return McpNotificationOutcome::Unmatched;
            };
            match state.subscriptions.get_mut(&uri) {
                Some(entry) => {
                    entry.updates = entry.updates.saturating_add(1);
                    entry.last_update_ms = Some(observed_at_ms);
                    vec![Arc::clone(&entry.observer)]
                }
                None => {
                    state.dropped_updates = state.dropped_updates.saturating_add(1);
                    Vec::new()
                }
            }
        };
        if observers.is_empty() {
            return McpNotificationOutcome::Unmatched;
        }
        let update = McpResourceUpdate {
            uri,
            observed_at_ms,
        };
        for observer in &observers {
            // A misbehaving observer must not take down the transport driver
            // that is routing this notification, exactly as `EventBus::emit`
            // contains listener panics.
            let _contained = catch_unwind(AssertUnwindSafe(|| observer(&update)));
        }
        McpNotificationOutcome::Delivered {
            subscriptions: observers.len(),
        }
    }

    fn note_dropped(&self) {
        if let Ok(mut state) = self.inner.state.lock() {
            state.dropped_updates = state.dropped_updates.saturating_add(1);
        }
    }

    fn record_capability(&self, capability: McpResourceCapability) -> Result<(), McpChannelError> {
        self.state()?.capability = Some(capability);
        Ok(())
    }

    fn channel(&self) -> Result<Arc<dyn McpRequestChannel>, McpChannelError> {
        self.state()?
            .channel
            .clone()
            .ok_or(McpChannelError::protocol(
                "resource read requires a connection bound by a successful listing",
            ))
    }

    fn commit(
        &self,
        channel: Arc<dyn McpRequestChannel>,
        resources: Vec<McpResourceDef>,
    ) -> Result<Arc<McpResourceGeneration>, McpChannelError> {
        let mut state = self.state()?;
        let number = state
            .next_generation
            .checked_add(1)
            .ok_or(McpChannelError::protocol(
                "resource generation counter is exhausted",
            ))?;
        let generation = Arc::new(McpResourceGeneration {
            number,
            committed_at_ms: crate::unix_time_ms(),
            resources,
        });
        state.next_generation = number;
        state.channel = Some(channel);
        state.generation = Some(Arc::clone(&generation));
        Ok(generation)
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, RegistryState>, McpChannelError> {
        self.inner
            .state
            .lock()
            .map_err(|_| McpChannelError::protocol("resource registry state is unavailable"))
    }
}

/// A view onto one effect-owned subscription.
///
/// Holding this does not keep the subscription alive and dropping it does not
/// end one — the owning [`Context`] effect decides that. It exists so a caller
/// can end its own subscription early and ask whether one is still live.
#[derive(Clone)]
pub struct McpResourceSubscription {
    inner: Weak<RegistryInner>,
    uri: String,
    token: Arc<()>,
}

impl std::fmt::Debug for McpResourceSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpResourceSubscription")
            .field("uri", &self.uri)
            .field("live", &self.is_live())
            .finish()
    }
}

impl McpResourceSubscription {
    /// Subscribed URI.
    #[must_use]
    pub fn uri(&self) -> &str {
        &self.uri
    }

    /// Whether this exact registration is still routing updates.
    ///
    /// False once the owning effect ran, once [`Self::unsubscribe`] completed,
    /// or once a replacement subscription claimed the same URI.
    #[must_use]
    pub fn is_live(&self) -> bool {
        let Some(inner) = self.inner.upgrade() else {
            return false;
        };
        let Ok(state) = inner.state.lock() else {
            return false;
        };
        state
            .subscriptions
            .get(&self.uri)
            .is_some_and(|entry| Arc::ptr_eq(&entry.token, &self.token))
    }

    /// End this subscription and tell the server.
    ///
    /// The routing entry is removed **before** the request is sent, so once
    /// this returns no further update can reach the observer whether or not the
    /// server answered. A failed `resources/unsubscribe` therefore leaves a
    /// server that may keep sending; those notifications are dropped safely.
    ///
    /// Idempotent: unsubscribing something already disposed succeeds silently,
    /// because a caller racing context shutdown has not made an error.
    ///
    /// # Errors
    /// Transport, timeout, cancellation or a JSON-RPC error from
    /// `resources/unsubscribe`.
    pub async fn unsubscribe(
        &self,
        cancellation: &CancellationToken,
    ) -> Result<(), McpChannelError> {
        let Some(inner) = self.inner.upgrade() else {
            return Ok(());
        };
        let channel = {
            let Ok(mut state) = inner.state.lock() else {
                return Err(McpChannelError::protocol(
                    "resource registry state is unavailable",
                ));
            };
            if !remove_matching(&mut state, &self.uri, &self.token) {
                return Ok(());
            }
            state.channel.clone()
        };
        let Some(channel) = channel else {
            return Ok(());
        };
        let _acknowledged = channel
            .call(
                METHOD_UNSUBSCRIBE,
                serde_json::json!({ "uri": self.uri }),
                cancellation,
            )
            .await?;
        Ok(())
    }
}

/// The subscription's actual owner, moved into a [`Context`] effect.
///
/// Disposal is synchronous by necessity — a context disposer cannot await — so
/// it removes the routing entry and does not send `resources/unsubscribe`. A
/// server that keeps sending afterwards is answered by dropping the update,
/// which is the same path an in-flight unsubscribe race takes.
struct SubscriptionRegistration {
    inner: Weak<RegistryInner>,
    uri: String,
    token: Arc<()>,
}

impl Drop for SubscriptionRegistration {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let Ok(mut state) = inner.state.lock() else {
            return;
        };
        let _removed = remove_matching(&mut state, &self.uri, &self.token);
    }
}

/// Remove `uri` only when it is still this exact registration.
///
/// Token identity is what stops a disposed handle from removing the
/// replacement subscription that took its URI.
fn remove_matching(state: &mut RegistryState, uri: &str, token: &Arc<()>) -> bool {
    let matches = state
        .subscriptions
        .get(uri)
        .is_some_and(|entry| Arc::ptr_eq(&entry.token, token));
    if matches {
        state.subscriptions.remove(uri);
    }
    matches
}

struct ResourcePage {
    resources: Vec<McpResourceDef>,
    next_cursor: Option<String>,
}

async fn walk_resource_list(
    channel: &dyn McpRequestChannel,
    limits: McpResourceListLimits,
    cancellation: &CancellationToken,
) -> Result<Vec<McpResourceDef>, McpChannelError> {
    let mut resources: Vec<McpResourceDef> = Vec::new();
    let mut cursor: Option<String> = None;
    let mut visited: BTreeSet<String> = BTreeSet::new();
    let mut pages: u32 = 0;
    loop {
        if cancellation.is_cancelled() {
            return Err(McpChannelError::Cancelled);
        }
        pages = pages.saturating_add(1);
        if pages > limits.max_pages.get() {
            return Err(McpChannelError::protocol(
                "resource list exceeds the configured page budget",
            ));
        }
        let params = match &cursor {
            Some(cursor) => serde_json::json!({ "cursor": cursor }),
            None => serde_json::json!({}),
        };
        let result = channel.call(METHOD_LIST, params, cancellation).await?;
        let page = parse_resource_page(&result, limits)?;
        if resources.len().saturating_add(page.resources.len())
            > limits.max_resources.get() as usize
        {
            return Err(McpChannelError::protocol(
                "resource list exceeds the configured resource budget",
            ));
        }
        resources.extend(page.resources);
        // Absence or null ends the walk. An empty string is a valid opaque
        // cursor and means more results follow.
        let Some(next) = page.next_cursor else {
            return Ok(resources);
        };
        if !visited.insert(next.clone()) {
            return Err(McpChannelError::protocol(
                "resource list repeated a pagination cursor",
            ));
        }
        cursor = Some(next);
    }
}

fn parse_resource_page(
    result: &serde_json::Value,
    limits: McpResourceListLimits,
) -> Result<ResourcePage, McpChannelError> {
    let result = result.as_object().ok_or(McpChannelError::protocol(
        "resources/list result must be an object",
    ))?;
    let rows = result
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .ok_or(McpChannelError::protocol(
            "resources/list result must contain a resources array",
        ))?;
    if rows.len() > limits.max_page_resources.get() as usize {
        return Err(McpChannelError::protocol(
            "resources/list page exceeds the configured page-size budget",
        ));
    }
    let mut resources = Vec::with_capacity(rows.len());
    for row in rows {
        resources.push(parse_resource(row)?);
    }
    let next_cursor = match result.get("nextCursor") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(cursor)) if cursor.len() <= MAX_CURSOR_BYTES => {
            Some(cursor.clone())
        }
        Some(_) => {
            return Err(McpChannelError::protocol(
                "nextCursor must be absent, null or a bounded opaque string",
            ));
        }
    };
    Ok(ResourcePage {
        resources,
        next_cursor,
    })
}

fn parse_resource(row: &serde_json::Value) -> Result<McpResourceDef, McpChannelError> {
    let row = row.as_object().ok_or(McpChannelError::protocol(
        "resource definition must be an object",
    ))?;
    let uri = validate_uri(row.get("uri").and_then(serde_json::Value::as_str).ok_or(
        McpChannelError::protocol("resource definition must carry a string uri"),
    )?)?;
    let name =
        display_text(
            row.get("name").and_then(serde_json::Value::as_str).ok_or(
                McpChannelError::protocol("resource definition must carry a string name"),
            )?,
            "resource name must be bounded trimmed control-free text",
        )?;
    let title = optional_display(row.get("title"), "resource title")?;
    let mime_type = optional_display(row.get("mimeType"), "resource mimeType")?;
    let description = match row.get("description") {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(text)) if valid_multiline(text) => Some(text.clone()),
        Some(_) => {
            return Err(McpChannelError::protocol(
                "resource description must be bounded text with ordinary whitespace only",
            ));
        }
    };
    let size = match row.get("size") {
        None | Some(serde_json::Value::Null) => None,
        Some(value) => Some(value.as_u64().ok_or(McpChannelError::protocol(
            "resource size must be a non-negative integer",
        ))?),
    };
    Ok(McpResourceDef {
        uri,
        name,
        title,
        description,
        mime_type,
        size,
    })
}

fn parse_read_result(
    result: &serde_json::Value,
) -> Result<Vec<McpResourceContent>, McpChannelError> {
    let rows = result
        .as_object()
        .and_then(|result| result.get("contents"))
        .and_then(serde_json::Value::as_array)
        .ok_or(McpChannelError::protocol(
            "resources/read result must contain a contents array",
        ))?;
    if rows.len() > MAX_READ_CONTENTS {
        return Err(McpChannelError::protocol(
            "resources/read result exceeds the configured entry budget",
        ));
    }
    let mut contents = Vec::with_capacity(rows.len());
    let mut total: usize = 0;
    for row in rows {
        let content = parse_read_content(row)?;
        total = total.saturating_add(content.body.len());
        if total > MAX_READ_BYTES {
            return Err(McpChannelError::protocol(
                "resources/read result exceeds the 4194304-byte bound",
            ));
        }
        contents.push(content);
    }
    Ok(contents)
}

fn parse_read_content(row: &serde_json::Value) -> Result<McpResourceContent, McpChannelError> {
    let row = row.as_object().ok_or(McpChannelError::protocol(
        "resource contents entry must be an object",
    ))?;
    let uri = validate_uri(row.get("uri").and_then(serde_json::Value::as_str).ok_or(
        McpChannelError::protocol("resource contents entry must carry a string uri"),
    )?)?;
    let mime_type = optional_display(row.get("mimeType"), "resource contents mimeType")?;
    let body = match (row.get("text"), row.get("blob")) {
        // The schema is a union of TextResourceContents and
        // BlobResourceContents. Both arms present is ambiguous and neither is
        // empty content — both reject rather than guess.
        (Some(_), Some(_)) => {
            return Err(McpChannelError::protocol(
                "resource contents entry must carry exactly one of text or blob",
            ));
        }
        (Some(text), None) => {
            let text = text.as_str().ok_or(McpChannelError::protocol(
                "resource contents text must be a string",
            ))?;
            if text.len() > MAX_CONTENT_BYTES {
                return Err(McpChannelError::protocol(
                    "resource contents entry exceeds the 1048576-byte bound",
                ));
            }
            McpResourceBody::Text(text.to_owned())
        }
        (None, Some(blob)) => {
            let blob = blob.as_str().ok_or(McpChannelError::protocol(
                "resource contents blob must be a base64 string",
            ))?;
            // Bound the encoded form first: decoding is the normalization step,
            // and an oversized payload is refused before it happens.
            if blob.len() > MAX_CONTENT_BYTES {
                return Err(McpChannelError::protocol(
                    "resource contents entry exceeds the 1048576-byte bound",
                ));
            }
            McpResourceBody::Blob(
                base64::engine::general_purpose::STANDARD
                    .decode(blob)
                    .map_err(|_| {
                        McpChannelError::protocol("resource contents blob must be valid base64")
                    })?,
            )
        }
        (None, None) => {
            return Err(McpChannelError::protocol(
                "resource contents entry must carry exactly one of text or blob",
            ));
        }
    };
    Ok(McpResourceContent {
        uri,
        mime_type,
        body,
    })
}

fn validate_uri(uri: &str) -> Result<String, McpChannelError> {
    if uri.is_empty()
        || uri.len() > MAX_URI_BYTES
        || uri.trim() != uri
        || uri.chars().any(char::is_control)
    {
        return Err(McpChannelError::protocol(
            "resource uri must be 1..=2048 trimmed control-free bytes",
        ));
    }
    Ok(uri.to_owned())
}

fn optional_display(
    value: Option<&serde_json::Value>,
    requirement: &'static str,
) -> Result<Option<String>, McpChannelError> {
    match value {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(text)) => display_text(text, requirement).map(Some),
        Some(_) => Err(McpChannelError::Protocol { requirement }),
    }
}

/// One-line display metadata a terminal panel renders verbatim.
fn display_text(value: &str, requirement: &'static str) -> Result<String, McpChannelError> {
    if value.is_empty()
        || value.len() > MAX_DISPLAY_BYTES
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(McpChannelError::Protocol { requirement });
    }
    Ok(value.to_owned())
}

/// Descriptions may wrap, so newlines and tabs are ordinary whitespace here;
/// every other control character is still refused.
fn valid_multiline(value: &str) -> bool {
    value.len() <= MAX_DESCRIPTION_BYTES
        && !value
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}
