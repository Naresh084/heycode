//! LM Studio endpoint, auth and health detection.
//!
//! PLM01 answers three questions about a local LM Studio install without
//! assuming any of them: is the server there, which published release is it at
//! least, and which protocol families does it actually serve. Every answer is
//! evidence-backed. A surface that was not observed stays `Unknown`, and
//! `Unknown` is never promoted to `Supported` — "OpenAI-compatible" proves only
//! that one endpoint resembles one request family (GOTCHAS #22).
//!
//! Three facts about LM Studio shape this crate:
//!
//! - It publishes **no** version, health or status endpoint
//!   (<https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/920>), so a
//!   version is reported only as a lower bound derived from the API generation
//!   that answered, never as an exact release.
//! - Its HTTP server answers `200 OK` on unknown paths
//!   (<https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1323>), so a
//!   status code alone proves nothing; every probe validates the documented
//!   response body shape.
//! - Every inference route is `POST`-only, and probing one would load a model,
//!   so detection observes each surface's model-list route instead. Protocols
//!   with no observable surface stay `Unknown`.
//!
//! PLM04 adds [`LmStudioModelControl`]. Preparing a load is pure; only consuming
//! a non-Clone [`LmStudioLoadPlan`] may call the documented native v1 load
//! endpoint. Every user-specified context/hardware value is requested with
//! `echo_load_config: true` and verified before publication. Unload consumes an
//! exact instance id. [`LmStudioModelControl::require_loaded`] is the
//! no-surprise route gate: downloaded-only weights are refused rather than JIT
//! loaded behind a model selection. The existing provider plugin publishes the
//! client under [`SERVICE_LM_STUDIO_MODEL_CONTROL`]. Default
//! [`lmstudio_control_plugin`] adds the live Settings namespace and queued
//! `/lmstudio` Consumer; success requires exact provider readback plus a shared
//! catalog refresh.
//!
//! PLM05 keeps Ollama a sibling, never an alias for LM Studio. Its native
//! `/api/version`, `/api/tags`, `/api/ps` and read-only `/api/show` surfaces
//! establish product identity, downloaded/loaded state and exact model
//! capabilities. The separately documented OpenAI-compatible `/v1/models`
//! surface establishes picker agreement for an explicit [`OllamaProfile`].
//! [`OllamaInference`] makes that profile constructible through the shared Chat
//! adapter, while native-only catalog reads still advertise protocol Unknown.
//! [`OllamaInspector`] is deliberately read-only and its receipt can never
//! claim a live inference smoke passed. [`ollama_plugin`] owns the complete
//! crate-local services and catalog effect without claiming root registration.

mod catalog;
mod config;
mod control_plugin;
mod detect;
mod endpoint;
mod inference;
mod model_control;
pub use inference::LmStudioInference;
mod ollama;
mod plugin;
mod report;
mod route;

pub use catalog::{
    LmStudioCatalog, LmStudioLoadedInstance, LmStudioModelKind, LmStudioModelRecord,
    LmStudioModelState, LmStudioQuantization, SERVICE_LM_STUDIO_MODELS, lmstudio_catalog_plugin,
};
pub use config::{LM_STUDIO_MAX_TIMEOUT, LmStudioConfig, LmStudioConfigError};
pub use control_plugin::{
    LM_STUDIO_CONTROL_SETTINGS_NAMESPACE, LmStudioControlOperations, LmStudioControlPreferences,
    LmStudioProductControlError, LmStudioProductControlReceipt, lmstudio_control_plugin,
    lmstudio_control_settings_definition,
};
pub use detect::{LM_STUDIO_DEFAULT_CATALOG_TIMEOUT, LM_STUDIO_DEFAULT_TIMEOUT, LmStudioDetector};
pub use endpoint::{
    LM_STUDIO_DEFAULT_BASE_URL, LM_STUDIO_DISPLAY_NAME, LM_STUDIO_PROVIDER, LmStudioAuth,
    LmStudioEndpoint, LmStudioSurface,
};
pub use model_control::{
    LmStudioControlError, LmStudioLoadPlan, LmStudioLoadReceipt, LmStudioLoadSettings,
    LmStudioModelControl, LmStudioUnloadPlan, LmStudioUnloadReceipt,
};
pub use ollama::{
    OLLAMA_DEFAULT_BASE_URL, OLLAMA_DISPLAY_NAME, OLLAMA_PROVIDER, OllamaCatalog, OllamaEndpoint,
    OllamaInference, OllamaInferenceError, OllamaInspector, OllamaModelDetails, OllamaModelRecord,
    OllamaPickerModel, OllamaProfile, OllamaReadinessError, OllamaRunningModel,
    OllamaSmokeReadiness, SERVICE_OLLAMA_CATALOG, SERVICE_OLLAMA_INFERENCE,
    SERVICE_OLLAMA_INSPECTOR, SERVICE_OLLAMA_PROFILE, ollama_catalog_plugin, ollama_plugin,
    ollama_plugin_with_credential,
};
pub use plugin::{SERVICE_LM_STUDIO, SERVICE_LM_STUDIO_MODEL_CONTROL, lmstudio_plugin};
pub use route::{
    LmStudioAgentEligibility, LmStudioAgentRefusal, LmStudioRouteError, agent_capable_models,
    validate_agent_route,
};

pub use report::{
    LM_STUDIO_NATIVE_REST_V1_RELEASE, LmStudioCredentialState, LmStudioHealth, LmStudioProbe,
    LmStudioProtocolDetection, LmStudioRestApi, LmStudioServerReport, LmStudioSurfaceObservation,
    LmStudioVersion,
};

/// Local server connections without fabricated model defaults.
#[must_use]
pub fn local_connection_profiles() -> Vec<heycode_llm::ConnectionProfile> {
    use heycode_llm::{ConnectionFamily, ConnectionProfile, ProviderDescriptor};
    [
        (LM_STUDIO_PROVIDER, LM_STUDIO_DISPLAY_NAME, LM_STUDIO_DEFAULT_BASE_URL, "Start the server in LM Studio's Developer tab and load a tool-capable model, then retry."),
        (OLLAMA_PROVIDER, OLLAMA_DISPLAY_NAME, OLLAMA_DEFAULT_BASE_URL, "Start Ollama with `ollama serve` and download a model with `ollama pull <model>`, then retry."),
    ].into_iter().map(|(id, name, endpoint, help)| ConnectionProfile {
        registry_name: id.into(), descriptor: ProviderDescriptor { id: id.into(), display_name: name.into(), protocols: vec![heycode_core::ProviderProtocol::OpenAiChatCompletions] },
        default_model: None, credential_reference: None, family: ConnectionFamily::Local, default_endpoint: Some(endpoint.into()), help: Some(help.into()), selectable_models: None, parameters: Vec::new(), model_selection: heycode_llm::ConnectionModelSelection::ToolCapableCatalog,
    }).collect()
}
