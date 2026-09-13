//! Plan-bound MiniMax model discovery over either documented list endpoint.
//!
//! MiniMax publishes the same model set twice, in two dialects:
//!
//! - `GET https://api.minimax.io/v1/models`, an OpenAI-compatible list with
//!   rows `{id, object, created, owned_by}` and no pagination
//!   (<https://platform.minimax.io/docs/api-reference/models/openai/list-models>).
//! - `GET https://api.minimax.io/anthropic/v1/models`, an Anthropic-compatible
//!   list with rows `{id, created_at, display_name, type}` plus
//!   `first_id`/`last_id`/`has_more` cursor paging
//!   (<https://platform.minimax.io/docs/api-reference/models/anthropic/list-models>).
//!
//! Both are read here and both normalize through [`crate::normalize_model`], so
//! a documented model has one shape regardless of which endpoint found it.
//! Neither endpoint publishes limits or capabilities, so identity is all that
//! comes off the wire.
//!
//! The credential is resolved at refresh time and arrives plan-bound from
//! PMM01, so a Token Plan key can never be spent against a pay-as-you-go
//! catalog or the reverse.

use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use heycode_core::{Context, CoreError, Plugin, PluginContributionKind, PluginDescriptor};
use heycode_credentials::{CredentialsService, SERVICE_CREDENTIALS};
use heycode_http::{HttpRequest, HttpResponse, HttpService, SERVICE_HTTP, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, CatalogRegistry, ModelCatalog,
    ModelDescriptor, ProviderDescriptor, SERVICE_MODELS,
};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::credential::MiniMaxCredentialError;
use crate::endpoint::MiniMaxApiFamily;
use crate::models::normalize_model;
use crate::plan::MiniMaxPlan;
use crate::profile::MiniMaxProfile;

/// Owner MiniMax reports for its own models on the OpenAI-compatible list.
///
/// Source: the documented example response carries `"owned_by": "minimax"`. A
/// different owner means the endpoint is not MiniMax's, which fails the whole
/// generation rather than importing foreign rows under MiniMax's provider id.
const MINIMAX_OWNER: &str = "minimax";

/// heycode-owned response cap. MiniMax publishes no list-size bound.
const CATALOG_RESPONSE_LIMIT: usize = 1024 * 1024;
/// heycode-owned page budget for the Anthropic-compatible cursor walk.
const MAX_PAGES: usize = 16;
/// heycode-owned generation size bound.
const MAX_MODELS: usize = 4096;

/// Configuration captured by a MiniMax catalog contribution.
#[derive(Debug, Clone)]
pub struct MiniMaxCatalogConfig<P: MiniMaxPlan> {
    profile: MiniMaxProfile<P>,
    family: MiniMaxApiFamily,
    base_url: Option<String>,
}

impl<P: MiniMaxPlan> MiniMaxCatalogConfig<P> {
    /// Discover over one documented MiniMax list endpoint.
    #[must_use]
    pub const fn new(profile: MiniMaxProfile<P>, family: MiniMaxApiFamily) -> Self {
        Self {
            profile,
            family,
            base_url: None,
        }
    }

    /// Override the base URL for a compatible proxy or test endpoint.
    ///
    /// This also bypasses the documented-route check, because the caller has
    /// supplied evidence heycode does not have.
    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = Some(base_url.into());
        self
    }
}

/// Provider-owned MiniMax model catalog source for one plan and dialect.
pub struct MiniMaxCatalog<P: MiniMaxPlan> {
    http: HttpService,
    credentials: Arc<CredentialsService>,
    profile: MiniMaxProfile<P>,
    family: MiniMaxApiFamily,
    models_url: String,
}

impl<P: MiniMaxPlan> MiniMaxCatalog<P> {
    /// Build a source from an explicit configuration.
    ///
    /// # Errors
    /// A region and dialect MiniMax does not document together is refused
    /// rather than composed into a plausible URL, and an unusable endpoint
    /// fails before the source is published.
    pub fn new(
        http: HttpService,
        credentials: Arc<CredentialsService>,
        config: MiniMaxCatalogConfig<P>,
    ) -> Result<Self, CatalogFetchError> {
        let base_url = match config.base_url {
            Some(base_url) => base_url,
            None => {
                if config.profile.documented_base_url(config.family) != CapabilitySupport::Supported
                {
                    return Err(invalid_response(
                        "MiniMax does not document this dialect for this region",
                    ));
                }
                config.profile.base_url(config.family)
            }
        };
        let models_url = format!(
            "{}{}",
            base_url.trim_end_matches('/'),
            config.family.list_models_path()
        );
        HttpRequest::get(&models_url)
            .map_err(|_| invalid_response("MiniMax catalog base URL is invalid"))?;
        Ok(Self {
            http,
            credentials,
            profile: config.profile,
            family: config.family,
            models_url,
        })
    }

    /// Dialect this source discovers over.
    #[must_use]
    pub const fn family(&self) -> MiniMaxApiFamily {
        self.family
    }

    async fn page(
        &self,
        url: &str,
        secret: &str,
        cancellation: CancellationToken,
    ) -> Result<HttpResponse, CatalogFetchError> {
        let scheme = self.family.list_models_auth();
        let request = HttpRequest::get(url)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header(scheme.header_name(), &scheme.header_value(secret)))
            .map(|request| request.with_max_response_bytes(CATALOG_RESPONSE_LIMIT))
            .map_err(|_| invalid_response("MiniMax catalog request could not be constructed"))?;
        let response = self
            .http
            .send(request, cancellation)
            .await
            .map_err(map_transport_error)?;
        classify_status(&response)?;
        require_json(&response)?;
        Ok(response)
    }

    async fn fetch_openai(
        &self,
        secret: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let response = self
            .page(&self.models_url, secret, cancellation.clone())
            .await?;
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let envelope: OpenAiModelsEnvelope = decode(&response)?;
        if envelope.object != "list" {
            return Err(invalid_response(
                "MiniMax OpenAI-compatible model list envelope is not `list`",
            ));
        }
        let mut rows = BTreeMap::new();
        for row in envelope.data {
            if row.object != "model" || row.owned_by != MINIMAX_OWNER {
                return Err(invalid_response(
                    "MiniMax OpenAI-compatible model list contains a foreign or malformed row",
                ));
            }
            insert_row(
                &mut rows,
                row.id,
                None,
                row.created
                    .and_then(|value| value.checked_mul(1_000))
                    .filter(|value| *value > 0),
            )?;
        }
        finish(rows)
    }

    async fn fetch_anthropic(
        &self,
        secret: &str,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let mut rows = BTreeMap::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let url = match cursor.as_deref() {
                // Cursors are validated model ids restricted to a URL-safe
                // charset, so they need no escaping here. MiniMax documents no
                // maximum `limit`, so none is asserted and the server's own
                // page size is used.
                Some(cursor) => format!("{}?after_id={cursor}", self.models_url),
                None => self.models_url.clone(),
            };
            let response = self.page(&url, secret, cancellation.clone()).await?;
            if cancellation.is_cancelled() {
                return Err(CatalogFetchError::cancelled());
            }
            let page: AnthropicModelsPage = decode(&response)?;
            for row in page.data {
                if row.kind != "model" || row.created_at.trim().is_empty() {
                    return Err(invalid_response(
                        "MiniMax Anthropic-compatible model list contains a malformed row",
                    ));
                }
                let created_at_ms = chrono::DateTime::parse_from_rfc3339(&row.created_at)
                    .ok()
                    .and_then(|value| u64::try_from(value.timestamp_millis()).ok())
                    .filter(|value| *value > 0);
                insert_row(&mut rows, row.id, Some(row.display_name), created_at_ms)?;
            }
            if rows.len() > MAX_MODELS {
                return Err(invalid_response("MiniMax model list is too large"));
            }
            if !page.has_more {
                return finish(rows);
            }
            // A promised further page with no usable cursor is a protocol
            // fault, not an empty result.
            let next = page.last_id.ok_or_else(|| {
                invalid_response("MiniMax model list promised a page without a cursor")
            })?;
            if cursor.as_deref() == Some(next.as_str()) {
                return Err(invalid_response(
                    "MiniMax model list repeated its pagination cursor",
                ));
            }
            cursor = Some(next);
        }
        Err(invalid_response(
            "MiniMax model list exceeded its page budget",
        ))
    }
}

/// Route identity only. The source holds no secret between refreshes — the
/// credential is resolved inside `fetch` and dropped with it.
impl<P: MiniMaxPlan> std::fmt::Debug for MiniMaxCatalog<P> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MiniMaxCatalog")
            .field("plan", &P::ID)
            .field("family", &self.family)
            .field("models_url", &self.models_url)
            .finish()
    }
}

#[async_trait]
impl<P: MiniMaxPlan> ModelCatalog for MiniMaxCatalog<P> {
    fn provider(&self) -> ProviderDescriptor {
        self.profile.provider_descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let credential = self
            .profile
            .resolve(&self.credentials)
            .map_err(unusable_credential)?;
        match self.family {
            MiniMaxApiFamily::OpenAiCompatible => {
                self.fetch_openai(credential.expose(), cancellation).await
            }
            MiniMaxApiFamily::AnthropicCompatible => {
                self.fetch_anthropic(credential.expose(), cancellation)
                    .await
            }
        }
    }
}

/// Register one plan's MiniMax discovery into the shared catalog registry.
#[must_use]
pub fn minimax_catalog_plugin<P: MiniMaxPlan>(config: MiniMaxCatalogConfig<P>) -> Box<dyn Plugin> {
    struct MiniMaxCatalogPlugin<P: MiniMaxPlan>(MiniMaxCatalogConfig<P>);

    impl<P: MiniMaxPlan> Plugin for MiniMaxCatalogPlugin<P> {
        fn name(&self) -> &'static str {
            P::CATALOG_PLUGIN_NAME
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            vec![heycode_core::PluginContributionSpec::new(
                heycode_core::ContributionKind::ModelCatalog,
                P::ID.registry_name(),
            )]
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_MODELS, SERVICE_CREDENTIALS, SERVICE_HTTP]
        }

        fn apply(&self, context: &mut Context) -> Result<(), CoreError> {
            let models = context
                .get::<CatalogRegistry>(SERVICE_MODELS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_MODELS.to_string()))?;
            let credentials = context
                .get::<CredentialsService>(SERVICE_CREDENTIALS)
                .ok_or_else(|| CoreError::MissingService(SERVICE_CREDENTIALS.to_string()))?;
            let http = context
                .get::<HttpService>(SERVICE_HTTP)
                .ok_or_else(|| CoreError::MissingService(SERVICE_HTTP.to_string()))?;
            let source = MiniMaxCatalog::new(http.as_ref().clone(), credentials, self.0.clone())
                .map_err(|error| CoreError::other(error.to_string()))?;
            models
                .register(context, Arc::new(source))
                .map_err(|error| CoreError::other(error.to_string()))
        }
    }

    Box::new(MiniMaxCatalogPlugin(config))
}

#[derive(Deserialize)]
struct OpenAiModelsEnvelope {
    object: String,
    data: Vec<OpenAiModelRow>,
}

#[derive(Deserialize)]
struct OpenAiModelRow {
    id: String,
    object: String,
    owned_by: String,
    created: Option<u64>,
}

#[derive(Deserialize)]
struct AnthropicModelsPage {
    data: Vec<AnthropicModelRow>,
    has_more: bool,
    last_id: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicModelRow {
    id: String,
    display_name: String,
    created_at: String,
    #[serde(rename = "type")]
    kind: String,
}

/// Admit one discovered row into the pending generation.
fn insert_row(
    rows: &mut BTreeMap<String, ModelDescriptor>,
    id: String,
    display_name: Option<String>,
    created_at_ms: Option<u64>,
) -> Result<(), CatalogFetchError> {
    if id.is_empty() || id.trim() != id {
        return Err(invalid_response(
            "MiniMax model list contains a blank or padded model id",
        ));
    }
    let mut model = normalize_model(id.clone(), display_name);
    model.created_at_ms = created_at_ms;
    if rows.insert(id, model).is_some() {
        return Err(invalid_response(
            "MiniMax model list contains duplicate model ids",
        ));
    }
    Ok(())
}

/// Normalize a complete generation.
fn finish(
    rows: BTreeMap<String, ModelDescriptor>,
) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
    if rows.is_empty() {
        // An authenticated list that names no model is a fault or an
        // entitlement problem; publishing it would silently empty the picker.
        return Err(invalid_response("MiniMax model list is empty"));
    }
    Ok(rows.into_values().collect())
}

fn decode<T: serde::de::DeserializeOwned>(response: &HttpResponse) -> Result<T, CatalogFetchError> {
    serde_json::from_slice(&response.body)
        .map_err(|_| invalid_response("MiniMax model list has an invalid JSON shape"))
}

fn require_json(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    if response
        .content_type
        .as_deref()
        .is_some_and(|value| value == "application/json" || value.ends_with("+json"))
    {
        Ok(())
    } else {
        Err(invalid_response("MiniMax model list response is not JSON"))
    }
}

/// Map a plan-bound credential failure onto the catalog taxonomy.
///
/// The safe text is preserved because it names *which* MiniMax product the
/// stored secret belongs to, which is the whole diagnostic value of PMM01's
/// admission rules. It provably contains no secret.
fn unusable_credential(error: MiniMaxCredentialError) -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        format!("MiniMax credential is unusable: {error}"),
    )
}

fn classify_status(response: &HttpResponse) -> Result<(), CatalogFetchError> {
    match response.status {
        200..=299 => Ok(()),
        401 | 403 => Err(unauthorized()),
        429 | 500..=599 => Err(CatalogFetchError::new(
            CatalogFailureKind::Unavailable,
            "MiniMax catalog is temporarily unavailable",
        )),
        _ => Err(invalid_response(
            "MiniMax catalog returned an unexpected HTTP status",
        )),
    }
}

fn map_transport_error(error: TransportError) -> CatalogFetchError {
    match error {
        TransportError::Cancelled => CatalogFetchError::cancelled(),
        TransportError::Http {
            status: 401 | 403, ..
        } => unauthorized(),
        TransportError::Http { status, .. } if status == 429 || status >= 500 => {
            CatalogFetchError::new(
                CatalogFailureKind::Unavailable,
                "MiniMax catalog is temporarily unavailable",
            )
        }
        TransportError::Network { .. } | TransportError::Timeout => CatalogFetchError::new(
            CatalogFailureKind::Network,
            "MiniMax catalog network request failed",
        ),
        _ => invalid_response("MiniMax catalog transport response is invalid"),
    }
}

fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "MiniMax credential is missing or unauthorized",
    )
}

fn invalid_response(message: &'static str) -> CatalogFetchError {
    CatalogFetchError::new(CatalogFailureKind::InvalidResponse, message)
}
