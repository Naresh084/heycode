//! Plan and protocol vocabulary, plus the compile-time plan markers.

use heycode_core::ProviderProtocol;

use crate::{
    ZAI_CODING_ANTHROPIC_BASE_URL, ZAI_CODING_API_KEY_REFERENCE, ZAI_CODING_CHAT_BASE_URL,
    ZAI_CODING_PLAN_USAGE_RESTRICTION, ZAI_CODING_RESPONSES_BASE_URL,
    ZAI_GENERAL_API_KEY_REFERENCE, ZAI_GENERAL_BASE_URL, ZaiAuthHeaderEvidence,
    ZaiPreservedThinking,
};

mod sealed {
    /// Prevents plan markers outside this crate.
    pub trait Sealed {}
}

/// Which Z.ai commercial plan serves a route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiPlan {
    /// Pay-as-you-go platform access through the general endpoint.
    General,
    /// GLM Coding Plan subscription access through the coding endpoints.
    Coding,
}

impl ZaiPlan {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Coding => "coding",
        }
    }
}

/// The wire protocols Z.ai publishes base URLs for.
///
/// This is deliberately a closed enum rather than the `#[non_exhaustive]`
/// [`ProviderProtocol`]: it mirrors exactly the rows of Z.ai's documented
/// endpoint table, so a protocol added to core cannot silently fall into a
/// wildcard arm here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZaiProtocol {
    /// Anthropic Messages.
    AnthropicMessages,
    /// OpenAI Chat Completions.
    OpenAiChatCompletions,
    /// OpenAI Responses.
    OpenAiResponses,
}

impl ZaiProtocol {
    /// Stable lowercase identifier.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AnthropicMessages => "anthropic-messages",
            Self::OpenAiChatCompletions => "openai-chat-completions",
            Self::OpenAiResponses => "openai-responses",
        }
    }
}

impl From<ZaiProtocol> for ProviderProtocol {
    fn from(protocol: ZaiProtocol) -> Self {
        match protocol {
            ZaiProtocol::AnthropicMessages => Self::AnthropicMessages,
            ZaiProtocol::OpenAiChatCompletions => Self::OpenAiChatCompletions,
            ZaiProtocol::OpenAiResponses => Self::OpenAiResponses,
        }
    }
}

/// One Z.ai plan, resolved at compile time.
///
/// Implemented only by [`General`] and [`Coding`], which is what makes a
/// cross-plan endpoint/credential pair a type error rather than a runtime
/// check a caller could skip.
pub trait ZaiPlanKind: sealed::Sealed {
    /// Runtime tag for this plan.
    const PLAN: ZaiPlan;
    /// Provider registry name and descriptor id.
    const REGISTRY_NAME: &'static str;
    /// Human display name.
    const DISPLAY_NAME: &'static str;
    /// Default non-secret credential reference for this plan.
    const DEFAULT_CREDENTIAL_REFERENCE: &'static str;
    /// The other plan's default reference, which may never back this plan.
    const FOREIGN_CREDENTIAL_REFERENCE: &'static str;
    /// Published restriction on where this plan's key may be used.
    const USAGE_RESTRICTION: Option<&'static str>;
    /// Name of the plugin that contributes this plan's model catalog.
    const CATALOG_PLUGIN_NAME: &'static str;
    /// What this plan's endpoint does with replayed `reasoning_content`.
    const PRESERVED_THINKING: ZaiPreservedThinking;
    /// Provenance of the request auth header this plan's route sends.
    const AUTH_HEADER: ZaiAuthHeaderEvidence;

    /// The base URL Z.ai documents for this plan and protocol.
    ///
    /// `None` means Z.ai publishes none — an absence heycode reports rather than
    /// fills in.
    #[must_use]
    fn documented_base_url(protocol: ZaiProtocol) -> Option<&'static str>;
}

/// Compile-time marker for the general pay-as-you-go plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct General;

impl sealed::Sealed for General {}

impl ZaiPlanKind for General {
    const PLAN: ZaiPlan = ZaiPlan::General;
    const REGISTRY_NAME: &'static str = "zai";
    const DISPLAY_NAME: &'static str = "Z.AI";
    const DEFAULT_CREDENTIAL_REFERENCE: &'static str = ZAI_GENERAL_API_KEY_REFERENCE;
    const FOREIGN_CREDENTIAL_REFERENCE: &'static str = ZAI_CODING_API_KEY_REFERENCE;
    const USAGE_RESTRICTION: Option<&'static str> = None;
    const CATALOG_PLUGIN_NAME: &'static str = "catalog-zai";
    // Preserved Thinking is "disabled by default on the standard API
    // endpoint", so this endpoint clears prior-turn `reasoning_content`
    // unless a request carries `thinking.clear_thinking: false`.
    const PRESERVED_THINKING: ZaiPreservedThinking =
        ZaiPreservedThinking::RequiresClearThinkingFalse;
    // `Authorization: Bearer YOUR_API_KEY` is documented for this endpoint.
    const AUTH_HEADER: ZaiAuthHeaderEvidence = ZaiAuthHeaderEvidence::Documented;

    fn documented_base_url(protocol: ZaiProtocol) -> Option<&'static str> {
        match protocol {
            // The general platform documents one endpoint. The Anthropic and
            // Responses base URLs appear only under the Coding Plan, so this
            // plan reports them as undocumented instead of reusing a URL that
            // would spend a different entitlement.
            ZaiProtocol::OpenAiChatCompletions => Some(ZAI_GENERAL_BASE_URL),
            ZaiProtocol::AnthropicMessages | ZaiProtocol::OpenAiResponses => None,
        }
    }
}

/// Compile-time marker for the GLM Coding Plan subscription.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coding;

impl sealed::Sealed for Coding {}

impl ZaiPlanKind for Coding {
    const PLAN: ZaiPlan = ZaiPlan::Coding;
    const REGISTRY_NAME: &'static str = "zai-coding";
    const DISPLAY_NAME: &'static str = "Z.AI Coding Plan";
    const DEFAULT_CREDENTIAL_REFERENCE: &'static str = ZAI_CODING_API_KEY_REFERENCE;
    const FOREIGN_CREDENTIAL_REFERENCE: &'static str = ZAI_GENERAL_API_KEY_REFERENCE;
    const USAGE_RESTRICTION: Option<&'static str> = Some(ZAI_CODING_PLAN_USAGE_RESTRICTION);
    const CATALOG_PLUGIN_NAME: &'static str = "catalog-zai-coding";
    // Preserved Thinking is "enabled by default on the Coding Plan endpoint",
    // so a verbatim replay reaches the model without an extra request field.
    const PRESERVED_THINKING: ZaiPreservedThinking = ZaiPreservedThinking::EndpointDefault;
    // Z.ai publishes no request header for the Coding Plan inference
    // endpoints; the route sends `Authorization: Bearer` unverified.
    const AUTH_HEADER: ZaiAuthHeaderEvidence = ZaiAuthHeaderEvidence::Undocumented;

    fn documented_base_url(protocol: ZaiProtocol) -> Option<&'static str> {
        match protocol {
            ZaiProtocol::AnthropicMessages => Some(ZAI_CODING_ANTHROPIC_BASE_URL),
            ZaiProtocol::OpenAiChatCompletions => Some(ZAI_CODING_CHAT_BASE_URL),
            ZaiProtocol::OpenAiResponses => Some(ZAI_CODING_RESPONSES_BASE_URL),
        }
    }
}
