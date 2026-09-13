//! MiniMax regions, protocol families and the evidence behind each base URL.

use heycode_core::ProviderProtocol;
use heycode_llm::CapabilitySupport;

/// Regional MiniMax deployment.
///
/// Accounts, balances and Token Plan seats live on one deployment; the hosts
/// are not aliases of each other, so the region is part of a route rather than
/// a convenience default.
///
/// Sources: <https://platform.minimax.io/docs/token-plan/claude-code> shows
/// `https://api.minimax.io/anthropic` for international accounts and
/// `https://api.minimaxi.com/anthropic` for accounts in China.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxRegion {
    /// International deployment on `api.minimax.io`.
    International,
    /// Mainland-China deployment on `api.minimaxi.com`.
    MainlandChina,
}

impl MiniMaxRegion {
    /// Every documented region, in a stable order.
    pub const ALL: [Self; 2] = [Self::International, Self::MainlandChina];

    /// Documented API host for this region.
    #[must_use]
    pub const fn host(self) -> &'static str {
        match self {
            Self::International => "api.minimax.io",
            Self::MainlandChina => "api.minimaxi.com",
        }
    }
}

/// Wire protocol family MiniMax exposes.
///
/// MiniMax serves the same models under two request dialects, each on its own
/// base path.
///
/// Sources: <https://platform.minimax.io/docs/guides/text-generation> and
/// <https://platform.minimax.io/docs/token-plan/other-tools> both give
/// OpenAI-compatible `https://api.minimax.io/v1` and Anthropic-compatible
/// `https://api.minimax.io/anthropic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxApiFamily {
    /// OpenAI Chat Completions dialect under `/v1`.
    OpenAiCompatible,
    /// Anthropic Messages dialect under `/anthropic`.
    AnthropicCompatible,
}

impl MiniMaxApiFamily {
    /// Every documented family, in a stable order.
    pub const ALL: [Self; 2] = [Self::OpenAiCompatible, Self::AnthropicCompatible];

    /// Documented base path this family is served from.
    #[must_use]
    pub const fn base_path(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "/v1",
            Self::AnthropicCompatible => "/anthropic",
        }
    }

    /// heycode protocol this family speaks.
    #[must_use]
    pub const fn protocol(self) -> ProviderProtocol {
        match self {
            Self::OpenAiCompatible => ProviderProtocol::OpenAiChatCompletions,
            Self::AnthropicCompatible => ProviderProtocol::AnthropicMessages,
        }
    }

    /// Path from this family's base URL to its model-list endpoint.
    ///
    /// Sources: the OpenAI-compatible list is
    /// `GET https://api.minimax.io/v1/models`
    /// (<https://platform.minimax.io/docs/api-reference/models/openai/list-models>)
    /// and the Anthropic-compatible list is
    /// `GET https://api.minimax.io/anthropic/v1/models`
    /// (<https://platform.minimax.io/docs/api-reference/models/anthropic/list-models>).
    /// The base paths already carry `/v1` and `/anthropic` respectively, so the
    /// remainders differ.
    #[must_use]
    pub const fn list_models_path(self) -> &'static str {
        match self {
            Self::OpenAiCompatible => "/models",
            Self::AnthropicCompatible => "/v1/models",
        }
    }

    /// Authentication scheme documented for this family's model-list endpoint.
    ///
    /// The two reference pages document different schemes, and each is
    /// authoritative for its own route: the OpenAI-compatible list specifies
    /// HTTP Bearer auth, the Anthropic-compatible list specifies `X-Api-Key`.
    /// This says nothing about the inference endpoints, whose scheme MiniMax
    /// documents inconsistently — that stays open for PMM03.
    #[must_use]
    pub const fn list_models_auth(self) -> MiniMaxAuthScheme {
        match self {
            Self::OpenAiCompatible => MiniMaxAuthScheme::AuthorizationBearer,
            Self::AnthropicCompatible => MiniMaxAuthScheme::XApiKey,
        }
    }
}

/// How a MiniMax route carries its credential.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MiniMaxAuthScheme {
    /// `Authorization: Bearer <key>`.
    AuthorizationBearer,
    /// `X-Api-Key: <key>`.
    XApiKey,
}

impl MiniMaxAuthScheme {
    /// Lowercase header name this scheme writes.
    #[must_use]
    pub const fn header_name(self) -> &'static str {
        match self {
            Self::AuthorizationBearer => "authorization",
            Self::XApiKey => "x-api-key",
        }
    }

    /// Render the header value carrying `secret`.
    ///
    /// The returned `String` holds credential material. Build it immediately
    /// before the request that consumes it and never store or log it.
    #[must_use]
    pub fn header_value(self, secret: &str) -> String {
        match self {
            Self::AuthorizationBearer => format!("Bearer {secret}"),
            Self::XApiKey => secret.to_owned(),
        }
    }
}

/// Absolute base URL for one region and family.
#[must_use]
pub fn base_url(region: MiniMaxRegion, family: MiniMaxApiFamily) -> String {
    format!("https://{}{}", region.host(), family.base_path())
}

/// Whether MiniMax's own documentation shows this region serving this family.
///
/// Only the pairs an official page actually shows are `Supported`. The
/// mainland-China OpenAI-compatible route is a plausible composition of two
/// documented halves and is exactly the kind of guess that reads as correct
/// until a request fails, so it stays `Unknown` and is never promoted.
#[must_use]
pub const fn documented_base_url(
    region: MiniMaxRegion,
    family: MiniMaxApiFamily,
) -> CapabilitySupport {
    match (region, family) {
        // <https://platform.minimax.io/docs/guides/text-generation>,
        // <https://platform.minimax.io/docs/api-reference/text-openai-api>,
        // <https://platform.minimax.io/docs/token-plan/other-tools>.
        (MiniMaxRegion::International, MiniMaxApiFamily::OpenAiCompatible) => {
            CapabilitySupport::Supported
        }
        // <https://platform.minimax.io/docs/guides/text-generation>,
        // <https://platform.minimax.io/docs/token-plan/quickstart>.
        (MiniMaxRegion::International, MiniMaxApiFamily::AnthropicCompatible) => {
            CapabilitySupport::Supported
        }
        // <https://platform.minimax.io/docs/token-plan/claude-code>.
        (MiniMaxRegion::MainlandChina, MiniMaxApiFamily::AnthropicCompatible) => {
            CapabilitySupport::Supported
        }
        // No MiniMax page read for PMM01 shows `api.minimaxi.com/v1`.
        (MiniMaxRegion::MainlandChina, MiniMaxApiFamily::OpenAiCompatible) => {
            CapabilitySupport::Unknown
        }
    }
}
