//! POR06 explicit OpenRouter request-plugin transforms.
//!
//! OpenRouter's request plugins change what is sent, what comes back, or what
//! is billed. There are two independent implicit-enable paths:
//!
//! - Context compression is on by default at the API level for small
//!   endpoints. "All OpenRouter endpoints with 8k (8,192 tokens) or less
//!   context length will default to using context compression. To disable
//!   this, pass `plugins: [{"id": "context-compression", "enabled": false}]`
//!   in the request body."
//!   (<https://openrouter.ai/docs/guides/features/message-transforms>)
//! - Every plugin can be switched on for the whole account. "If a plugin is
//!   enabled in your account defaults but not specified in a request, the
//!   default configuration will be applied."
//!   (<https://openrouter.ai/docs/guides/features/plugins>)
//!
//! Both facts invert the naive implementation. Sending no `plugins` field is
//! not "off"; it is "OpenRouter and the account decide". So the policy always
//! serializes an entry for every transform it knows about, and the explicit
//! choice for a transform heycode does not want is a documented
//! `"enabled": false`, never omission.
//!
//! What the request cannot establish is the outcome. "When 'Prevent
//! overrides' is enabled for a plugin, individual API requests cannot disable
//! or modify that plugin's configuration."
//! (<https://openrouter.ai/docs/guides/features/plugins>) A request is
//! therefore evidence of what heycode asked for and never evidence of what ran,
//! which is why every activation reports
//! [`CapabilitySupport::Unknown`] for its effective state.

use std::fmt;

use heycode_core::ProviderRequestOption;
use heycode_llm::{CapabilitySupport, PriceCurrency};
use serde_json::{Map, Value};

/// Pico-units in one currency unit, matching the shared pricing convention in
/// [`heycode_llm::TokenPrice`]: prices are exact integers, never floats.
const PICO_UNITS_PER_CURRENCY_UNIT: u64 = 1_000_000_000_000;

/// Durable option kind carrying the OpenRouter transform decision.
pub const OPENROUTER_TRANSFORM_OPTION_KIND: &str = "transforms";
/// Exact top-level OpenRouter request field the option projects onto.
pub const OPENROUTER_TRANSFORM_WIRE_FIELD: &str = "plugins";

/// The OpenRouter request plugins heycode controls.
///
/// Deliberately closed. A new gateway transform must break every match rather
/// than pass through unnoticed, because passing through unnoticed is the
/// failure this module exists to prevent.
///
/// Two documented OpenRouter plugins are deliberately outside this set, and a
/// reader should know the prohibition does not reach them:
///
/// - `web` is deprecated — "The `:online` variant and the web search plugin
///   are deprecated. Use the `openrouter:web_search` server tool instead." —
///   and that server tool is POR05's, reaching the wire through the
///   server-tool path. Splitting one feature across two request mechanisms
///   would let each hide the other.
/// - `pareto-router` configures a coding quality tier for OpenRouter's Pareto
///   code router, a model heycode does not select.
///
/// Neither is disabled by this policy, so an account default for either still
/// applies. That is a known gap, not an oversight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenRouterTransform {
    /// Middle-out prompt compression
    /// (<https://openrouter.ai/docs/guides/features/message-transforms>).
    ContextCompression,
    /// Gateway document parsing
    /// (<https://openrouter.ai/docs/guides/overview/multimodal/pdfs>).
    ///
    /// Its disable rests on the general plugins contract — "you can disable it
    /// for a specific request by passing `"enabled": false` in the plugins
    /// array" — because the PDF page itself never shows an `enabled` field.
    /// That is the weakest documentary footing of the three, and POR07 live
    /// conformance is what would settle it.
    FileParser,
    /// Malformed-JSON repair
    /// (<https://openrouter.ai/docs/guides/features/plugins/response-healing>).
    ResponseHealing,
}

impl OpenRouterTransform {
    /// Every transform heycode controls, in stable wire order.
    pub const ALL: [Self; 3] = [
        Self::ContextCompression,
        Self::FileParser,
        Self::ResponseHealing,
    ];

    /// Documented OpenRouter plugin id.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ContextCompression => "context-compression",
            Self::FileParser => "file-parser",
            Self::ResponseHealing => "response-healing",
        }
    }

    /// Whether OpenRouter has a built-in API default for this transform,
    /// independently of account plugin settings.
    ///
    /// True only for context compression, and only because the documentation
    /// says so for endpoints of 8k context or less. A `true` here is the
    /// reason the disable must be sent explicitly rather than implied by
    /// leaving the plugin out of the request.
    #[must_use]
    pub const fn defaults_on_upstream(self) -> bool {
        match self {
            Self::ContextCompression => true,
            Self::FileParser | Self::ResponseHealing => false,
        }
    }

    /// Whether an OpenRouter account default can enable this transform when a
    /// request omits it.
    ///
    /// True for every plugin in this set. This is why the all-disabled policy
    /// must serialize every row, not only context compression's API default.
    #[must_use]
    pub const fn account_default_can_enable(self) -> bool {
        match self {
            Self::ContextCompression | Self::FileParser | Self::ResponseHealing => true,
        }
    }

    /// Whether OpenRouter documents this transform as non-streaming only.
    ///
    /// This is a prerequisite, not an availability claim for the other
    /// transforms. Response healing cannot be enabled on heycode's current
    /// streaming Chat route.
    #[must_use]
    pub const fn requires_non_streaming(self) -> bool {
        match self {
            Self::ResponseHealing => true,
            Self::ContextCompression | Self::FileParser => false,
        }
    }

    /// Whether the transform requires a structured-output request.
    ///
    /// OpenRouter documents response healing only with `response_format`
    /// `json_schema` or `json_object`.
    #[must_use]
    pub const fn requires_structured_output(self) -> bool {
        match self {
            Self::ResponseHealing => true,
            Self::ContextCompression | Self::FileParser => false,
        }
    }

    /// What a surface must tell the user this transform changes.
    #[must_use]
    pub const fn effect(self) -> OpenRouterTransformEffect {
        match self {
            Self::ContextCompression => OpenRouterTransformEffect::RewritesPromptAndRoute,
            Self::FileParser => OpenRouterTransformEffect::ParsesDocuments,
            Self::ResponseHealing => OpenRouterTransformEffect::RewritesResponse,
        }
    }
}

/// The change a transform makes, beyond its price.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenRouterTransformEffect {
    /// Removes or truncates messages from the middle of the prompt, and may
    /// route the call to a different model than the one requested: "OpenRouter
    /// will first try to find models whose context length is at least half of
    /// your total required tokens".
    RewritesPromptAndRoute,
    /// Rewrites the model's response body before heycode observes it.
    RewritesResponse,
    /// Parses request documents through a gateway engine that may be billed
    /// separately from tokens.
    ParsesDocuments,
}

/// A PDF parsing engine heycode can pin explicitly.
///
/// Pinning matters because the unpinned default is model-dependent: "If you
/// don't explicitly specify an engine, OpenRouter will default first to the
/// model's native file processing capabilities, and if that's not available,
/// we will use the mistral-ocr engine."
/// (<https://openrouter.ai/docs/guides/overview/multimodal/pdfs>)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenRouterPdfEngine {
    /// Mistral OCR, published at "$2 per 1,000 pages".
    MistralOcr,
    /// Cloudflare Workers AI markdown conversion, published as "(Free)".
    CloudflareAi,
    /// The upstream model's own file input, "charged as input tokens".
    Native,
}

impl OpenRouterPdfEngine {
    /// Documented OpenRouter engine name.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::MistralOcr => "mistral-ocr",
            Self::CloudflareAi => "cloudflare-ai",
            Self::Native => "native",
        }
    }

    /// The published price for parsing documents with this engine.
    ///
    /// The gateway fee is charged even when the upstream model is reached with
    /// the caller's own key: "OCR costs apply to all requests, including BYOK."
    #[must_use]
    pub const fn cost(self) -> OpenRouterTransformCost {
        match self {
            Self::MistralOcr => OpenRouterTransformCost::PerThousandPages(OpenRouterPagePrice {
                currency: PriceCurrency::Usd,
                pico_units_per_thousand_pages: 2 * PICO_UNITS_PER_CURRENCY_UNIT,
            }),
            Self::CloudflareAi => OpenRouterTransformCost::DocumentedFree,
            Self::Native => OpenRouterTransformCost::UpstreamInputTokens,
        }
    }
}

/// One published gateway document fee, as an exact integer amount in the unit
/// OpenRouter publishes it in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OpenRouterPagePrice {
    currency: PriceCurrency,
    pico_units_per_thousand_pages: u64,
}

impl OpenRouterPagePrice {
    /// Currency the fee is published in.
    #[must_use]
    pub const fn currency(self) -> PriceCurrency {
        self.currency
    }

    /// Exact fee for one thousand parsed pages, in pico-units of
    /// [`Self::currency`].
    #[must_use]
    pub const fn pico_units_per_thousand_pages(self) -> u64 {
        self.pico_units_per_thousand_pages
    }
}

/// What OpenRouter bills for one transform.
///
/// [`Self::Unknown`] and [`Self::DocumentedFree`] are different claims and must
/// render differently. An absent published price is unknown, never free.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenRouterTransformCost {
    /// OpenRouter publishes no price heycode can attribute to this transform, or
    /// the price depends on a choice heycode did not pin. A surface must render
    /// this as unknown and must never render it as zero.
    Unknown,
    /// OpenRouter documents this transform as costing nothing.
    DocumentedFree,
    /// Billed as ordinary upstream input tokens; the model's published input
    /// price already covers it and there is no separate gateway fee.
    UpstreamInputTokens,
    /// A published gateway fee per thousand parsed pages.
    PerThousandPages(OpenRouterPagePrice),
}

impl OpenRouterTransformCost {
    /// The cost of sending a document without pinning an engine.
    ///
    /// OpenRouter chooses native parsing when the model supports files and
    /// billed OCR otherwise, so the fee follows the model rather than the
    /// request and cannot be known from the request alone.
    pub const UNPINNED_DOCUMENT: Self = Self::Unknown;

    /// True only when OpenRouter documents this as costing nothing.
    ///
    /// [`Self::Unknown`] answers `false`: an unpublished price is not a zero
    /// price, and a surface that treats the two alike understates cost.
    #[must_use]
    pub const fn is_documented_free(self) -> bool {
        matches!(self, Self::DocumentedFree)
    }

    /// The published fee, when one exists.
    #[must_use]
    pub const fn published_price(self) -> Option<OpenRouterPagePrice> {
        match self {
            Self::PerThousandPages(price) => Some(price),
            Self::Unknown | Self::DocumentedFree | Self::UpstreamInputTokens => None,
        }
    }
}

/// What heycode asked OpenRouter to do with one transform.
///
/// Never what OpenRouter did: see [`OpenRouterTransformActivation::effective`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OpenRouterTransformRequest {
    /// heycode sent an explicit `"enabled": false` for this transform. This
    /// overrides ordinary API/account defaults, but an account with "Prevent
    /// overrides" may still enforce a different outcome.
    Disabled,
    /// heycode explicitly asked for this transform.
    Enabled,
}

impl OpenRouterTransformRequest {
    /// Whether heycode asked for the transform to run.
    #[must_use]
    pub const fn is_enabled(self) -> bool {
        matches!(self, Self::Enabled)
    }
}

/// The durable record that one transform decision was made, and at what price.
///
/// A policy produces exactly one of these per known transform, enabled or not,
/// so a surface that renders the record cannot omit a transform that was
/// switched on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRouterTransformActivation {
    transform: OpenRouterTransform,
    request: OpenRouterTransformRequest,
    cost: Option<OpenRouterTransformCost>,
    pdf_engine: Option<OpenRouterPdfEngine>,
}

impl OpenRouterTransformActivation {
    /// Which transform this records.
    #[must_use]
    pub const fn transform(self) -> OpenRouterTransform {
        self.transform
    }

    /// What heycode asked for.
    #[must_use]
    pub const fn request(self) -> OpenRouterTransformRequest {
        self.request
    }

    /// What the transform costs under this decision, present only when heycode
    /// asked for it to run.
    ///
    /// `None` says heycode requested no fee-bearing transform, not that the
    /// transform is free or that an enforced account override could not run
    /// it.
    #[must_use]
    pub const fn cost(self) -> Option<OpenRouterTransformCost> {
        self.cost
    }

    /// The pinned document engine, present only when document parsing is on.
    #[must_use]
    pub const fn pdf_engine(self) -> Option<OpenRouterPdfEngine> {
        self.pdf_engine
    }

    /// Whether the transform actually ran.
    ///
    /// Always [`CapabilitySupport::Unknown`], and not out of caution. An
    /// account may enforce a plugin configuration that a request cannot
    /// change: "When 'Prevent overrides' is enabled for a plugin, individual
    /// API requests cannot disable or modify that plugin's configuration."
    /// heycode holds the request, not the account setting and not the response,
    /// so the request proves what was asked and nothing about the outcome.
    /// Promoting this to `Supported` or `Unsupported` would report a guess as
    /// evidence.
    #[must_use]
    pub const fn effective(self) -> CapabilitySupport {
        CapabilitySupport::Unknown
    }
}

/// Explicit transform decision for OpenRouter requests.
///
/// There is deliberately no `Default`: the whole point of the row is that the
/// transform set is chosen, and a derived default is a place for one to drift
/// back on. Start from [`Self::all_disabled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRouterTransformPolicy {
    context_compression: bool,
    document_parsing: Option<OpenRouterPdfEngine>,
    response_healing: bool,
}

/// Request facts needed to validate a transform policy before it becomes a
/// durable provider option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpenRouterTransformRequestContext {
    streaming: bool,
    structured_output: bool,
}

impl OpenRouterTransformRequestContext {
    /// Construct the request facts that affect OpenRouter plugin eligibility.
    #[must_use]
    pub const fn new(streaming: bool, structured_output: bool) -> Self {
        Self {
            streaming,
            structured_output,
        }
    }

    /// Whether the request streams its response.
    #[must_use]
    pub const fn streaming(self) -> bool {
        self.streaming
    }

    /// Whether the request carries a structured `response_format`.
    #[must_use]
    pub const fn structured_output(self) -> bool {
        self.structured_output
    }
}

/// Failure to materialize an OpenRouter transform policy for one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenRouterTransformPolicyError {
    /// Response healing is documented as non-streaming only.
    ResponseHealingRequiresNonStreaming,
    /// Response healing requires `json_schema` or `json_object` output.
    ResponseHealingRequiresStructuredOutput,
    /// The fixed provider-option envelope failed shared boundary validation.
    InvalidProviderOption,
}

impl fmt::Display for OpenRouterTransformPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::ResponseHealingRequiresNonStreaming => {
                "OpenRouter response healing requires a non-streaming request"
            }
            Self::ResponseHealingRequiresStructuredOutput => {
                "OpenRouter response healing requires structured output"
            }
            Self::InvalidProviderOption => "OpenRouter transform option is invalid",
        })
    }
}

impl std::error::Error for OpenRouterTransformPolicyError {}

impl OpenRouterTransformPolicy {
    /// Every transform explicitly disabled.
    ///
    /// This is not the same as sending no `plugins` field. Omission lets
    /// OpenRouter's small-endpoint default and the account plugin defaults
    /// decide; this sends a documented `"enabled": false` for each one.
    #[must_use]
    pub const fn all_disabled() -> Self {
        Self {
            context_compression: false,
            document_parsing: None,
            response_healing: false,
        }
    }

    /// Explicitly enable middle-out context compression.
    ///
    /// Enabling accepts that prompt content may be dropped and that the call
    /// may be routed to a different model.
    #[must_use]
    pub const fn with_context_compression(mut self) -> Self {
        self.context_compression = true;
        self
    }

    /// Explicitly enable response healing.
    ///
    /// OpenRouter applies it only to a non-streaming request carrying
    /// `response_format` with `json_schema` or `json_object`. The current heycode
    /// strict Chat route streams, so its product integration must keep this
    /// disabled until a compatible request surface exists.
    #[must_use]
    pub const fn with_response_healing(mut self) -> Self {
        self.response_healing = true;
        self
    }

    /// Explicitly enable document parsing with a pinned engine.
    ///
    /// The engine is required rather than optional: leaving it to OpenRouter
    /// is what makes a per-page fee unpredictable, and an unpredictable fee is
    /// the billing surprise this row exists to prevent.
    #[must_use]
    pub const fn with_document_parsing(mut self, engine: OpenRouterPdfEngine) -> Self {
        self.document_parsing = Some(engine);
        self
    }

    /// Whether heycode asked for this transform.
    #[must_use]
    pub const fn request(&self, transform: OpenRouterTransform) -> OpenRouterTransformRequest {
        let enabled = match transform {
            OpenRouterTransform::ContextCompression => self.context_compression,
            OpenRouterTransform::FileParser => self.document_parsing.is_some(),
            OpenRouterTransform::ResponseHealing => self.response_healing,
        };
        if enabled {
            OpenRouterTransformRequest::Enabled
        } else {
            OpenRouterTransformRequest::Disabled
        }
    }

    /// The exact `plugins` array for one request.
    ///
    /// Always carries one entry per known transform, in
    /// [`OpenRouterTransform::ALL`] order. Enabled entries use OpenRouter's
    /// documented bare-id form; disabled entries use the documented
    /// `"enabled": false` form.
    #[must_use]
    pub fn plugins_wire(&self) -> Value {
        Value::Array(
            OpenRouterTransform::ALL
                .into_iter()
                .map(|transform| self.plugin_entry(transform))
                .collect(),
        )
    }

    fn plugin_entry(&self, transform: OpenRouterTransform) -> Value {
        let mut entry = Map::new();
        entry.insert("id".to_owned(), Value::String(transform.id().to_owned()));
        // Exhaustive on purpose: a new transform must be given an explicit
        // wire form here rather than inheriting one.
        match transform {
            OpenRouterTransform::ContextCompression if self.context_compression => {}
            OpenRouterTransform::ResponseHealing if self.response_healing => {}
            OpenRouterTransform::FileParser => match self.document_parsing {
                Some(engine) => {
                    let mut pdf = Map::new();
                    pdf.insert("engine".to_owned(), Value::String(engine.id().to_owned()));
                    entry.insert("pdf".to_owned(), Value::Object(pdf));
                }
                None => {
                    entry.insert("enabled".to_owned(), Value::Bool(false));
                }
            },
            OpenRouterTransform::ContextCompression | OpenRouterTransform::ResponseHealing => {
                entry.insert("enabled".to_owned(), Value::Bool(false));
            }
        }
        Value::Object(entry)
    }

    /// One activation record per known transform, in wire order.
    #[must_use]
    pub fn activations(&self) -> Vec<OpenRouterTransformActivation> {
        OpenRouterTransform::ALL
            .into_iter()
            .map(|transform| OpenRouterTransformActivation {
                transform,
                request: self.request(transform),
                cost: self.cost(transform),
                pdf_engine: match transform {
                    OpenRouterTransform::FileParser => self.document_parsing,
                    OpenRouterTransform::ContextCompression
                    | OpenRouterTransform::ResponseHealing => None,
                },
            })
            .collect()
    }

    /// The records for transforms heycode actually switched on.
    ///
    /// A surface that only has room for what changed renders these; a surface
    /// showing the full decision renders [`Self::activations`].
    #[must_use]
    pub fn enabled_activations(&self) -> Vec<OpenRouterTransformActivation> {
        self.activations()
            .into_iter()
            .filter(|activation| activation.request().is_enabled())
            .collect()
    }

    fn cost(&self, transform: OpenRouterTransform) -> Option<OpenRouterTransformCost> {
        if !self.request(transform).is_enabled() {
            // The request asks for no fee-bearing transform. That is not a
            // claim the transform is free or that an enforced account
            // override could not run it; `effective` remains Unknown.
            return None;
        }
        Some(match transform {
            // The message-transforms documentation publishes no fee for
            // compression. Absent is unknown, not free.
            OpenRouterTransform::ContextCompression => OpenRouterTransformCost::Unknown,
            OpenRouterTransform::FileParser => self
                .document_parsing
                .map_or(OpenRouterTransformCost::UNPINNED_DOCUMENT, |engine| {
                    engine.cost()
                }),
            // "The plugin is free to use."
            // <https://openrouter.ai/blog/response-healing-reduce-json-defects-by-80percent/>
            OpenRouterTransform::ResponseHealing => OpenRouterTransformCost::DocumentedFree,
        })
    }

    /// Project the decision into a durable provider request option.
    ///
    /// The array is wrapped under [`OPENROUTER_TRANSFORM_WIRE_FIELD`] because
    /// a provider request option's data must be a JSON object; the consuming
    /// wire dialect unwraps that one key onto the top-level request field.
    ///
    /// # Errors
    /// Response healing on a streaming or non-structured request is refused.
    /// Shared provider-option validation failure is also reported, though the
    /// fixed identity and bounded plugin array cannot currently produce it.
    pub fn provider_option(
        &self,
        context: OpenRouterTransformRequestContext,
    ) -> Result<ProviderRequestOption, OpenRouterTransformPolicyError> {
        if self.response_healing && context.streaming() {
            return Err(OpenRouterTransformPolicyError::ResponseHealingRequiresNonStreaming);
        }
        if self.response_healing && !context.structured_output() {
            return Err(OpenRouterTransformPolicyError::ResponseHealingRequiresStructuredOutput);
        }
        let mut data = Map::new();
        data.insert(
            OPENROUTER_TRANSFORM_WIRE_FIELD.to_owned(),
            self.plugins_wire(),
        );
        ProviderRequestOption::new(
            heycode_llm::OpenRouterProvider::NAME,
            OPENROUTER_TRANSFORM_OPTION_KIND,
            Value::Object(data),
        )
        .map_err(|_| OpenRouterTransformPolicyError::InvalidProviderOption)
    }
}
