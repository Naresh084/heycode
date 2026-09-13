//! Effect-owned construction of exact AWS inference provider routes.
//!
//! The `*_live` constructors register a provider-owned catalog and inference
//! provider as one activation generation. Applying the plugin performs no
//! request. The async provider preparation hook forces the catalog after model
//! selection; its wait is caller-cancelled and joined, while the registry owns
//! the shared flight. Discovery and inference resolve separate operation-time
//! credentials, and adapter resolution remains locked until preparation.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use heycode_authorization_aws::AwsRegion;
use heycode_core::{
    Context, ContributionKind, CoreError, Plugin, PluginContributionKind, PluginContributionSpec,
    PluginDescriptor, ServiceKey,
};
use heycode_credentials::{CredentialQuery, CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpService, SERVICE_HTTP};
use heycode_llm::{
    CapabilitySupport, CatalogRegistry, ModelDescriptor, Provider, ProviderRegistry,
    SERVICE_MODELS, SERVICE_PROVIDERS,
};

use crate::{
    BEDROCK_PROVIDER, BedrockApiKeyAuthorizer, BedrockCatalog, BedrockConverseProvider,
    BedrockFoundationModel, BedrockRouteMetadata, BedrockRuntimeRequestMetadata, MANTLE_PROVIDER,
    MantleCatalog, MantleMessagesProvider, MantleProtocol, MantleResponsesProvider,
};

/// Exact streaming/on-demand model evidence admitted for a Converse provider.
#[derive(Debug, Clone, PartialEq)]
pub struct BedrockConverseModelEvidence {
    models: BTreeMap<String, ModelDescriptor>,
}

/// One explicit Converse callability fact. Both tri-state inputs must be
/// Supported; Unknown is not permission to construct a product route.
#[derive(Debug, Clone, PartialEq)]
pub struct BedrockConverseModelFact {
    descriptor: ModelDescriptor,
}

impl BedrockConverseModelFact {
    /// Bind one descriptor to exact streaming and on-demand evidence.
    ///
    /// # Errors
    /// Unsupported and Unknown facts fail distinctly.
    pub fn new(
        descriptor: ModelDescriptor,
        response_streaming: CapabilitySupport,
        on_demand: CapabilitySupport,
    ) -> Result<Self, AwsInferencePluginError> {
        for support in [response_streaming, on_demand] {
            match support {
                CapabilitySupport::Supported => {}
                CapabilitySupport::Unsupported => {
                    return Err(AwsInferencePluginError::UnsupportedModelEvidence);
                }
                CapabilitySupport::Unknown => {
                    return Err(AwsInferencePluginError::UnprovenModelEvidence);
                }
            }
        }
        Ok(Self { descriptor })
    }
}

impl BedrockConverseModelEvidence {
    /// Build one non-empty unique exact callability set.
    ///
    /// # Errors
    /// Empty or duplicate rows fail rather than being filtered.
    pub fn new(facts: Vec<BedrockConverseModelFact>) -> Result<Self, AwsInferencePluginError> {
        if facts.is_empty() {
            return Err(AwsInferencePluginError::EmptyModelEvidence);
        }
        let mut models = BTreeMap::new();
        for fact in facts {
            if models
                .insert(fact.descriptor.id.clone(), fact.descriptor)
                .is_some()
            {
                return Err(AwsInferencePluginError::DuplicateModelEvidence);
            }
        }
        Ok(Self { models })
    }

    /// Retain selected PAWS03 rows only when both required dispatch facts are
    /// affirmative.
    ///
    /// # Errors
    /// Empty, duplicate, non-streaming, unproven-streaming, non-on-demand or
    /// unproven-on-demand rows fail instead of being filtered or promoted.
    pub fn from_discovery(
        rows: Vec<BedrockFoundationModel>,
    ) -> Result<Self, AwsInferencePluginError> {
        let mut facts = Vec::with_capacity(rows.len());
        for row in rows {
            let response_streaming = row.response_streaming();
            let on_demand = row.on_demand();
            let descriptor = row.into_descriptor();
            facts.push(BedrockConverseModelFact::new(
                descriptor,
                response_streaming,
                on_demand,
            )?);
        }
        Self::new(facts)
    }
}

/// Exact model ids proven compatible with one Mantle protocol dialect.
#[derive(Debug, Clone, PartialEq)]
pub struct MantleProtocolModelEvidence {
    protocol: MantleProtocol,
    models: BTreeMap<String, ModelDescriptor>,
}

impl MantleProtocolModelEvidence {
    /// Bind a non-empty unique model list to one exact Mantle protocol.
    ///
    /// Membership is the caller's maintained/account evidence. Family rows AWS
    /// explicitly excludes from the protocol are rejected here; merely
    /// matching a family without membership remains unproven at dispatch.
    ///
    /// # Errors
    /// Empty, duplicate, malformed or protocol-incompatible model ids fail.
    pub fn new(
        protocol: MantleProtocol,
        models: Vec<String>,
    ) -> Result<Self, AwsInferencePluginError> {
        if models.is_empty() {
            return Err(AwsInferencePluginError::EmptyModelEvidence);
        }
        let mut seen = BTreeSet::new();
        let mut descriptors = BTreeMap::new();
        for model in models {
            if crate::BedrockModelId::new(&model).is_none()
                || protocol.model_family_support(&model) == CapabilitySupport::Unsupported
            {
                return Err(AwsInferencePluginError::UnsupportedModelEvidence);
            }
            if !seen.insert(model.clone()) {
                return Err(AwsInferencePluginError::DuplicateModelEvidence);
            }
            descriptors.insert(model.clone(), ModelDescriptor::unknown(&model));
        }
        Ok(Self {
            protocol,
            models: descriptors,
        })
    }

    fn contains(&self, model: &str) -> bool {
        self.models.contains_key(model)
    }
}

#[derive(Clone)]
enum ConverseEvidenceSource {
    Supplied(BedrockConverseModelEvidence),
    LiveCatalog,
}

#[derive(Clone)]
enum MantleEvidenceSource {
    Supplied(MantleProtocolModelEvidence),
    LiveCatalog,
}

#[derive(Clone)]
enum AwsInferenceRoute {
    Converse {
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: String,
        evidence: ConverseEvidenceSource,
        runtime_metadata: Option<BedrockRuntimeRequestMetadata>,
    },
    MantleResponses {
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: String,
        evidence: MantleEvidenceSource,
    },
    MantleMessages {
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: String,
        default_max_output_tokens: Option<u64>,
        evidence: MantleEvidenceSource,
    },
}

/// Complete explicit input for one AWS inference-provider contribution.
#[derive(Clone)]
pub struct AwsInferencePluginConfig {
    route: AwsInferenceRoute,
}

impl AwsInferencePluginConfig {
    /// Select exact Bedrock Converse with discovered model evidence and an
    /// explicit optional PAWS06 policy.
    ///
    /// # Errors
    /// Wrong credential kind, an unevidenced default or policy/model mismatch.
    pub fn converse(
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: impl Into<String>,
        evidence: BedrockConverseModelEvidence,
        runtime_metadata: Option<BedrockRuntimeRequestMetadata>,
    ) -> Result<Self, AwsInferencePluginError> {
        validate_credential(&credential)?;
        let default_model = default_model.into();
        let descriptor = evidence
            .models
            .get(&default_model)
            .ok_or(AwsInferencePluginError::DefaultModelNotEvidenced)?;
        if let Some(metadata) = &runtime_metadata {
            metadata
                .to_provider_option(&region, descriptor)
                .map_err(|_| AwsInferencePluginError::PolicyModelMismatch)?;
        }
        Ok(Self {
            route: AwsInferenceRoute::Converse {
                region,
                credential,
                default_model,
                evidence: ConverseEvidenceSource::Supplied(evidence),
                runtime_metadata,
            },
        })
    }

    /// Select a lazily evidenced Bedrock Converse route.
    ///
    /// Composition registers an inert provider-owned catalog and inference
    /// provider without I/O. A request can resolve only after that exact
    /// catalog has produced affirmative streaming and on-demand evidence for
    /// the selected model; absent, Unsupported and Unknown facts fail before
    /// inference transport.
    ///
    /// # Errors
    /// Wrong credential kind, unsafe default target or cache policy bound to a
    /// different model.
    pub fn converse_live(
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: impl Into<String>,
        runtime_metadata: Option<BedrockRuntimeRequestMetadata>,
    ) -> Result<Self, AwsInferencePluginError> {
        validate_credential(&credential)?;
        let default_model = default_model.into();
        BedrockRouteMetadata::new(&region, &default_model)
            .map_err(|_| AwsInferencePluginError::InvalidPolicy)?;
        if runtime_metadata.as_ref().is_some_and(|metadata| {
            metadata
                .prompt_cache()
                .is_some_and(|cache| cache.capabilities().model_id() != default_model)
        }) {
            return Err(AwsInferencePluginError::PolicyModelMismatch);
        }
        Ok(Self {
            route: AwsInferenceRoute::Converse {
                region,
                credential,
                default_model,
                evidence: ConverseEvidenceSource::LiveCatalog,
                runtime_metadata,
            },
        })
    }

    /// Select the exact Mantle Responses dialect.
    ///
    /// # Errors
    /// Wrong credential/protocol evidence or an unevidenced default model.
    pub fn mantle_responses(
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: impl Into<String>,
        evidence: MantleProtocolModelEvidence,
    ) -> Result<Self, AwsInferencePluginError> {
        validate_credential(&credential)?;
        let default_model = default_model.into();
        if evidence.protocol != MantleProtocol::Responses {
            return Err(AwsInferencePluginError::ProtocolEvidenceMismatch);
        }
        if !evidence.contains(&default_model) {
            return Err(AwsInferencePluginError::DefaultModelNotEvidenced);
        }
        Ok(Self {
            route: AwsInferenceRoute::MantleResponses {
                region,
                credential,
                default_model,
                evidence: MantleEvidenceSource::Supplied(evidence),
            },
        })
    }

    /// Select Mantle Responses with account-visible model membership resolved
    /// from the provider-owned `/v1/models` catalog at operation time.
    ///
    /// # Errors
    /// Wrong credential kind or an invalid/protocol-incompatible default.
    pub fn mantle_responses_live(
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: impl Into<String>,
    ) -> Result<Self, AwsInferencePluginError> {
        validate_credential(&credential)?;
        let default_model = validate_live_mantle_model(MantleProtocol::Responses, default_model)?;
        Ok(Self {
            route: AwsInferenceRoute::MantleResponses {
                region,
                credential,
                default_model,
                evidence: MantleEvidenceSource::LiveCatalog,
            },
        })
    }

    /// Select the exact Mantle Messages dialect with an explicit optional
    /// output default (`None` means every request must provide a cap).
    ///
    /// # Errors
    /// Wrong credential/protocol evidence, invalid output cap or an
    /// unevidenced default model.
    pub fn mantle_messages(
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: impl Into<String>,
        default_max_output_tokens: Option<u64>,
        evidence: MantleProtocolModelEvidence,
    ) -> Result<Self, AwsInferencePluginError> {
        if default_max_output_tokens == Some(0) {
            return Err(AwsInferencePluginError::InvalidPolicy);
        }
        validate_credential(&credential)?;
        let default_model = default_model.into();
        if evidence.protocol != MantleProtocol::Messages {
            return Err(AwsInferencePluginError::ProtocolEvidenceMismatch);
        }
        if !evidence.contains(&default_model) {
            return Err(AwsInferencePluginError::DefaultModelNotEvidenced);
        }
        Ok(Self {
            route: AwsInferenceRoute::MantleMessages {
                region,
                credential,
                default_model,
                default_max_output_tokens,
                evidence: MantleEvidenceSource::Supplied(evidence),
            },
        })
    }

    /// Select Mantle Messages with account-visible membership plus the
    /// provider-maintained exact Messages compatibility matrix resolved at
    /// operation time. A listed but unclassified Anthropic id remains
    /// unproven and fails before inference transport.
    ///
    /// # Errors
    /// Wrong credential kind, zero output default or an invalid/incompatible
    /// default model.
    pub fn mantle_messages_live(
        region: AwsRegion,
        credential: CredentialQuery,
        default_model: impl Into<String>,
        default_max_output_tokens: Option<u64>,
    ) -> Result<Self, AwsInferencePluginError> {
        if default_max_output_tokens == Some(0) {
            return Err(AwsInferencePluginError::InvalidPolicy);
        }
        validate_credential(&credential)?;
        let default_model = validate_live_mantle_model(MantleProtocol::Messages, default_model)?;
        Ok(Self {
            route: AwsInferenceRoute::MantleMessages {
                region,
                credential,
                default_model,
                default_max_output_tokens,
                evidence: MantleEvidenceSource::LiveCatalog,
            },
        })
    }
}

fn validate_live_mantle_model(
    protocol: MantleProtocol,
    model: impl Into<String>,
) -> Result<String, AwsInferencePluginError> {
    let model = model.into();
    if crate::BedrockModelId::new(&model).is_none()
        || protocol.model_family_support(&model) == CapabilitySupport::Unsupported
    {
        Err(AwsInferencePluginError::UnsupportedModelEvidence)
    } else {
        Ok(model)
    }
}

fn validate_credential(credential: &CredentialQuery) -> Result<(), AwsInferencePluginError> {
    if credential.kind.as_str() == "api-key" {
        Ok(())
    } else {
        Err(AwsInferencePluginError::CredentialKindMismatch)
    }
}

/// Stable construction failure with no credential/model value echo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AwsInferencePluginError {
    /// Route requires the Bedrock bearer API-key semantic kind.
    CredentialKindMismatch,
    /// No model evidence was supplied.
    EmptyModelEvidence,
    /// Model evidence repeats an id.
    DuplicateModelEvidence,
    /// Exact evidence explicitly denies this route.
    UnsupportedModelEvidence,
    /// Exact evidence remains Unknown.
    UnprovenModelEvidence,
    /// The configured default is absent from the exact evidence set.
    DefaultModelNotEvidenced,
    /// Mantle evidence belongs to the other dialect.
    ProtocolEvidenceMismatch,
    /// Request policy belongs to another model/route.
    PolicyModelMismatch,
    /// Explicit policy value is invalid.
    InvalidPolicy,
}

impl std::fmt::Display for AwsInferencePluginError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CredentialKindMismatch => "AWS inference credential kind is incompatible",
            Self::EmptyModelEvidence => "AWS inference model evidence is empty",
            Self::DuplicateModelEvidence => "AWS inference model evidence contains a duplicate",
            Self::UnsupportedModelEvidence => "AWS inference model evidence is unsupported",
            Self::UnprovenModelEvidence => "AWS inference model evidence is unproven",
            Self::DefaultModelNotEvidenced => "AWS inference default model is not evidenced",
            Self::ProtocolEvidenceMismatch => {
                "AWS Mantle model evidence belongs to another protocol"
            }
            Self::PolicyModelMismatch => "AWS inference policy does not match its model evidence",
            Self::InvalidPolicy => "AWS inference policy is invalid",
        })
    }
}

impl std::error::Error for AwsInferencePluginError {}

/// Register exactly one effect-owned AWS inference provider.
#[must_use]
pub fn aws_inference_plugin(config: AwsInferencePluginConfig) -> Box<dyn Plugin> {
    struct AwsInferencePlugin(AwsInferencePluginConfig);

    impl Plugin for AwsInferencePlugin {
        fn name(&self) -> &'static str {
            match &self.0.route {
                AwsInferenceRoute::Converse { .. } => "inference-bedrock-converse",
                AwsInferenceRoute::MantleResponses { .. } => "inference-bedrock-mantle-responses",
                AwsInferenceRoute::MantleMessages { .. } => "inference-bedrock-mantle-messages",
            }
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<PluginContributionSpec> {
            let provider = match &self.0.route {
                AwsInferenceRoute::Converse { .. } => BEDROCK_PROVIDER,
                AwsInferenceRoute::MantleResponses { .. }
                | AwsInferenceRoute::MantleMessages { .. } => MANTLE_PROVIDER,
            };
            let mut inventory = vec![PluginContributionSpec::new(
                ContributionKind::InferenceProvider,
                provider,
            )];
            if route_uses_live_catalog(&self.0.route) {
                inventory.push(PluginContributionSpec::new(
                    ContributionKind::ModelCatalog,
                    provider,
                ));
            }
            inventory
        }

        fn inject(&self) -> &'static [ServiceKey] {
            const BASE: &[ServiceKey] = &[SERVICE_PROVIDERS, SERVICE_HTTP, SERVICE_CREDENTIALS];
            const LIVE: &[ServiceKey] = &[
                SERVICE_PROVIDERS,
                SERVICE_MODELS,
                SERVICE_HTTP,
                SERVICE_CREDENTIALS,
            ];
            if route_uses_live_catalog(&self.0.route) {
                LIVE
            } else {
                BASE
            }
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let providers = context
                .get::<ProviderRegistry>(SERVICE_PROVIDERS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_PROVIDERS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let models = if route_uses_live_catalog(&self.0.route) {
                Some(
                    context
                        .get::<CatalogRegistry>(SERVICE_MODELS)
                        .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?,
                )
            } else {
                None
            };
            let provider: Arc<dyn Provider> = match &self.0.route {
                AwsInferenceRoute::Converse {
                    region,
                    credential,
                    default_model,
                    evidence,
                    runtime_metadata,
                } => {
                    let mut provider = BedrockConverseProvider::from_credentials(
                        http.as_ref().clone(),
                        credentials.as_ref(),
                        credential,
                        region,
                        default_model.clone(),
                    )
                    .map_err(|_| CoreError::other("Bedrock Converse provider is invalid"))?;
                    if let Some(metadata) = runtime_metadata {
                        provider = provider.with_runtime_metadata(metadata.clone());
                    }
                    Arc::new(match evidence {
                        ConverseEvidenceSource::Supplied(evidence) => {
                            provider.with_model_evidence(evidence.models.clone())
                        }
                        ConverseEvidenceSource::LiveCatalog => {
                            let models = models.as_ref().ok_or_else(|| {
                                CoreError::MissingService(SERVICE_MODELS.to_string())
                            })?;
                            let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(
                                credentials.clone(),
                                credential.clone(),
                            ));
                            let source =
                                BedrockCatalog::new(http.as_ref().clone(), authorizer, region);
                            let live = source.evidence();
                            models
                                .register(context, Arc::new(source))
                                .map_err(|error| CoreError::other(error.to_string()))?;
                            provider.with_live_model_evidence(live, models.as_ref().clone())
                        }
                    })
                }
                AwsInferenceRoute::MantleResponses {
                    region,
                    credential,
                    default_model,
                    evidence,
                } => {
                    let provider = MantleResponsesProvider::from_credentials(
                        http.as_ref().clone(),
                        credentials.as_ref(),
                        credential,
                        region,
                        default_model.clone(),
                    )
                    .map_err(|_| CoreError::other("Mantle Responses provider is invalid"))?;
                    Arc::new(match evidence {
                        MantleEvidenceSource::Supplied(evidence) => {
                            provider.with_model_evidence(evidence.models.clone())
                        }
                        MantleEvidenceSource::LiveCatalog => {
                            let models = models.as_ref().ok_or_else(|| {
                                CoreError::MissingService(SERVICE_MODELS.to_string())
                            })?;
                            let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(
                                credentials.clone(),
                                credential.clone(),
                            ));
                            let source =
                                MantleCatalog::new(http.as_ref().clone(), authorizer, region);
                            let live = source.evidence();
                            models
                                .register(context, Arc::new(source))
                                .map_err(|error| CoreError::other(error.to_string()))?;
                            provider.with_catalog_evidence(models.as_ref().clone(), live)
                        }
                    })
                }
                AwsInferenceRoute::MantleMessages {
                    region,
                    credential,
                    default_model,
                    default_max_output_tokens,
                    evidence,
                } => {
                    let provider = MantleMessagesProvider::from_credentials(
                        http.as_ref().clone(),
                        credentials.as_ref(),
                        credential,
                        region,
                        default_model.clone(),
                        *default_max_output_tokens,
                    )
                    .map_err(|_| CoreError::other("Mantle Messages provider is invalid"))?;
                    Arc::new(match evidence {
                        MantleEvidenceSource::Supplied(evidence) => {
                            provider.with_model_evidence(evidence.models.clone())
                        }
                        MantleEvidenceSource::LiveCatalog => {
                            let models = models.as_ref().ok_or_else(|| {
                                CoreError::MissingService(SERVICE_MODELS.to_string())
                            })?;
                            let authorizer = Arc::new(BedrockApiKeyAuthorizer::new(
                                credentials.clone(),
                                credential.clone(),
                            ));
                            let source =
                                MantleCatalog::new(http.as_ref().clone(), authorizer, region);
                            let live = source.evidence();
                            models
                                .register(context, Arc::new(source))
                                .map_err(|error| CoreError::other(error.to_string()))?;
                            provider.with_catalog_evidence(models.as_ref().clone(), live)
                        }
                    })
                }
            };
            let registration = providers
                .register_owned(provider)
                .map_err(CoreError::DuplicatePlugin)?;
            context.effect(move || drop(registration));
            Ok(())
        }
    }

    Box::new(AwsInferencePlugin(config))
}

fn route_uses_live_catalog(route: &AwsInferenceRoute) -> bool {
    match route {
        AwsInferenceRoute::Converse { evidence, .. } => {
            matches!(evidence, ConverseEvidenceSource::LiveCatalog)
        }
        AwsInferenceRoute::MantleResponses { evidence, .. }
        | AwsInferenceRoute::MantleMessages { evidence, .. } => {
            matches!(evidence, MantleEvidenceSource::LiveCatalog)
        }
    }
}
