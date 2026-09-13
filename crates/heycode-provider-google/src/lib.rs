//! Google Gemini provider profile and accessible-model catalog.
//!
//! PGCP02 owns the discovery half of the Google provider: the safe identity
//! and default a setup or routing surface needs, and the live catalog of the
//! models a given credential can actually call, normalized into heycode's
//! existing model vocabulary.
//!
//! # Accessible, not published
//!
//! The catalog answers "what can this credential call?", not "what has Google
//! announced?". Two rules follow from that and are enforced in
//! [`GeminiCatalog`]:
//!
//! - A row that does not advertise the `generateContent` method is not
//!   published. It is a real model the credential may hold — an embedding,
//!   speech or tuning-only model — but it is not one a heycode inference request
//!   can call.
//! - A capability the endpoint does not publish stays
//!   [`heycode_llm::CapabilitySupport::Unknown`]. The `Model` resource carries
//!   exactly one capability field, `thinking`, so reasoning is the only
//!   capability with evidence; tools, image and document input, structured
//!   output, provider-hosted web, native compaction and prompt caching are all
//!   Unknown, and no documentation table is transcribed to fill them in.
//!
//! # The two Google surfaces
//!
//! Google serves Gemini through two different APIs, and this crate is explicit
//! about which one it describes.
//!
//! | | Gemini Developer API | Vertex AI |
//! |---|---|---|
//! | Host | `generativelanguage.googleapis.com` | `{location}-aiplatform.googleapis.com` |
//! | Base-model list | `GET /v1/models` | `GET /v1beta1/publishers/google/models` |
//! | Response key | `models` | `publisherModels` |
//! | Token limits | `inputTokenLimit`, `outputTokenLimit` | none |
//! | Capability field | `thinking` | none |
//! | Method list | `supportedGenerationMethods` | none |
//! | Auth | API key, or OAuth plus a quota project | OAuth (`cloud-platform` scope) |
//!
//! The Developer API remains the only live accessible-model catalog in this
//! crate. Vertex's `ListPublisherModels` is a Model Garden directory whose
//! `PublisherModel` resource carries no per-project entitlement evidence. It
//! is therefore never presented as account discovery. PGCP05–07 instead add
//! two explicit credential-blind maintained sources: the current Vertex model
//! card for Gemini 3.7 Flash and the current Google Cloud card for Claude
//! Sonnet 5. Each returns one exact row, publishes no user assertion, performs
//! no network request, and reports account access separately as Unknown.
//!
//! PGCP05 also owns one Vertex-only request boundary under the distinct
//! provider id `vertex-google`: `retrieval.externalApi` grounding. The strict
//! [`GoogleGeminiProvider`] can consume it for an explicitly evidenced model,
//! and [`google_inference_plugin`] owns its exact N01 candidate when configured.
//! Root does not compose this plugin yet, so lower construction cannot be
//! mistaken for product reachability.
//!
//! PGCP01 is still load-bearing here: the Developer API's documented OAuth
//! mode requires the caller to name a billing/quota project, and
//! [`GoogleCatalogConfig::oauth_quota_project`] takes that project from
//! `heycode-authorization-gcp` rather than re-deriving it. Its three project
//! states stay distinct — an unset or malformed project is a determinate
//! authorization failure, while an undetermined one is reported as
//! unavailable and never as a negative finding.
//!
//! # Secrets
//!
//! An API key or access token is resolved per refresh and travels only in a
//! request header; nothing in this crate places a credential in a URL, and no
//! error message or `Debug` rendering carries a credential or a provider
//! response body.
//!
//! # Sources
//!
//! - Gemini Developer API discovery document (field names, paths, parameters):
//!   <https://generativelanguage.googleapis.com/$discovery/rest?version=v1>
//! - `models.list` reference: <https://ai.google.dev/api/models#method:-models.list>
//! - Model variants and lifecycle prose: <https://ai.google.dev/gemini-api/docs/models>
//! - API-key header and environment variables:
//!   <https://ai.google.dev/gemini-api/docs/api-key>
//! - OAuth/ADC bearer plus `x-goog-user-project`:
//!   <https://ai.google.dev/gemini-api/docs/oauth>
//! - Vertex AI discovery document (`ListPublisherModels`, `PublisherModel`):
//!   <https://aiplatform.googleapis.com/$discovery/rest?version=v1beta1>

mod caching;
mod catalog;
mod claude_vertex;
mod execution;
mod external_grounding;
mod grounding;
mod inference;
mod inference_plugin;
mod key_validation;
mod maintained_catalog;
mod settings;
mod vertex_catalog;

pub use caching::{
    CacheMetadataError, CacheTokenDetail, GOOGLE_CONTEXT_CACHE_OPTION_KIND, GeminiCacheMode,
    GeminiCacheRequest, GeminiCacheUsage, GeminiModality,
};
pub use catalog::{
    GOOGLE_GEMINI_3_7_FLASH, GeminiCatalog, GoogleCatalogConfig, google_catalog_plugin,
};
pub use claude_vertex::{
    CLAUDE_VERTEX_ANTHROPIC_VERSION, CLAUDE_VERTEX_DEFAULT_MODEL, CLAUDE_VERTEX_OAUTH_SCOPE,
    CLAUDE_VERTEX_PROVIDER, ClaudeVertexControls, ClaudeVertexEffort, ClaudeVertexError,
    ClaudeVertexLiveEvidence, ClaudeVertexProfile, ClaudeVertexThinking,
};
pub use execution::{
    CodeExecutionError, CodeExecutionProjector, CodeExecutionRequest,
    GOOGLE_CODE_EXECUTION_IMPLEMENTATION, GOOGLE_CODE_EXECUTION_OPTION_KIND,
    GOOGLE_CODE_EXECUTION_TOOL_NAME, GOOGLE_NATIVE_CODE_EXECUTION_LOGICAL,
    google_code_execution_native_tools_plugin,
};
pub use external_grounding::{
    ExternalApiAuth, ExternalApiKeyLocation, ExternalElasticSearchSpec, ExternalGroundingError,
    ExternalGroundingRequest, ExternalGroundingSpec, GOOGLE_EXTERNAL_GROUNDING_LOGICAL,
    GOOGLE_EXTERNAL_GROUNDING_OPTION_KIND, GOOGLE_EXTERNAL_GROUNDING_TOOL_NAME,
    GOOGLE_VERTEX_PROVIDER,
};
pub use grounding::{
    GOOGLE_SEARCH_IMPLEMENTATION, GOOGLE_SEARCH_OPTION_KIND, GOOGLE_SEARCH_TOOL_NAME,
    GOOGLE_WEB_SEARCH_LOGICAL, GoogleSearchRequest, GoogleSearchTypes, GroundingError,
    GroundingProjector, GroundingStatus, ProjectedGrounding, google_search_native_tools_plugin,
    reanchor,
};
pub use inference::{
    ClaudeVertexProvider, GOOGLE_EXTERNAL_GROUNDING_IMPLEMENTATION, GoogleGeminiProvider,
};
pub use inference_plugin::{
    GoogleGeminiModelEvidence, GoogleGeminiPolicy, GoogleInferencePluginConfig,
    GoogleInferencePluginError, google_inference_plugin,
};
pub use key_validation::GoogleApiKeyValidator;
pub use maintained_catalog::{
    GOOGLE_CLAUDE_VERTEX_MODEL_SOURCE, GOOGLE_VERTEX_MODEL_SOURCE, MaintainedVertexCatalog,
    maintained_claude_vertex_catalog_plugin, maintained_vertex_gemini_catalog_plugin,
};
pub use settings::{
    GOOGLE_INFERENCE_SETTINGS_NAMESPACE, GoogleInferenceSettings, GoogleInferenceSettingsError,
    google_developer_config_from_settings, google_developer_settings_plugin,
    google_inference_settings_definition, google_inference_settings_namespace,
    google_inference_settings_plugin, google_lazy_vertex_config_from_settings,
    google_lazy_vertex_settings_plugin, google_vertex_config_from_settings,
    resolve_google_inference_settings,
};
pub use vertex_catalog::{
    VertexGeminiCatalog, VertexGeminiCatalogConfig, vertex_gemini_catalog_plugin,
};

/// Provider-owned non-secret credential reference for the Gemini Developer
/// API.
///
/// The documented environment variables are `GEMINI_API_KEY` and
/// `GOOGLE_API_KEY` (<https://ai.google.dev/gemini-api/docs/api-key>); the
/// Gemini-specific name is the provider-owned reference because
/// `GOOGLE_API_KEY` is shared with every other Google API. The OAuth mode
/// presents an access token through its own credential query instead, so it
/// does not use this reference.
pub const GOOGLE_API_KEY_REFERENCE: &str = "GEMINI_API_KEY";

/// Provider-owned non-secret reference for a Google Cloud OAuth access token.
///
/// PGCP01 inspects ADC without minting a token. A composition owner that makes
/// a Vertex inference route reachable binds this reference to an operation-time
/// provider such as a trusted command helper; it never copies ADC file values
/// into settings or this crate.
pub const GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE: &str = "gcp:cloud-platform-access-token";

/// Safe Google identity/default metadata shared by setup and routing.
#[must_use]
pub fn google_profile() -> heycode_llm::ProviderProfile {
    heycode_llm::ProviderProfile {
        registry_name: catalog::GOOGLE_PROVIDER.to_owned(),
        descriptor: catalog::provider_descriptor(),
        default_model: GOOGLE_GEMINI_3_7_FLASH.to_owned(),
        credential_reference: Some(GOOGLE_API_KEY_REFERENCE.to_owned()),
    }
}

/// Gemini connection choices limited to the production adapter's maintained model.
#[must_use]
pub fn google_connection_profile() -> heycode_llm::ConnectionProfile {
    heycode_llm::ConnectionProfile {
        selectable_models: Some(vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()]),
        ..google_profile().into()
    }
}

/// Vertex Gemini connection metadata for the externally provisioned ADC flow.
#[must_use]
pub fn vertex_google_connection_profile() -> heycode_llm::ConnectionProfile {
    heycode_llm::ConnectionProfile {
        registry_name: GOOGLE_VERTEX_PROVIDER.to_owned(),
        descriptor: maintained_catalog::vertex_gemini_provider_descriptor(),
        default_model: Some(GOOGLE_GEMINI_3_7_FLASH.to_owned()),
        credential_reference: Some(GOOGLE_CLOUD_ACCESS_TOKEN_REFERENCE.to_owned()),
        family: heycode_llm::ConnectionFamily::Cloud,
        default_endpoint: None,
        help: Some(
            "Configure Application Default Credentials, project billing, the Vertex AI API, and the Vertex AI User role outside heycode. Supply a current cloud-platform OAuth token through the configured credential source; heycode inspects ADC but does not mint tokens."
                .to_owned(),
        ),
        selectable_models: Some(vec![GOOGLE_GEMINI_3_7_FLASH.to_owned()]),
        parameters: vec![
            heycode_llm::ConnectionParameter {
                id: "project".to_owned(),
                label: "Google Cloud project".to_owned(),
                description: "Project id or numeric project number used by Vertex AI".to_owned(),
            },
            heycode_llm::ConnectionParameter {
                id: "location".to_owned(),
                label: "Vertex AI location".to_owned(),
                description: "Regional Vertex AI location or global".to_owned(),
            },
        ],
        model_selection: heycode_llm::ConnectionModelSelection::Catalog,
    }
}
