//! Exact and estimated token counting: the evidence vocabulary, the
//! effect-owned counter registry and the always-available local estimator.
//!
//! A token count is EVIDENCE, and the two kinds are different types. An
//! [`ExactTokenCount`] is a number the provider itself produced — a dedicated
//! count-tokens endpoint, or a usage figure returned with a response. An
//! [`EstimatedTokenCount`] is a number an explicitly inexact provider endpoint
//! or local heuristic produced, and it always carries the [`EstimationMethod`]
//! that produced it. There is no
//! accessor that yields a bare number without naming which of the two it is.
//!
//! Nothing outside this module can construct either count. A [`TokenCounter`]
//! returns a plain `u64`; [`TokenCounterRegistry::count`] mints the typed
//! count from the counter's DECLARED evidence, so an implementation cannot
//! present a guess as a measurement — only its own declaration can, and that
//! declaration is visible in every listing and is what selection ranks by.
//!
//! Selection is deterministic: the eligible counters for a provider/model are
//! ordered by declared evidence and then by stable id, never by registration
//! order. A counter that cannot honestly serve a request refuses distinctly
//! instead of returning a number, and a refusal is recorded in the outcome so
//! a fallback is never silent. A counter FAILURE never degrades into an
//! estimate: the caller is told, and decides.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use heycode_core::Context;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::{ChatMessage, ChatRequest, ToolSpec};

/// UTF-8 bytes [`HeuristicTokenEstimator`] charges per token.
const UTF8_BYTES_PER_TOKEN: u64 = 4;

/// Longest accepted [`TokenCounterId`], in bytes.
const MAX_COUNTER_ID_BYTES: usize = 64;

/// Longest safe diagnostic retained from a counter refusal or failure.
const MAX_FAILURE_MESSAGE_BYTES: usize = 256;

/// Safe token counting failure. Messages never contain provider bodies.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TokenCountError {
    /// Registration or request data is not valid.
    #[error("invalid token counting {field}: {message}")]
    Invalid {
        /// Field that failed validation.
        field: &'static str,
        /// Safe structural diagnostic.
        message: String,
    },
    /// Two counters claimed one id.
    #[error("token counter `{id}` is already registered")]
    DuplicateCounter {
        /// Contested counter id.
        id: TokenCounterId,
    },
    /// No registered counter declares that it serves this provider/model.
    #[error("no token counter serves provider `{provider}` model `{model}`")]
    NoCounter {
        /// Requested provider id.
        provider: String,
        /// Requested model id.
        model: String,
    },
    /// Every counter serving the target refused this request's content.
    #[error("every token counter serving provider `{provider}` model `{model}` refused")]
    Unsupported {
        /// Requested provider id.
        provider: String,
        /// Requested model id.
        model: String,
        /// Each refusal, best-ranked counter first.
        refusals: Vec<TokenCountRefusal>,
    },
    /// The best available counter failed. This never degrades to an estimate.
    #[error("token counter `{counter}` failed: {message}")]
    CounterFailed {
        /// Counter that failed.
        counter: TokenCounterId,
        /// Safe one-line diagnostic.
        message: String,
    },
    /// Counting was cancelled before any number existed.
    #[error("token counter `{counter}` was cancelled")]
    Cancelled {
        /// Counter that was cancelled.
        counter: TokenCounterId,
    },
    /// Registry state is poisoned.
    #[error("token counter registry is unavailable")]
    RegistryUnavailable,
}

/// Stable id of one registered token counter. Ties in evidence rank are broken
/// by this id, so it is part of the selection contract, not a label.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TokenCounterId(String);

impl TokenCounterId {
    /// Validate one counter id without rewriting it.
    ///
    /// # Errors
    /// Returns [`TokenCountError::Invalid`] for blank, overlong ids or ids
    /// containing anything but lowercase ASCII letters, digits, `-`, `_`, `.`
    /// and `:`. Case is not folded, so two spellings never collide silently.
    pub fn new(value: impl Into<String>) -> Result<Self, TokenCountError> {
        let value = value.into();
        let valid = |byte: u8| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'.' | b':')
        };
        if value.is_empty() || value.len() > MAX_COUNTER_ID_BYTES || !value.bytes().all(valid) {
            return Err(TokenCountError::Invalid {
                field: "counter id",
                message: format!("id must be 1..={MAX_COUNTER_ID_BYTES} bytes of `a-z0-9-_.:`"),
            });
        }
        Ok(Self(value))
    }

    /// Borrow the id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Id of a counter shipped in this crate, valid by construction.
    fn built_in(value: &'static str) -> Self {
        Self(value.to_owned())
    }
}

impl fmt::Display for TokenCounterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// How an explicitly inexact count was derived.
///
/// Variants are declared best-first, and [`TokenEvidence`] orders by that
/// declaration: a counter declaring an earlier method deterministically
/// supersedes one declaring a later method for the same target. Adding a
/// method is a deliberate compiler-enforced break for every matcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EstimationMethod {
    /// A provider tokenizer/count endpoint produced a documented estimate.
    /// This outranks local heuristics but remains non-exact.
    ProviderTokenizer,
    /// A fixed ratio of UTF-8 bytes per token produced the number.
    ///
    /// Declaration order is the rank: a better estimation method added later
    /// sorts ahead of this one. Adding a variant is a deliberate
    /// compiler-enforced break that forces every match to be revisited, which
    /// is why no placeholder rank is reserved for a counter that does not
    /// exist yet.
    Utf8ByteRatio,
}

impl EstimationMethod {
    /// Stable name for diagnostics and display.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ProviderTokenizer => "provider_tokenizer",
            Self::Utf8ByteRatio => "utf8_byte_ratio",
        }
    }
}

/// What a counter's numbers are worth.
///
/// Ordering is best-evidence-first: [`Self::Exact`] sorts ahead of every
/// estimate and estimates sort by [`EstimationMethod`] declaration order.
/// Selection reads this order, so a better counter supersedes a worse one
/// deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TokenEvidence {
    /// The provider counted these tokens: a dedicated count-tokens endpoint,
    /// or a usage figure the provider returned with a response.
    Exact,
    /// An explicitly inexact provider endpoint or local heuristic produced the
    /// number by the named method.
    Estimated(EstimationMethod),
}

impl TokenEvidence {
    /// True only for a provider-measured count. An estimate is never exact,
    /// however good its method.
    #[must_use]
    pub const fn is_exact(self) -> bool {
        matches!(self, Self::Exact)
    }
}

/// A token count the provider itself measured.
///
/// Only [`TokenCounterRegistry::count`] mints one, and only for a counter
/// whose descriptor declares [`TokenEvidence::Exact`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactTokenCount {
    counter: TokenCounterId,
    tokens: u64,
}

impl ExactTokenCount {
    /// Number of tokens the provider measured.
    #[must_use]
    pub const fn tokens(&self) -> u64 {
        self.tokens
    }

    /// Counter that produced this measurement.
    #[must_use]
    pub const fn counter(&self) -> &TokenCounterId {
        &self.counter
    }
}

/// An explicitly inexact token count carrying the method that produced it so
/// provider and local estimates remain distinguishable and rankable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EstimatedTokenCount {
    counter: TokenCounterId,
    tokens: u64,
    method: EstimationMethod,
}

impl EstimatedTokenCount {
    /// Estimated number of tokens. This is not an exact measurement.
    #[must_use]
    pub const fn tokens(&self) -> u64 {
        self.tokens
    }

    /// How the estimate was derived.
    #[must_use]
    pub const fn method(&self) -> EstimationMethod {
        self.method
    }

    /// Counter that produced this estimate.
    #[must_use]
    pub const fn counter(&self) -> &TokenCounterId {
        &self.counter
    }
}

/// One token count and the evidence class it belongs to.
///
/// There is deliberately no accessor yielding a number without stating which
/// arm it came from: a consumer that needs the figure names the evidence it is
/// willing to accept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCount {
    /// The provider measured this count.
    Exact(ExactTokenCount),
    /// A local heuristic estimated this count.
    Estimated(EstimatedTokenCount),
}

impl TokenCount {
    /// The provider-measured count, or `None` when this is an estimate.
    #[must_use]
    pub fn exact(&self) -> Option<&ExactTokenCount> {
        match self {
            Self::Exact(exact) => Some(exact),
            Self::Estimated(_) => None,
        }
    }

    /// The estimate, or `None` when the provider measured this count.
    #[must_use]
    pub fn estimated(&self) -> Option<&EstimatedTokenCount> {
        match self {
            Self::Exact(_) => None,
            Self::Estimated(estimate) => Some(estimate),
        }
    }

    /// Evidence class of this count.
    #[must_use]
    pub fn evidence(&self) -> TokenEvidence {
        match self {
            Self::Exact(_) => TokenEvidence::Exact,
            Self::Estimated(estimate) => TokenEvidence::Estimated(estimate.method()),
        }
    }

    /// Counter that produced this count.
    #[must_use]
    pub fn counter(&self) -> &TokenCounterId {
        match self {
            Self::Exact(exact) => exact.counter(),
            Self::Estimated(estimate) => estimate.counter(),
        }
    }
}

/// Distinct reasons one counter produced no number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenCountFailureKind {
    /// This counter cannot honestly count this request and will not guess.
    /// Selection moves on to the next-ranked counter and records the refusal.
    Unsupported,
    /// The counter's own operation failed. Selection stops rather than
    /// degrading a failed measurement into an estimate.
    Failed,
    /// Cancelled before a number existed. Never a successful count.
    Cancelled,
}

/// Why one counter produced no number.
///
/// Diagnostics are safe, bounded and one line: never a provider body, URL,
/// query or credential.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCountFailure {
    kind: TokenCountFailureKind,
    message: String,
}

impl TokenCountFailure {
    /// This counter does not serve the request and refuses to guess.
    #[must_use]
    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(TokenCountFailureKind::Unsupported, message)
    }

    /// The counter's own operation failed.
    #[must_use]
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(TokenCountFailureKind::Failed, message)
    }

    /// Cancelled before a number existed.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::new(TokenCountFailureKind::Cancelled, "cancelled")
    }

    fn new(kind: TokenCountFailureKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: bounded_message(message.into()),
        }
    }

    /// Which kind of failure this is.
    #[must_use]
    pub const fn kind(&self) -> TokenCountFailureKind {
        self.kind
    }

    /// Safe bounded one-line diagnostic.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// One counter that declined a request, retained so a fallback to a worse
/// counter is visible in the result rather than silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCountRefusal {
    counter: TokenCounterId,
    message: String,
}

impl TokenCountRefusal {
    /// Counter that declined.
    #[must_use]
    pub const fn counter(&self) -> &TokenCounterId {
        &self.counter
    }

    /// Safe bounded reason it declined.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// Which provider/model a counter declares it serves.
///
/// Scope decides eligibility only. It deliberately does not outrank evidence:
/// a model-specific heuristic never wins over a provider-measured count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TokenCounterScope {
    /// Serves every provider and model. Only a provider-independent local
    /// estimator may honestly claim this.
    Any,
    /// Serves every model of exactly one provider.
    Provider(String),
    /// Serves exactly one model of one provider.
    Model {
        /// Provider id served.
        provider: String,
        /// Model id served.
        model: String,
    },
}

impl TokenCounterScope {
    /// Serve every model of one provider.
    ///
    /// # Errors
    /// Returns [`TokenCountError::Invalid`] for a blank or
    /// surrounding-whitespace provider id.
    pub fn provider(provider: impl Into<String>) -> Result<Self, TokenCountError> {
        Ok(Self::Provider(validated_id("scope provider", provider)?))
    }

    /// Serve exactly one model of one provider.
    ///
    /// # Errors
    /// Returns [`TokenCountError::Invalid`] for a blank or
    /// surrounding-whitespace provider or model id.
    pub fn model(
        provider: impl Into<String>,
        model: impl Into<String>,
    ) -> Result<Self, TokenCountError> {
        Ok(Self::Model {
            provider: validated_id("scope provider", provider)?,
            model: validated_id("scope model", model)?,
        })
    }

    /// Whether this scope covers an exact provider/model pair.
    #[must_use]
    pub fn serves(&self, provider: &str, model: &str) -> bool {
        match self {
            Self::Any => true,
            Self::Provider(served) => served == provider,
            Self::Model {
                provider: served_provider,
                model: served_model,
            } => served_provider == provider && served_model == model,
        }
    }
}

/// What one counter declares about itself. Selection reads this and nothing
/// else, so it must not change between calls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCounterDescriptor {
    id: TokenCounterId,
    evidence: TokenEvidence,
    scope: TokenCounterScope,
}

impl TokenCounterDescriptor {
    /// Declare one counter's identity, evidence class and served scope.
    ///
    /// # Errors
    /// Returns [`TokenCountError::Invalid`] when the scope carries a blank or
    /// surrounding-whitespace provider/model id.
    pub fn new(
        id: TokenCounterId,
        evidence: TokenEvidence,
        scope: TokenCounterScope,
    ) -> Result<Self, TokenCountError> {
        match &scope {
            TokenCounterScope::Any => {}
            TokenCounterScope::Provider(provider) => {
                validated_id("scope provider", provider.clone())?;
            }
            TokenCounterScope::Model { provider, model } => {
                validated_id("scope provider", provider.clone())?;
                validated_id("scope model", model.clone())?;
            }
        }
        Ok(Self {
            id,
            evidence,
            scope,
        })
    }

    /// Stable id used to break evidence ties.
    #[must_use]
    pub const fn id(&self) -> &TokenCounterId {
        &self.id
    }

    /// Declared evidence class. This alone decides whether a count reads back
    /// as exact.
    #[must_use]
    pub const fn evidence(&self) -> TokenEvidence {
        self.evidence
    }

    /// Declared served scope.
    #[must_use]
    pub const fn scope(&self) -> &TokenCounterScope {
        &self.scope
    }
}

/// One part of a request whose tokens are being counted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CountableContent<'a> {
    /// Free text the request sends outside the transcript.
    Text(&'a str),
    /// One transcript message exactly as it will be sent.
    Message(&'a ChatMessage),
    /// One tool definition exactly as it will be offered.
    Tool(&'a ToolSpec),
}

/// Everything one count call must measure, and the provider/model it is for.
///
/// The parts are borrowed: counting a transcript must not deep-copy megabytes
/// of message text or image bytes on every pressure check.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenCountRequest<'a> {
    provider: &'a str,
    model: &'a str,
    content: Vec<CountableContent<'a>>,
}

impl<'a> TokenCountRequest<'a> {
    /// Start an empty count request for one provider/model.
    ///
    /// # Errors
    /// Returns [`TokenCountError::Invalid`] for a blank or
    /// surrounding-whitespace provider or model id.
    pub fn new(provider: &'a str, model: &'a str) -> Result<Self, TokenCountError> {
        if provider.is_empty() || provider.trim() != provider {
            return Err(TokenCountError::Invalid {
                field: "provider",
                message: "provider id must be non-blank with no surrounding whitespace".to_owned(),
            });
        }
        if model.is_empty() || model.trim() != model {
            return Err(TokenCountError::Invalid {
                field: "model",
                message: "model id must be non-blank with no surrounding whitespace".to_owned(),
            });
        }
        Ok(Self {
            provider,
            model,
            content: Vec::new(),
        })
    }

    /// Count exactly what one [`ChatRequest`] sends: every message in order,
    /// then every offered tool definition.
    ///
    /// # Errors
    /// Returns [`TokenCountError::Invalid`] for a blank provider id or a
    /// request whose model id is blank.
    pub fn for_chat_request(
        provider: &'a str,
        request: &'a ChatRequest,
    ) -> Result<Self, TokenCountError> {
        let mut countable = Self::new(provider, request.model.as_str())?;
        for message in &request.messages {
            countable = countable.with_message(message);
        }
        if let Some(tools) = request.tools.as_ref() {
            for tool in tools {
                countable = countable.with_tool(tool);
            }
        }
        Ok(countable)
    }

    /// Add free text the request sends.
    #[must_use]
    pub fn with_text(mut self, text: &'a str) -> Self {
        self.content.push(CountableContent::Text(text));
        self
    }

    /// Add one transcript message.
    #[must_use]
    pub fn with_message(mut self, message: &'a ChatMessage) -> Self {
        self.content.push(CountableContent::Message(message));
        self
    }

    /// Add one offered tool definition.
    #[must_use]
    pub fn with_tool(mut self, tool: &'a ToolSpec) -> Self {
        self.content.push(CountableContent::Tool(tool));
        self
    }

    /// Provider this count is for.
    #[must_use]
    pub const fn provider(&self) -> &'a str {
        self.provider
    }

    /// Model this count is for.
    #[must_use]
    pub const fn model(&self) -> &'a str {
        self.model
    }

    /// Ordered parts to count.
    #[must_use]
    pub fn content(&self) -> &[CountableContent<'a>] {
        &self.content
    }
}

/// One token counting implementation.
///
/// Implementations return a bare number. The evidence class comes from
/// [`Self::descriptor`] and never from a per-call choice, so a heuristic
/// cannot hand a consumer an [`ExactTokenCount`].
#[async_trait]
pub trait TokenCounter: Send + Sync {
    /// Declared identity, evidence class and served scope.
    fn descriptor(&self) -> TokenCounterDescriptor;

    /// Count the tokens `request` costs.
    ///
    /// # Errors
    /// Returns [`TokenCountFailureKind::Unsupported`] when this counter cannot
    /// honestly count this request — an unserved provider/model, or content it
    /// has no way to measure — [`TokenCountFailureKind::Failed`] when its own
    /// operation failed, and [`TokenCountFailureKind::Cancelled`] when
    /// `cancellation` fired first. Never return a fabricated number.
    async fn count(
        &self,
        request: &TokenCountRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<u64, TokenCountFailure>;
}

/// One completed count plus every better-ranked counter that declined.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenCountOutcome {
    count: TokenCount,
    refused: Vec<TokenCountRefusal>,
}

impl TokenCountOutcome {
    /// The count that was produced.
    #[must_use]
    pub const fn count(&self) -> &TokenCount {
        &self.count
    }

    /// Take the count.
    #[must_use]
    pub fn into_count(self) -> TokenCount {
        self.count
    }

    /// Better-ranked counters that refused this request, best-ranked first.
    /// A non-empty list means a fallback happened; it is never silent.
    #[must_use]
    pub fn refused(&self) -> &[TokenCountRefusal] {
        &self.refused
    }
}

struct CounterEntry {
    descriptor: TokenCounterDescriptor,
    counter: Arc<dyn TokenCounter>,
    registration: Arc<()>,
}

#[derive(Default)]
struct TokenCounterRegistryInner {
    counters: Mutex<BTreeMap<TokenCounterId, Arc<CounterEntry>>>,
}

/// Registered token counters and their deterministic selection.
#[derive(Clone, Default)]
pub struct TokenCounterRegistry {
    inner: Arc<TokenCounterRegistryInner>,
}

impl TokenCounterRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one counter as a context-owned effect.
    ///
    /// Disposal removes exactly this registration: a later registration of the
    /// same id survives this one's disposer.
    ///
    /// # Errors
    /// Returns [`TokenCountError::DuplicateCounter`] when the id is taken and
    /// [`TokenCountError::RegistryUnavailable`] when registry state is
    /// poisoned. Nothing is published on failure.
    pub fn register(
        &self,
        context: &Context,
        counter: Arc<dyn TokenCounter>,
    ) -> Result<(), TokenCountError> {
        let descriptor = counter.descriptor();
        let id = descriptor.id().clone();
        let registration = Arc::new(());
        let entry = Arc::new(CounterEntry {
            descriptor,
            counter,
            registration: Arc::clone(&registration),
        });

        let mut counters = self
            .inner
            .counters
            .lock()
            .map_err(|_| TokenCountError::RegistryUnavailable)?;
        if counters.contains_key(&id) {
            return Err(TokenCountError::DuplicateCounter { id });
        }
        counters.insert(id.clone(), entry);
        drop(counters);

        let removal = CounterRegistration {
            inner: Arc::downgrade(&self.inner),
            id,
            token: registration,
            active: true,
        };
        context.effect(move || drop(removal));
        Ok(())
    }

    /// Every registered counter in stable id order.
    ///
    /// # Errors
    /// Returns [`TokenCountError::RegistryUnavailable`] when registry state is
    /// poisoned.
    pub fn descriptors(&self) -> Result<Vec<TokenCounterDescriptor>, TokenCountError> {
        let counters = self
            .inner
            .counters
            .lock()
            .map_err(|_| TokenCountError::RegistryUnavailable)?;
        Ok(counters
            .values()
            .map(|entry| entry.descriptor.clone())
            .collect())
    }

    /// Counters serving `provider`/`model`, best evidence first and stable id
    /// within one evidence rank. Registration order never affects this.
    ///
    /// # Errors
    /// Returns [`TokenCountError::RegistryUnavailable`] when registry state is
    /// poisoned.
    pub fn candidates(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Vec<TokenCounterDescriptor>, TokenCountError> {
        Ok(self
            .eligible(provider, model)?
            .into_iter()
            .map(|entry| entry.descriptor.clone())
            .collect())
    }

    /// Count `request` with the best counter that serves it.
    ///
    /// A counter refusing the request as unsupported hands over to the next
    /// candidate and is recorded in [`TokenCountOutcome::refused`]. A counter
    /// FAILURE or cancellation stops here rather than silently degrading a
    /// measurement into an estimate.
    ///
    /// # Errors
    /// Returns [`TokenCountError::NoCounter`] when nothing serves the target,
    /// [`TokenCountError::Unsupported`] when every candidate refused,
    /// [`TokenCountError::CounterFailed`] / [`TokenCountError::Cancelled`] for
    /// a candidate's own failure, and
    /// [`TokenCountError::RegistryUnavailable`] for poisoned registry state.
    pub async fn count(
        &self,
        request: &TokenCountRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<TokenCountOutcome, TokenCountError> {
        let candidates = self.eligible(request.provider(), request.model())?;
        if candidates.is_empty() {
            return Err(TokenCountError::NoCounter {
                provider: request.provider().to_owned(),
                model: request.model().to_owned(),
            });
        }

        let mut refused = Vec::new();
        for entry in candidates {
            let id = entry.descriptor.id().clone();
            match entry.counter.count(request, cancellation).await {
                Ok(tokens) => {
                    return Ok(TokenCountOutcome {
                        count: mint(&entry.descriptor, tokens),
                        refused,
                    });
                }
                Err(failure) => match failure.kind() {
                    TokenCountFailureKind::Unsupported => refused.push(TokenCountRefusal {
                        counter: id,
                        message: failure.message().to_owned(),
                    }),
                    TokenCountFailureKind::Failed => {
                        return Err(TokenCountError::CounterFailed {
                            counter: id,
                            message: failure.message().to_owned(),
                        });
                    }
                    TokenCountFailureKind::Cancelled => {
                        return Err(TokenCountError::Cancelled { counter: id });
                    }
                },
            }
        }
        Err(TokenCountError::Unsupported {
            provider: request.provider().to_owned(),
            model: request.model().to_owned(),
            refusals: refused,
        })
    }

    /// Eligible entries in selection order. The lock is released before any
    /// counter runs.
    fn eligible(
        &self,
        provider: &str,
        model: &str,
    ) -> Result<Vec<Arc<CounterEntry>>, TokenCountError> {
        let counters = self
            .inner
            .counters
            .lock()
            .map_err(|_| TokenCountError::RegistryUnavailable)?;
        // Id-ordered by the map; a stable sort by evidence keeps that order
        // within one rank, so equal evidence is broken by id and never by
        // registration order.
        let mut rows: Vec<Arc<CounterEntry>> = counters
            .values()
            .filter(|entry| entry.descriptor.scope().serves(provider, model))
            .map(Arc::clone)
            .collect();
        drop(counters);
        rows.sort_by_key(|entry| entry.descriptor.evidence());
        Ok(rows)
    }
}

/// Mint the typed count from the counter's DECLARED evidence. This is the only
/// construction site for either count type.
fn mint(descriptor: &TokenCounterDescriptor, tokens: u64) -> TokenCount {
    match descriptor.evidence() {
        TokenEvidence::Exact => TokenCount::Exact(ExactTokenCount {
            counter: descriptor.id().clone(),
            tokens,
        }),
        TokenEvidence::Estimated(method) => TokenCount::Estimated(EstimatedTokenCount {
            counter: descriptor.id().clone(),
            tokens,
            method,
        }),
    }
}

struct CounterRegistration {
    inner: Weak<TokenCounterRegistryInner>,
    id: TokenCounterId,
    token: Arc<()>,
    active: bool,
}

impl CounterRegistration {
    fn remove(&mut self) {
        if !self.active {
            return;
        }
        self.active = false;
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        if let Ok(mut counters) = inner.counters.lock() {
            let mine = counters
                .get(&self.id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.registration, &self.token));
            if mine {
                counters.remove(&self.id);
            }
        }
    }
}

impl Drop for CounterRegistration {
    fn drop(&mut self) {
        self.remove();
    }
}

/// The always-available local fallback: `ceil(utf8_bytes / 4)` over everything
/// the request will send.
///
/// Accuracy, stated honestly: this is a ratio, not a tokenizer, and it never
/// claims to be exact. It is derived from the widely published "~4 characters
/// per token" rule for English prose in Latin-script byte-pair vocabularies.
/// Counting UTF-8 BYTES rather than characters keeps it usable for non-Latin
/// scripts, where a character ratio collapses (three CJK characters are nine
/// bytes, so three tokens here, versus zero under `chars / 4`) — but the true
/// figure varies by vocabulary and can differ substantially in either
/// direction. It does not model per-message protocol framing, so it understates
/// a chat request by a few tokens per message, and it refuses image and
/// document input outright rather than inventing a number for bytes it cannot
/// read. Use it for pressure and display, never where an exact count is
/// required.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeuristicTokenEstimator;

impl HeuristicTokenEstimator {
    /// Stable registry id of this estimator.
    pub const ID: &'static str = "local-utf8-byte-ratio";

    /// The estimator.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Estimate without a registry.
    ///
    /// Consumers holding a [`TokenCounterRegistry`] should count through it
    /// instead, so a better counter can supersede this one.
    ///
    /// # Errors
    /// Returns [`TokenCountFailureKind::Unsupported`] for image or document
    /// input, which a byte ratio cannot honestly count, and
    /// [`TokenCountFailureKind::Failed`] for a tool schema that does not
    /// serialize.
    pub fn estimate(
        &self,
        request: &TokenCountRequest<'_>,
    ) -> Result<EstimatedTokenCount, TokenCountFailure> {
        let mut bytes: u64 = 0;
        for item in request.content() {
            match item {
                CountableContent::Text(text) => bytes = add_bytes(bytes, text.len()),
                CountableContent::Message(message) => {
                    if !message.images.is_empty() || !message.documents.is_empty() {
                        return Err(TokenCountFailure::unsupported(
                            "byte-ratio estimation cannot count image or document input",
                        ));
                    }
                    bytes = add_bytes(bytes, message.content.len());
                    if let Some(tool_call_id) = message.tool_call_id.as_ref() {
                        bytes = add_bytes(bytes, tool_call_id.len());
                    }
                    for call in message.tool_calls.iter().flatten() {
                        bytes = add_bytes(bytes, call.id.len());
                        bytes = add_bytes(bytes, call.name.len());
                        bytes = add_bytes(bytes, call.arguments.len());
                    }
                }
                CountableContent::Tool(tool) => {
                    let schema = serde_json::to_string(&tool.parameters)
                        .map_err(|_| TokenCountFailure::failed("tool schema does not serialize"))?;
                    bytes = add_bytes(bytes, tool.name.len());
                    bytes = add_bytes(bytes, tool.description.len());
                    bytes = add_bytes(bytes, schema.len());
                }
            }
        }
        Ok(EstimatedTokenCount {
            counter: TokenCounterId::built_in(Self::ID),
            tokens: bytes.div_ceil(UTF8_BYTES_PER_TOKEN),
            method: EstimationMethod::Utf8ByteRatio,
        })
    }
}

#[async_trait]
impl TokenCounter for HeuristicTokenEstimator {
    fn descriptor(&self) -> TokenCounterDescriptor {
        TokenCounterDescriptor {
            id: TokenCounterId::built_in(Self::ID),
            evidence: TokenEvidence::Estimated(EstimationMethod::Utf8ByteRatio),
            scope: TokenCounterScope::Any,
        }
    }

    async fn count(
        &self,
        request: &TokenCountRequest<'_>,
        _cancellation: &CancellationToken,
    ) -> Result<u64, TokenCountFailure> {
        self.estimate(request).map(|estimate| estimate.tokens())
    }
}

/// Saturating byte accumulation: an absurd input yields `u64::MAX` rather than
/// wrapping into a small, believable number.
fn add_bytes(total: u64, len: usize) -> u64 {
    total.saturating_add(u64::try_from(len).unwrap_or(u64::MAX))
}

/// Validate one non-blank provider/model id used in a scope.
fn validated_id(field: &'static str, value: impl Into<String>) -> Result<String, TokenCountError> {
    let value = value.into();
    if value.is_empty() || value.trim() != value {
        return Err(TokenCountError::Invalid {
            field,
            message: "id must be non-blank with no surrounding whitespace".to_owned(),
        });
    }
    Ok(value)
}

/// Collapse control characters and bound a diagnostic on a character boundary.
fn bounded_message(message: String) -> String {
    let mut safe = String::with_capacity(message.len().min(MAX_FAILURE_MESSAGE_BYTES));
    for character in message.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if safe.len() + character.len_utf8() > MAX_FAILURE_MESSAGE_BYTES {
            break;
        }
        safe.push(character);
    }
    safe
}
