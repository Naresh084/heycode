//! Evidence-attributed catalog values.
//!
//! Every value in this module carries the evidence that produced it, and the
//! un-attributed state is not representable rather than merely discouraged:
//!
//! * [`AttributedSupport`] and [`AttributedLimit`] have exactly two variants,
//!   and both carry a payload naming the origin. There is no third "plain
//!   value" variant to fall into.
//! * The payload structs have private fields and `pub(crate)` constructors, so
//!   no code outside this crate can build a [`CapabilityAssertion`] or a
//!   [`LimitAssertion`] at all. The only mint is [`crate::CatalogOverrides`],
//!   which produces one solely from bytes it read out of a user override
//!   document.
//! * Nothing here implements `Deserialize`, `Default` or `From<CapabilitySupport>`.
//!   Provenance is decided by the reader that produced the value, never carried
//!   in the data, so there is no field a document could set to claim it — the
//!   same rule `heycode_core::UntrustedContentBoundary` applies to external text.
//! * There is no projection back to a merged [`ModelDescriptor`].
//!   [`AttributedModel::evidence`] returns the provider's row exactly as the
//!   catalog published it, never a row with user assertions folded in, so a
//!   descriptor carrying an unlabelled assertion cannot be constructed.

use std::num::NonZeroU64;

use heycode_llm::{CapabilitySupport, ModelCapabilities, ModelDescriptor, ProviderDescriptor};

/// One capability field of a catalog row that a user override may assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelCapabilityKind {
    /// Function/tool calling.
    Tools,
    /// Reasoning/thinking state.
    Reasoning,
    /// Image input.
    ImageInput,
    /// Native document/file input.
    DocumentInput,
    /// Schema-constrained structured output.
    StructuredOutput,
    /// Provider-hosted web search/fetch.
    NativeWeb,
    /// Provider-native compaction/context editing.
    NativeCompaction,
    /// Prompt-prefix caching.
    PromptCache,
}

impl ModelCapabilityKind {
    /// Every capability kind, in stable declaration order.
    pub const ALL: [Self; 8] = [
        Self::Tools,
        Self::Reasoning,
        Self::ImageInput,
        Self::DocumentInput,
        Self::StructuredOutput,
        Self::NativeWeb,
        Self::NativeCompaction,
        Self::PromptCache,
    ];

    /// Number of capability kinds.
    pub const COUNT: usize = Self::ALL.len();

    /// Stable document key for this capability.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Tools => "tools",
            Self::Reasoning => "reasoning",
            Self::ImageInput => "image_input",
            Self::DocumentInput => "document_input",
            Self::StructuredOutput => "structured_output",
            Self::NativeWeb => "native_web",
            Self::NativeCompaction => "native_compaction",
            Self::PromptCache => "prompt_cache",
        }
    }

    /// Position of this kind in [`Self::ALL`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Tools => 0,
            Self::Reasoning => 1,
            Self::ImageInput => 2,
            Self::DocumentInput => 3,
            Self::StructuredOutput => 4,
            Self::NativeWeb => 5,
            Self::NativeCompaction => 6,
            Self::PromptCache => 7,
        }
    }

    /// Read this kind out of a provider-published capability snapshot.
    #[must_use]
    pub const fn of(self, capabilities: &ModelCapabilities) -> CapabilitySupport {
        match self {
            Self::Tools => capabilities.tools,
            Self::Reasoning => capabilities.reasoning,
            Self::ImageInput => capabilities.image_input,
            Self::DocumentInput => capabilities.document_input,
            Self::StructuredOutput => capabilities.structured_output,
            Self::NativeWeb => capabilities.native_web,
            Self::NativeCompaction => capabilities.native_compaction,
            Self::PromptCache => capabilities.prompt_cache,
        }
    }
}

/// A numeric catalog limit a user override may assert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ModelLimitField {
    /// Input context window in tokens.
    ContextWindow,
    /// Maximum output tokens.
    MaxOutputTokens,
}

impl ModelLimitField {
    /// Every limit field, in stable declaration order.
    pub const ALL: [Self; 2] = [Self::ContextWindow, Self::MaxOutputTokens];

    /// Stable document key for this limit.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::ContextWindow => "context_window",
            Self::MaxOutputTokens => "max_output_tokens",
        }
    }
}

/// Where a user assertion stands relative to the provider evidence it replaced.
///
/// This is computed from the two values, never read from a document, so a user
/// cannot label their own assertion as harmless.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssertionDirection {
    /// The assertion restates what the provider's catalog already published.
    Redundant,
    /// The assertion removes capability or lowers a limit. This direction can
    /// only make heycode decline to attempt something; it can never permit a
    /// request the provider's own evidence did not already permit.
    Narrowing,
    /// The assertion claims support or a limit the provider's catalog left
    /// unknown. Nothing contradicts it and nothing evidences it.
    ClaimsUnevidencedSupport,
    /// The assertion claims support or a limit the provider's catalog
    /// explicitly published as absent or smaller. This is the strongest
    /// warning a surface can carry.
    ContradictsEvidence,
}

impl AssertionDirection {
    /// Stable short label naming the user as the author.
    ///
    /// Every label starts with `user-`; none of them can be mistaken for
    /// provider evidence, which is rendered without any such prefix.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Redundant => "user-asserted (matches provider evidence)",
            Self::Narrowing => "user-restricted",
            Self::ClaimsUnevidencedSupport => "user-asserted (no provider evidence)",
            Self::ContradictsEvidence => "user-asserted (CONTRADICTS provider evidence)",
        }
    }

    /// Whether this direction can permit something the provider's own catalog
    /// did not evidence. True only for the two claiming directions.
    #[must_use]
    pub const fn permits_unevidenced_request(self) -> bool {
        matches!(
            self,
            Self::ClaimsUnevidencedSupport | Self::ContradictsEvidence
        )
    }
}

const fn support_name(support: CapabilitySupport) -> &'static str {
    match support {
        CapabilitySupport::Supported => "supported",
        CapabilitySupport::Unsupported => "unsupported",
        CapabilitySupport::Unknown => "unknown",
    }
}

const fn capability_direction(
    evidence: CapabilitySupport,
    asserted: CapabilitySupport,
) -> AssertionDirection {
    match (evidence, asserted) {
        (CapabilitySupport::Supported, CapabilitySupport::Supported)
        | (CapabilitySupport::Unsupported, CapabilitySupport::Unsupported)
        | (CapabilitySupport::Unknown, CapabilitySupport::Unknown) => AssertionDirection::Redundant,
        (CapabilitySupport::Unknown, CapabilitySupport::Supported) => {
            AssertionDirection::ClaimsUnevidencedSupport
        }
        (CapabilitySupport::Unsupported, CapabilitySupport::Supported) => {
            AssertionDirection::ContradictsEvidence
        }
        _ => AssertionDirection::Narrowing,
    }
}

const fn limit_direction(evidence: Option<u64>, asserted: NonZeroU64) -> AssertionDirection {
    match evidence {
        None => AssertionDirection::ClaimsUnevidencedSupport,
        Some(evidenced) if evidenced == asserted.get() => AssertionDirection::Redundant,
        Some(evidenced) if asserted.get() < evidenced => AssertionDirection::Narrowing,
        Some(_) => AssertionDirection::ContradictsEvidence,
    }
}

/// The exact override document one user assertion was read from.
///
/// Only [`crate::CatalogOverrides`] can construct this, and it does so from the
/// layer it is reading at that moment. A document cannot spell a source, so an
/// assertion cannot claim to have come from anywhere but a user override file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverrideSource {
    layer: String,
    document: String,
    precedence: usize,
    captured_at_ms: u64,
}

impl OverrideSource {
    pub(crate) fn new(
        layer: impl Into<String>,
        document: impl Into<String>,
        precedence: usize,
        captured_at_ms: u64,
    ) -> Self {
        Self {
            layer: layer.into(),
            document: document.into(),
            precedence,
            captured_at_ms,
        }
    }

    /// Caller-assigned layer label, such as `user` or `project`.
    #[must_use]
    pub fn layer(&self) -> &str {
        &self.layer
    }

    /// Display path of the override document.
    #[must_use]
    pub fn document(&self) -> &str {
        &self.document
    }

    /// Zero-based precedence rank in the configured low-to-high layer order.
    #[must_use]
    pub const fn precedence(&self) -> usize {
        self.precedence
    }

    /// Unix-millisecond instant this immutable override generation was read.
    #[must_use]
    pub const fn captured_at_ms(&self) -> u64 {
        self.captured_at_ms
    }
}

impl std::fmt::Display for OverrideSource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "override layer `{}` ({}, precedence {}, captured at {})",
            self.layer, self.document, self.precedence, self.captured_at_ms
        )
    }
}

/// Capability evidence published by one provider catalog generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidencedSupport {
    support: CapabilitySupport,
    revision: u64,
    fetched_at_ms: u64,
}

impl EvidencedSupport {
    /// Tri-state the provider's own catalog published.
    #[must_use]
    pub const fn support(self) -> CapabilitySupport {
        self.support
    }

    /// Provider-local revision of the generation that published it.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Commit time of that generation in Unix milliseconds.
    #[must_use]
    pub const fn fetched_at_ms(self) -> u64 {
        self.fetched_at_ms
    }
}

/// Limit evidence published by one provider catalog generation. An absent
/// value is unknown, never zero and never unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EvidencedLimit {
    value: Option<u64>,
    revision: u64,
    fetched_at_ms: u64,
}

impl EvidencedLimit {
    /// Limit the provider's own catalog published, when it published one.
    #[must_use]
    pub const fn value(self) -> Option<u64> {
        self.value
    }

    /// Provider-local revision of the generation that published it.
    #[must_use]
    pub const fn revision(self) -> u64 {
        self.revision
    }

    /// Commit time of that generation in Unix milliseconds.
    #[must_use]
    pub const fn fetched_at_ms(self) -> u64 {
        self.fetched_at_ms
    }
}

/// One user assertion about a model capability, carried together with the
/// provider evidence it stands against. The evidence is a field, not something
/// the assertion replaced, so no surface can show the claim without also being
/// handed what the provider actually published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityAssertion {
    kind: ModelCapabilityKind,
    asserted: CapabilitySupport,
    evidence: EvidencedSupport,
    source: OverrideSource,
}

impl CapabilityAssertion {
    /// Which capability was asserted.
    #[must_use]
    pub const fn kind(&self) -> ModelCapabilityKind {
        self.kind
    }

    /// What the user asserted.
    #[must_use]
    pub const fn asserted(&self) -> CapabilitySupport {
        self.asserted
    }

    /// What the provider's own catalog published for the same capability.
    #[must_use]
    pub const fn provider_evidence(&self) -> CapabilitySupport {
        self.evidence.support
    }

    /// Provider-local revision that supplied the evidence this assertion
    /// stands against.
    #[must_use]
    pub const fn provider_revision(&self) -> u64 {
        self.evidence.revision
    }

    /// Commit instant of the provider generation this assertion stands
    /// against.
    #[must_use]
    pub const fn provider_fetched_at_ms(&self) -> u64 {
        self.evidence.fetched_at_ms
    }

    /// Which override document supplied the assertion.
    #[must_use]
    pub const fn source(&self) -> &OverrideSource {
        &self.source
    }

    /// Where the assertion stands relative to the provider evidence.
    #[must_use]
    pub const fn direction(&self) -> AssertionDirection {
        capability_direction(self.evidence.support, self.asserted)
    }

    /// Deterministic one-line rendering naming the capability, the claim, the
    /// provider evidence and the document.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "{}: {} — {}; provider catalog: {}; from {}",
            self.kind.name(),
            support_name(self.asserted),
            self.direction().label(),
            support_name(self.evidence.support),
            self.source,
        )
    }
}

/// One user assertion about a numeric catalog limit, carried together with the
/// provider evidence it stands against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LimitAssertion {
    field: ModelLimitField,
    asserted: NonZeroU64,
    evidence: EvidencedLimit,
    source: OverrideSource,
}

impl LimitAssertion {
    /// Which limit was asserted.
    #[must_use]
    pub const fn field(&self) -> ModelLimitField {
        self.field
    }

    /// What the user asserted. A document cannot assert zero or absence.
    #[must_use]
    pub const fn asserted(&self) -> NonZeroU64 {
        self.asserted
    }

    /// What the provider's own catalog published for the same limit.
    #[must_use]
    pub const fn provider_evidence(&self) -> Option<u64> {
        self.evidence.value
    }

    /// Provider-local revision that supplied the evidence this assertion
    /// stands against.
    #[must_use]
    pub const fn provider_revision(&self) -> u64 {
        self.evidence.revision
    }

    /// Commit instant of the provider generation this assertion stands
    /// against.
    #[must_use]
    pub const fn provider_fetched_at_ms(&self) -> u64 {
        self.evidence.fetched_at_ms
    }

    /// Which override document supplied the assertion.
    #[must_use]
    pub const fn source(&self) -> &OverrideSource {
        &self.source
    }

    /// Where the assertion stands relative to the provider evidence.
    #[must_use]
    pub const fn direction(&self) -> AssertionDirection {
        limit_direction(self.evidence.value, self.asserted)
    }

    /// Deterministic one-line rendering naming the limit, the claim, the
    /// provider evidence and the document.
    #[must_use]
    pub fn describe(&self) -> String {
        let evidence = match self.evidence.value {
            Some(value) => value.to_string(),
            None => "unknown".to_owned(),
        };
        format!(
            "{}: {} — {}; provider catalog: {}; from {}",
            self.field.name(),
            self.asserted.get(),
            self.direction().label(),
            evidence,
            self.source,
        )
    }
}

/// Capability decision safe for machine enforcement.
///
/// An assertion that would add support is excluded from this value: it stays
/// visible on [`AttributedSupport`] but this decision retains the provider's
/// exact tri-state and generation. Only redundant or narrowing user policy can
/// become [`Self::UserConstraint`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapabilityEnforcement<'assertion> {
    /// The provider generation remains authoritative.
    ProviderEvidence(EvidencedSupport),
    /// A non-escalating user assertion is the effective constraint.
    UserConstraint(&'assertion CapabilityAssertion),
}

impl CapabilityEnforcement<'_> {
    /// Effective tri-state. This can never be more permissive than the
    /// provider evidence carried by the same decision.
    #[must_use]
    pub const fn support(self) -> CapabilitySupport {
        match self {
            Self::ProviderEvidence(evidence) => evidence.support,
            Self::UserConstraint(assertion) => assertion.asserted,
        }
    }

    /// Provider evidence retained regardless of whether user policy narrows
    /// it.
    #[must_use]
    pub const fn provider_evidence(self) -> EvidencedSupport {
        match self {
            Self::ProviderEvidence(evidence) => evidence,
            Self::UserConstraint(assertion) => assertion.evidence,
        }
    }
}

/// Numeric-limit decision safe for machine enforcement.
///
/// A user value cannot invent an absent limit or raise a published one. Such
/// assertions stay visible beside the row while this decision retains the
/// provider value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LimitEnforcement<'assertion> {
    /// The provider generation remains authoritative.
    ProviderEvidence(EvidencedLimit),
    /// A known non-escalating user limit is the effective constraint.
    UserConstraint(&'assertion LimitAssertion),
}

impl LimitEnforcement<'_> {
    /// Effective limit. Absence is Unknown, never unlimited or zero.
    #[must_use]
    pub const fn value(self) -> Option<u64> {
        match self {
            Self::ProviderEvidence(evidence) => evidence.value,
            Self::UserConstraint(assertion) => Some(assertion.asserted.get()),
        }
    }

    /// Provider evidence retained regardless of whether user policy narrows
    /// it.
    #[must_use]
    pub const fn provider_evidence(self) -> EvidencedLimit {
        match self {
            Self::ProviderEvidence(evidence) => evidence,
            Self::UserConstraint(assertion) => assertion.evidence,
        }
    }
}

/// One capability value together with the evidence that produced it.
///
/// Both variants carry their origin, so reading the value means matching on
/// where it came from. There is no variant and no accessor that yields a bare
/// tri-state without also naming its author, except the explicitly named
/// [`Self::enforced`], which exists for machine enforcement only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributedSupport {
    /// The provider's own catalog generation published this value.
    ProviderEvidenced(EvidencedSupport),
    /// A user override document asserted this value.
    UserAsserted(CapabilityAssertion),
}

impl AttributedSupport {
    pub(crate) const fn evidenced(
        support: CapabilitySupport,
        revision: u64,
        fetched_at_ms: u64,
    ) -> Self {
        Self::ProviderEvidenced(EvidencedSupport {
            support,
            revision,
            fetched_at_ms,
        })
    }

    pub(crate) const fn asserted(
        kind: ModelCapabilityKind,
        asserted: CapabilitySupport,
        evidence: CapabilitySupport,
        revision: u64,
        fetched_at_ms: u64,
        source: OverrideSource,
    ) -> Self {
        Self::UserAsserted(CapabilityAssertion {
            kind,
            asserted,
            evidence: EvidencedSupport {
                support: evidence,
                revision,
                fetched_at_ms,
            },
            source,
        })
    }

    /// What the provider's own catalog published, whether or not a user
    /// asserted something else over it. This never returns a user assertion.
    #[must_use]
    pub const fn provider_evidence(&self) -> CapabilitySupport {
        match self {
            Self::ProviderEvidenced(evidence) => evidence.support,
            Self::UserAsserted(assertion) => assertion.evidence.support,
        }
    }

    /// The user assertion standing over this value, when there is one.
    #[must_use]
    pub const fn assertion(&self) -> Option<&CapabilityAssertion> {
        match self {
            Self::ProviderEvidenced(_) => None,
            Self::UserAsserted(assertion) => Some(assertion),
        }
    }

    /// Origin-retaining capability decision for machine enforcement.
    ///
    /// User claims that would turn Unknown/Unsupported into Supported remain
    /// visible on this attributed value but cannot enter the enforcement
    /// decision. Redundant and narrowing assertions retain their exact source.
    #[must_use]
    pub const fn enforcement(&self) -> CapabilityEnforcement<'_> {
        match self {
            Self::ProviderEvidenced(evidence) => CapabilityEnforcement::ProviderEvidence(*evidence),
            Self::UserAsserted(assertion)
                if assertion.direction().permits_unevidenced_request() =>
            {
                CapabilityEnforcement::ProviderEvidence(assertion.evidence)
            }
            Self::UserAsserted(assertion) => CapabilityEnforcement::UserConstraint(assertion),
        }
    }

    /// Tri-state to enforce against after non-escalation.
    ///
    /// This deliberately discards the origin and is therefore **never correct
    /// for a rendered surface**: a display that calls it shows a user's guess
    /// and vendor evidence as the same word. Request resolution — which must
    /// decide whether a capability may be used — is the only correct caller.
    /// Anything that displays a capability must use [`Self::render`], or match
    /// this enum and render the two variants differently.
    #[must_use]
    pub const fn enforced(&self) -> CapabilitySupport {
        self.enforcement().support()
    }

    /// Deterministic human-readable rendering that always names the origin.
    ///
    /// A provider-evidenced value and a user-asserted value can never render
    /// the same string: the asserted form always carries a `user-` labelled
    /// direction and the document it came from.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::ProviderEvidenced(evidence) => format!(
                "{} (provider catalog, revision {})",
                support_name(evidence.support),
                evidence.revision
            ),
            Self::UserAsserted(assertion) => assertion.describe(),
        }
    }
}

/// One numeric limit together with the evidence that produced it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributedLimit {
    /// The provider's own catalog generation published this value.
    ProviderEvidenced(EvidencedLimit),
    /// A user override document asserted this value.
    UserAsserted(LimitAssertion),
}

impl AttributedLimit {
    pub(crate) const fn evidenced(value: Option<u64>, revision: u64, fetched_at_ms: u64) -> Self {
        Self::ProviderEvidenced(EvidencedLimit {
            value,
            revision,
            fetched_at_ms,
        })
    }

    pub(crate) const fn asserted(
        field: ModelLimitField,
        asserted: NonZeroU64,
        evidence: Option<u64>,
        revision: u64,
        fetched_at_ms: u64,
        source: OverrideSource,
    ) -> Self {
        Self::UserAsserted(LimitAssertion {
            field,
            asserted,
            evidence: EvidencedLimit {
                value: evidence,
                revision,
                fetched_at_ms,
            },
            source,
        })
    }

    /// What the provider's own catalog published, whether or not a user
    /// asserted something else over it.
    #[must_use]
    pub const fn provider_evidence(&self) -> Option<u64> {
        match self {
            Self::ProviderEvidenced(evidence) => evidence.value,
            Self::UserAsserted(assertion) => assertion.evidence.value,
        }
    }

    /// The user assertion standing over this value, when there is one.
    #[must_use]
    pub const fn assertion(&self) -> Option<&LimitAssertion> {
        match self {
            Self::ProviderEvidenced(_) => None,
            Self::UserAsserted(assertion) => Some(assertion),
        }
    }

    /// Origin-retaining limit decision for machine enforcement.
    #[must_use]
    pub const fn enforcement(&self) -> LimitEnforcement<'_> {
        match self {
            Self::ProviderEvidenced(evidence) => LimitEnforcement::ProviderEvidence(*evidence),
            Self::UserAsserted(assertion)
                if assertion.direction().permits_unevidenced_request() =>
            {
                LimitEnforcement::ProviderEvidence(assertion.evidence)
            }
            Self::UserAsserted(assertion) => LimitEnforcement::UserConstraint(assertion),
        }
    }

    /// Limit to enforce after non-escalation. Carries the same warning as
    /// [`AttributedSupport::enforced`]: never render this.
    #[must_use]
    pub const fn enforced(&self) -> Option<u64> {
        self.enforcement().value()
    }

    /// Deterministic human-readable rendering that always names the origin.
    #[must_use]
    pub fn render(&self) -> String {
        match self {
            Self::ProviderEvidenced(evidence) => {
                let value = match evidence.value {
                    Some(value) => value.to_string(),
                    None => "unknown".to_owned(),
                };
                format!("{value} (provider catalog, revision {})", evidence.revision)
            }
            Self::UserAsserted(assertion) => assertion.describe(),
        }
    }
}

/// One user assertion on a model row, of either kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelAssertion<'a> {
    /// An asserted capability.
    Capability(&'a CapabilityAssertion),
    /// An asserted numeric limit.
    Limit(&'a LimitAssertion),
}

impl ModelAssertion<'_> {
    /// Document key of the asserted field.
    #[must_use]
    pub const fn field_name(&self) -> &'static str {
        match self {
            Self::Capability(assertion) => assertion.kind.name(),
            Self::Limit(assertion) => assertion.field.name(),
        }
    }

    /// Where the assertion stands relative to the provider evidence.
    #[must_use]
    pub const fn direction(&self) -> AssertionDirection {
        match self {
            Self::Capability(assertion) => assertion.direction(),
            Self::Limit(assertion) => assertion.direction(),
        }
    }

    /// Which override document supplied the assertion.
    #[must_use]
    pub const fn source(&self) -> &OverrideSource {
        match self {
            Self::Capability(assertion) => assertion.source(),
            Self::Limit(assertion) => assertion.source(),
        }
    }

    /// Deterministic one-line rendering of the assertion and its evidence.
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Capability(assertion) => assertion.describe(),
            Self::Limit(assertion) => assertion.describe(),
        }
    }
}

/// A user override that named a model this provider's catalog does not
/// publish. An override never adds a row, so the assertion is inert and the
/// user is told rather than left believing it applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnmatchedOverride {
    provider: String,
    model: String,
    source: OverrideSource,
}

impl UnmatchedOverride {
    pub(crate) fn new(
        provider: impl Into<String>,
        model: impl Into<String>,
        source: OverrideSource,
    ) -> Self {
        Self {
            provider: provider.into(),
            model: model.into(),
            source,
        }
    }

    /// Provider the override named.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Model id the override named.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    /// Which override document supplied it.
    #[must_use]
    pub const fn source(&self) -> &OverrideSource {
        &self.source
    }

    /// Deterministic one-line rendering.
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "override for `{}`/`{}` from {} matched no model in the provider catalog and was not applied",
            self.provider, self.model, self.source,
        )
    }
}

/// One catalog row with every overridable field attributed to its evidence.
///
/// The provider's row is retained verbatim in [`Self::evidence`] and is never
/// rewritten, so nothing in this type can hand out a [`ModelDescriptor`]
/// carrying an unlabelled user assertion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedModel {
    evidence: ModelDescriptor,
    capabilities: [AttributedSupport; ModelCapabilityKind::COUNT],
    context_window: AttributedLimit,
    max_output_tokens: AttributedLimit,
}

impl AttributedModel {
    pub(crate) const fn new(
        evidence: ModelDescriptor,
        capabilities: [AttributedSupport; ModelCapabilityKind::COUNT],
        context_window: AttributedLimit,
        max_output_tokens: AttributedLimit,
    ) -> Self {
        Self {
            evidence,
            capabilities,
            context_window,
            max_output_tokens,
        }
    }

    /// Provider-native model id.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.evidence.id
    }

    /// The provider catalog row exactly as it was published, with no user
    /// assertion folded in.
    #[must_use]
    pub const fn evidence(&self) -> &ModelDescriptor {
        &self.evidence
    }

    /// Attributed value of one capability.
    #[must_use]
    pub fn capability(&self, kind: ModelCapabilityKind) -> &AttributedSupport {
        &self.capabilities[kind.index()]
    }

    /// Attributed input context window.
    #[must_use]
    pub const fn context_window(&self) -> &AttributedLimit {
        &self.context_window
    }

    /// Attributed maximum output tokens.
    #[must_use]
    pub const fn max_output_tokens(&self) -> &AttributedLimit {
        &self.max_output_tokens
    }

    /// Every user assertion on this row, in stable field order. An empty
    /// result means the row is pure provider evidence.
    #[must_use]
    pub fn assertions(&self) -> Vec<ModelAssertion<'_>> {
        let mut assertions = Vec::new();
        for kind in ModelCapabilityKind::ALL {
            if let Some(assertion) = self.capabilities[kind.index()].assertion() {
                assertions.push(ModelAssertion::Capability(assertion));
            }
        }
        for limit in [&self.context_window, &self.max_output_tokens] {
            if let Some(assertion) = limit.assertion() {
                assertions.push(ModelAssertion::Limit(assertion));
            }
        }
        assertions
    }

    /// Whether any field of this row carries a user assertion.
    #[must_use]
    pub fn has_user_assertion(&self) -> bool {
        !self.assertions().is_empty()
    }

    /// Origin-retaining capability decision for request enforcement.
    #[must_use]
    pub fn capability_enforcement(&self, kind: ModelCapabilityKind) -> CapabilityEnforcement<'_> {
        self.capabilities[kind.index()].enforcement()
    }

    /// Capability tri-state to enforce after non-escalation.
    /// Carries the warning on [`AttributedSupport::enforced`]: never render it.
    #[must_use]
    pub fn enforced_capability(&self, kind: ModelCapabilityKind) -> CapabilitySupport {
        self.capabilities[kind.index()].enforced()
    }

    /// Origin-retaining context-window decision for request enforcement.
    #[must_use]
    pub const fn context_window_enforcement(&self) -> LimitEnforcement<'_> {
        self.context_window.enforcement()
    }

    /// Context window to enforce after non-escalation.
    #[must_use]
    pub const fn enforced_context_window(&self) -> Option<u64> {
        self.context_window.enforced()
    }

    /// Origin-retaining maximum-output decision for request enforcement.
    #[must_use]
    pub const fn max_output_tokens_enforcement(&self) -> LimitEnforcement<'_> {
        self.max_output_tokens.enforcement()
    }

    /// Maximum output tokens to enforce after non-escalation.
    #[must_use]
    pub const fn enforced_max_output_tokens(&self) -> Option<u64> {
        self.max_output_tokens.enforced()
    }
}

/// One provider generation with user assertions attributed on top of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttributedCatalog {
    provider: ProviderDescriptor,
    models: Vec<AttributedModel>,
    revision: u64,
    fetched_at_ms: u64,
    override_captured_at_ms: Option<u64>,
    unmatched: Vec<UnmatchedOverride>,
}

impl AttributedCatalog {
    pub(crate) const fn new(
        provider: ProviderDescriptor,
        models: Vec<AttributedModel>,
        revision: u64,
        fetched_at_ms: u64,
        override_captured_at_ms: Option<u64>,
        unmatched: Vec<UnmatchedOverride>,
    ) -> Self {
        Self {
            provider,
            models,
            revision,
            fetched_at_ms,
            override_captured_at_ms,
            unmatched,
        }
    }

    /// Provider identity of the attributed generation.
    #[must_use]
    pub const fn provider(&self) -> &ProviderDescriptor {
        &self.provider
    }

    /// Attributed rows in the generation's own catalog order.
    #[must_use]
    pub fn models(&self) -> &[AttributedModel] {
        &self.models
    }

    /// Attributed row for one canonical model id.
    #[must_use]
    pub fn model(&self, id: &str) -> Option<&AttributedModel> {
        self.models.iter().find(|model| model.id() == id)
    }

    /// Provider-local revision of the underlying generation.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Commit time of the underlying generation in Unix milliseconds.
    #[must_use]
    pub const fn fetched_at_ms(&self) -> u64 {
        self.fetched_at_ms
    }

    /// Capture instant of the immutable override generation projected here.
    ///
    /// `None` means [`CatalogOverrides::empty`](crate::CatalogOverrides::empty)
    /// supplied no loaded override generation.
    #[must_use]
    pub const fn override_captured_at_ms(&self) -> Option<u64> {
        self.override_captured_at_ms
    }

    /// Overrides that named a model this generation does not publish.
    #[must_use]
    pub fn unmatched(&self) -> &[UnmatchedOverride] {
        &self.unmatched
    }

    /// Every user assertion in this generation, paired with the model id it
    /// applies to, in catalog then field order.
    #[must_use]
    pub fn assertions(&self) -> Vec<(&str, ModelAssertion<'_>)> {
        self.models
            .iter()
            .flat_map(|model| {
                model
                    .assertions()
                    .into_iter()
                    .map(move |assertion| (model.id(), assertion))
            })
            .collect()
    }

    /// Every assertion that contradicts explicit provider evidence — the ones
    /// a live refresh has since disproved.
    #[must_use]
    pub fn contradictions(&self) -> Vec<(&str, ModelAssertion<'_>)> {
        self.assertions()
            .into_iter()
            .filter(|(_, assertion)| {
                assertion.direction() == AssertionDirection::ContradictsEvidence
            })
            .collect()
    }
}
