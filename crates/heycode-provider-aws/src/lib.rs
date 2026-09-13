//! AWS Bedrock catalog and provider profiles.
//!
//! PAWS03 owns runtime model discovery: one `ListFoundationModels` call
//! against the regional Amazon Bedrock control plane, normalized into one
//! all-or-nothing CAT02 catalog generation.
//!
//! What "normalized" means here is deliberately narrow. The shared
//! [`ModelDescriptor`](heycode_llm::ModelDescriptor) vocabulary can carry a
//! model's identity, its lifecycle and its capability evidence, but it has no
//! field for an output modality, for streaming support, or for the inference
//! types an account may call a model through — and those decide whether a
//! model is usable at all. Discovery therefore yields
//! [`BedrockFoundationModel`], which holds the descriptor *and* the Bedrock
//! facts CAT02 cannot name; [`ModelCatalog::fetch`](heycode_llm::ModelCatalog)
//! hands the registry the descriptor half.
//!
//! Two rules run through every path:
//!
//! * **Unknown is never promoted.** An absent `responseStreamingSupported` is
//!   [`CapabilitySupport::Unknown`](heycode_llm::CapabilitySupport), an
//!   unpublished modality list is `None` rather than an empty one, and a
//!   lifecycle phase outside the documented enumeration is Unknown rather than
//!   active.
//! * **No signing happens here.** Discovery builds an unauthorized request and
//!   hands it to a [`BedrockRequestAuthorizer`]; SigV4 belongs to PAWS01 and
//!   plugs in as one implementation of that trait, documented on that trait.
//!
//! Every AWS wire fact encoded in this crate cites the documentation page it
//! came from next to the constant or type it describes.

mod catalog;
mod converse;
mod discovery;
mod inference_plugin;
mod mantle;
mod mantle_inference;
mod model;
mod runtime_metadata;
mod settings;

pub use catalog::{
    BEDROCK_DISPLAY_NAME, BEDROCK_PROVIDER, BedrockApiKeyAuthorizer, BedrockCatalog,
    BedrockCatalogConfig, BedrockRequestAuthorizer, bedrock_catalog_plugin,
    bedrock_connection_profile, provider_descriptor,
};
pub use converse::{
    BedrockConverseProvider, bedrock_converse_profile, converse_stream_eligible, runtime_url,
};
pub use inference_plugin::{
    AwsInferencePluginConfig, AwsInferencePluginError, BedrockConverseModelEvidence,
    BedrockConverseModelFact, MantleProtocolModelEvidence, aws_inference_plugin,
};
pub use mantle::{
    MANTLE_DISPLAY_NAME, MANTLE_PROVIDER, MantleCatalog, MantleCatalogConfig,
    mantle_catalog_plugin, mantle_provider_descriptor,
};
pub use mantle_inference::{
    MantleInferenceProfile, MantleMessagesProvider, MantleProfileCapabilities, MantleProtocol,
    MantleResponsesProvider, mantle_messages_base_url, mantle_messages_profile,
    mantle_responses_base_url, mantle_responses_profile,
};
pub use model::{
    BedrockFoundationModel, BedrockInferenceType, BedrockLifecycle, BedrockLifecycleStatus,
    BedrockModality, BedrockModelId,
};
pub use runtime_metadata::{
    BedrockCachePlacement, BedrockCachePoint, BedrockCacheTtl, BedrockCrossRegionScope,
    BedrockGuardrailConfig, BedrockGuardrailStreamMode, BedrockGuardrailTrace,
    BedrockInferenceTargetKind, BedrockMetadataError, BedrockMetadataErrorClass,
    BedrockPromptCacheCapabilities, BedrockPromptCacheConfig, BedrockRouteMetadata,
    BedrockRuntimeRequestMetadata,
};
pub use settings::{
    AWS_BEDROCK_SETTINGS_NAMESPACE, AwsBedrockSettings, AwsBedrockSettingsError,
    aws_bedrock_settings_definition, aws_bedrock_settings_namespace, aws_bedrock_settings_plugin,
    aws_converse_config_from_settings, aws_converse_live_config_from_settings,
    aws_converse_live_settings_plugin, resolve_aws_bedrock_settings,
};
