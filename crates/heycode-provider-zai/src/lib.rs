//! Z.ai General and GLM Coding Plan provider profiles.
//!
//! PZA01 owns one fact: the two Z.ai plans are separate products behind one
//! vendor. They publish different base URLs, they are entitled separately, and
//! Z.ai states that a Coding Plan key "is not interchangeable with other
//! Z.AI's API Keys". The plan is therefore part of the *type*: an endpoint and
//! a credential each carry their plan, so [`ZaiProfile::new`] cannot pair one
//! plan's endpoint with the other plan's credential — that mistake fails to
//! compile instead of failing in production.
//!
//! Nothing here resolves a secret. [`ZaiProfile::report`] inspects the
//! credential registry rather than resolving it (GOTCHAS #33) and returns a
//! [`ZaiProfileReport`] whose every field is non-secret by construction: the
//! plan, the endpoint, the credential reference and its provenance are all
//! visible, and there is no field a secret value could occupy.
//!
//! Model discovery, capability evidence and inference belong to PZA02/PZA03;
//! this crate encodes only the identity facts a profile needs and leaves every
//! unverified fact absent rather than guessing it.
//!
//! # Sources
//!
//! Every endpoint, reference and model code below cites the Z.ai page it came
//! from. Facts Z.ai does not publish are named as heycode-owned where they exist
//! and omitted where they do not.

mod catalog;
mod credential;
mod endpoint;
mod inference;
mod mcp_bundle;
mod plan;
mod profile;
mod web_search;

pub use catalog::{ZaiCatalog, zai_catalog_plugin};
pub use credential::ZaiCredential;
pub use endpoint::ZaiEndpoint;
pub use inference::{
    ZAI_ALWAYS_THINKING_MODELS, ZAI_DEFAULT_REASONING_EFFORT, ZAI_REASONING_EFFORT_MODELS,
    ZAI_REASONING_EFFORTS, ZaiAuthHeaderEvidence, ZaiInference, ZaiInferenceError,
    ZaiPreservedThinking, ZaiThinkingReport, zai_thinking_reports,
};
pub use mcp_bundle::{
    ZAI_CODING_MCP_PLUGIN_ID, ZAI_MCP_READER_ENDPOINT, ZAI_MCP_SEARCH_ENDPOINT,
    ZAI_MCP_VISION_MINIMUM_NODE_MAJOR, ZAI_MCP_VISION_MINIMUM_VERSION, ZAI_MCP_VISION_PACKAGE,
    ZAI_MCP_ZREAD_ENDPOINT, ZaiCodingMcpBundle, ZaiCodingMcpHost, ZaiMcpApproval,
    ZaiMcpAuthorization, ZaiMcpBundleError, ZaiMcpCredentialEnvironment, ZaiMcpExposurePolicy,
    ZaiMcpHostFailure, ZaiMcpServerSpec, ZaiMcpTransportSpec, ZaiVisionMcpLaunch,
    zai_coding_mcp_bundle_plugin,
};
pub use plan::{Coding, General, ZaiPlan, ZaiPlanKind, ZaiProtocol};
pub use profile::{ZaiProfile, ZaiProfileReport, zai_plan_reports};
pub use web_search::{
    ProjectedZaiWebSearch, ZAI_WEB_SEARCH_ENDPOINT, ZAI_WEB_SEARCH_IMPLEMENTATION,
    ZAI_WEB_SEARCH_LOGICAL, ZAI_WEB_SEARCH_TOOL_NAME, ZaiWebSearchClient, ZaiWebSearchContribution,
    ZaiWebSearchError, ZaiWebSearchRecord, ZaiWebSearchRequest, ZaiWebSearchResultMetadata,
    zai_web_search_contribution,
};

/// General (pay-as-you-go) API base URL.
///
/// Source: <https://docs.z.ai/guides/develop/http/introduction> — "General API
/// Endpoint" `https://api.z.ai/api/paas/v4/`. The trailing slash is dropped so
/// a path is joined exactly once.
pub const ZAI_GENERAL_BASE_URL: &str = "https://api.z.ai/api/paas/v4";

/// GLM Coding Plan base URL for the Anthropic Messages protocol.
///
/// Source: <https://docs.z.ai/devpack/quick-start> — "Endpoint Guide" table.
pub const ZAI_CODING_ANTHROPIC_BASE_URL: &str = "https://api.z.ai/api/anthropic";

/// GLM Coding Plan base URL for the OpenAI Chat Completions protocol.
///
/// Source: <https://docs.z.ai/devpack/quick-start> — "Endpoint Guide" table.
pub const ZAI_CODING_CHAT_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";

/// GLM Coding Plan base URL for the OpenAI Responses protocol.
///
/// Source: <https://docs.z.ai/devpack/quick-start> — "Endpoint Guide" table.
pub const ZAI_CODING_RESPONSES_BASE_URL: &str = "https://api.z.ai/api/v1";

/// Non-secret reference for the general-plan API key.
///
/// Z.ai documents this exact environment variable for the general endpoint:
/// "It is recommended to set the API Key as an environment variable: `export
/// ZAI_API_KEY=your-api-key`"
/// (<https://docs.z.ai/guides/develop/python/introduction>).
pub const ZAI_GENERAL_API_KEY_REFERENCE: &str = "ZAI_API_KEY";

/// Non-secret reference for the GLM Coding Plan key.
///
/// Z.ai publishes no environment-variable name of its own for this key — its
/// integration guides configure the host tool's variable instead (the Claude
/// Code guide sets `ANTHROPIC_AUTH_TOKEN`). This name is therefore heycode-owned,
/// not a documented vendor fact, and exists so the Coding Plan key can never
/// occupy the same store slot as the general key: every credential provider in
/// this workspace keys on the reference alone.
pub const ZAI_CODING_API_KEY_REFERENCE: &str = "ZAI_CODING_API_KEY";

/// Current flagship GLM model code, offered on both plans.
///
/// Sources: <https://docs.z.ai/guides/llm/glm-5.3> publishes model code
/// `glm-5.3`, and <https://docs.z.ai/devpack/latest-model> confirms the Coding
/// Plan serves it. Which model a plan *defaults* to is a heycode product contract
/// (GOTCHAS #56), not a Z.ai fact; the rest of the catalog is PZA02's.
pub const ZAI_GLM_5_3: &str = "glm-5.3";

/// Z.ai's published restriction on where a GLM Coding Plan key may be used.
///
/// Source: <https://docs.z.ai/devpack/usage-policy> — "GLM Coding Plan may only
/// be used within officially supported tools and products. Use in unsupported
/// tools may result in restricted benefits." heycode is not on that list, so this
/// notice travels with every Coding Plan report instead of being buried in a
/// design document.
pub const ZAI_CODING_PLAN_USAGE_RESTRICTION: &str = "Z.ai restricts GLM Coding Plan keys to its officially supported tools and products; using one elsewhere may restrict plan benefits";

/// Semantic credential kind for both plans.
///
/// Both plans present a plain Z.ai API key rather than different credential
/// types, so they share the S04 kind and are separated by reference — the
/// field every credential provider actually keys on.
///
/// Z.ai documents the exact header `Authorization: Bearer YOUR_API_KEY` for
/// the general endpoint (<https://docs.z.ai/guides/develop/http/introduction>)
/// and for the Coding Plan MCP servers
/// (<https://docs.z.ai/devpack/mcp/search-mcp-server>). It publishes no header
/// for the Coding Plan *inference* endpoints; those guides only say to enter
/// the API key in each tool. PZA03 owns the per-protocol request header and
/// must verify it against a live call rather than assume the general one
/// carries over.
const ZAI_CREDENTIAL_KIND: &str = "api-key";

/// Failures constructing or inspecting a Z.ai plan profile.
///
/// Every field is non-secret: plans, protocols and credential references are
/// public identifiers, and inspection failures carry only the credential
/// registry's already-redacted text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ZaiProfileError {
    /// Z.ai publishes no base URL for this plan and protocol, and heycode will
    /// not invent one.
    UndocumentedEndpoint {
        /// Plan that was asked for an endpoint.
        plan: ZaiPlan,
        /// Protocol Z.ai does not document for that plan.
        protocol: ZaiProtocol,
    },
    /// A reference that is the *other* plan's default cannot back this plan.
    CrossPlanReference {
        /// Plan the credential was being built for.
        plan: ZaiPlan,
        /// Rejected non-secret reference.
        reference: String,
    },
    /// A non-secret identifier was rejected by the S04 vocabulary.
    InvalidReference {
        /// Rejected non-secret identifier.
        reference: String,
    },
    /// The credential registry failed while inspecting a reference.
    Credentials {
        /// Already-redacted registry text.
        message: String,
    },
}

impl std::fmt::Display for ZaiProfileError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UndocumentedEndpoint { plan, protocol } => write!(
                formatter,
                "Z.ai publishes no {} base URL for the {} plan",
                protocol.as_str(),
                plan.as_str()
            ),
            Self::CrossPlanReference { plan, reference } => write!(
                formatter,
                "credential reference `{reference}` is the other Z.ai plan's default and cannot back the {} plan",
                plan.as_str()
            ),
            Self::InvalidReference { reference } => {
                write!(formatter, "invalid Z.ai credential reference `{reference}`")
            }
            Self::Credentials { message } => {
                write!(formatter, "Z.ai credential inspection failed: {message}")
            }
        }
    }
}

impl std::error::Error for ZaiProfileError {}
