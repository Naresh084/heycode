//! MiniMax provider profiles and credential kinds.
//!
//! MiniMax sells the same models twice. *Pay-as-you-go* meters usage against
//! an account balance and authenticates with an **API Key**; the *Token Plan*
//! is a fixed-price subscription plus purchased Credits and authenticates with
//! a **Subscription Key**. MiniMax states the two are not interchangeable, yet
//! both products reach the same two base URLs with the same
//! `Authorization: Bearer` syntax, and MiniMax's own guides tell you to put
//! either one in a single `MINIMAX_API_KEY` environment variable. Nothing
//! about the wire distinguishes them, so nothing about the wire can be relied
//! on to keep them apart.
//!
//! PMM01 therefore makes the plan part of the *type* of a MiniMax credential.
//! [`MiniMaxCredential<P>`] is parameterised by a sealed [`MiniMaxPlan`] marker
//! and is only ever minted by a [`MiniMaxProfile<P>`] carrying the same
//! parameter, so a value admitted for one product cannot be handed to code
//! that named the other — that is a compile error, not a runtime check a caller
//! could skip. Three further barriers sit underneath it: the two products use
//! different S04 credential references, different S04 credential kinds, and —
//! where MiniMax documents a key prefix — admission refuses a secret carrying
//! the other product's prefix.
//!
//! Every MiniMax fact encoded here cites the page it came from. Facts MiniMax
//! does not publish stay [`heycode_llm::CapabilitySupport::Unknown`] or [`None`];
//! the pay-as-you-go key prefix and the mainland-China OpenAI-compatible base
//! URL are both in that category.
//!
//! PMM02 adds the first real Consumer of that boundary:
//! [`MiniMaxCatalog`] discovers models over either of MiniMax's two documented
//! list endpoints, resolving a plan-bound credential at refresh time, and
//! [`minimax_catalog_plugin`] registers one plan's discovery into the shared
//! catalog registry. Both dialects normalize through [`normalize_model`], so a
//! documented model has one shape whichever endpoint found it.
//!
//! PMM03 adds [`MiniMaxStateRoute`], a plan/model/protocol-bound boundary for
//! MiniMax interleaved-thinking continuation. It preserves native Chat
//! `<think>` content, split Chat reasoning details, and complete Messages block
//! lists through the shared schema-v1 provider-state vocabulary.
//! [`MiniMaxInference`] reuses the strict Chat adapter and applies this boundary
//! at capture and replay, with plan-bound rotating credentials. Protocol fixture
//! success does not promote Unknown catalog capabilities to Supported.
//!
//! PMM04 adds [`MiniMaxTokenPlanMcpBundle`], a secret-free provider-owned
//! installation description for MiniMax's `uvx minimax-coding-plan-mcp -y`
//! server. The current guide documents `web_search` and `understand_image`;
//! both are prompt-gated and marked as untrusted MCP content under
//! `AllDocumented`, while `WebSearchOnly` is an explicit least-privilege
//! restriction. Concrete `heycode-mcp` registration remains composition-root work.
//!
//! PMM05's historical “Coding Plan” source now redirects to Token Plan.
//! [`MiniMaxCodingProfile`] therefore adds the current evidence-backed boundary:
//! a Subscription Key alone is insufficient, and endpoint selection requires
//! an explicitly affirmed Token Plan seat or purchased Credits. It delegates
//! to the existing Token Plan provider identity and only documented regional
//! routes. No dedicated Coding Plan URL is encoded because MiniMax publishes
//! none; that historical tracker clause remains Unknown rather than guessed.

mod catalog;
mod coding_profile;
mod credential;
mod endpoint;
mod inference;
mod mcp_bundle;
mod models;
pub use inference::MiniMaxInference;
mod plan;
mod profile;
mod state;

pub use catalog::{MiniMaxCatalog, MiniMaxCatalogConfig, minimax_catalog_plugin};
pub use coding_profile::{
    MiniMaxCodingEligibility, MiniMaxCodingProfile, MiniMaxCodingProfileError,
};
pub use credential::{MiniMaxCredential, MiniMaxCredentialError};
pub use endpoint::{
    MiniMaxApiFamily, MiniMaxAuthScheme, MiniMaxRegion, base_url, documented_base_url,
};
pub use mcp_bundle::{
    MINIMAX_TOKEN_PLAN_MCP_PLUGIN_ID, MiniMaxImageUnderstandingPolicy, MiniMaxMcpApproval,
    MiniMaxMcpBundleError, MiniMaxMcpEnvironmentValue, MiniMaxMcpExposurePolicy,
    MiniMaxMcpFeatureEvidence, MiniMaxMcpHostFailure, MiniMaxMcpInstallScope, MiniMaxMcpLaunch,
    MiniMaxMcpResourceMode, MiniMaxMcpTool, MiniMaxTokenPlanMcpBundle, MiniMaxTokenPlanMcpHost,
    minimax_token_plan_mcp_bundle_plugin,
};
pub use models::{MINIMAX_M3, documented_model_ids, normalize_model};
pub use plan::{MiniMaxPlan, MiniMaxPlanId, PayAsYouGo, TokenPlan};
pub use profile::MiniMaxProfile;
pub use state::{
    MiniMaxReasoningGuarantee, MiniMaxStateDialect, MiniMaxStateError, MiniMaxStateRoute,
    MiniMaxTurnPart,
};
