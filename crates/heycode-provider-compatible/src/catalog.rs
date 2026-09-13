use crate::{CatalogDialect, CompatibleSpec};
use async_trait::async_trait;
use heycode_http::{HttpRequest, HttpService, TransportError};
use heycode_llm::{
    CapabilitySupport, CatalogFailureKind, CatalogFetchError, ModelCatalog, ModelDescriptor,
    ProviderDescriptor, RouteCredential,
};
use serde::Deserialize;
use std::collections::BTreeSet;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

const MAX_PAGES: usize = 20;
const MAX_MODELS: usize = 4096;
const MAX_BYTES: usize = 4 * 1024 * 1024;

/// Bounded provider model discovery through the composed HTTP and credential services.
pub struct CompatibleCatalog {
    spec: CompatibleSpec,
    http: HttpService,
    models_url: String,
    credential: RouteCredential,
}

impl CompatibleCatalog {
    /// Bind an exact catalog endpoint; credentials remain references until fetch.
    ///
    /// # Errors
    /// Invalid HTTP(S) endpoints fail before registration.
    pub fn new(
        spec: CompatibleSpec,
        http: HttpService,
        models_url: impl Into<String>,
        credential: RouteCredential,
    ) -> Result<Self, CatalogFetchError> {
        let models_url = models_url.into();
        HttpRequest::get(&models_url).map_err(|_| invalid())?;
        Ok(Self {
            spec,
            http,
            models_url,
            credential,
        })
    }

    async fn fetch_pages(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        let secret = self.credential.acquire().map_err(|_| unauthorized())?;
        let mut models = Vec::new();
        let mut ids = BTreeSet::new();
        let mut tokens = BTreeSet::new();
        let mut token = None;
        for _ in 0..MAX_PAGES {
            if cancellation.is_cancelled() {
                return Err(CatalogFetchError::cancelled());
            }
            let mut url = url::Url::parse(&self.models_url).map_err(|_| invalid())?;
            if self.spec.catalog_dialect == CatalogDialect::Fireworks {
                url.query_pairs_mut().append_pair("pageSize", "200");
                if let Some(token) = token.as_deref() {
                    url.query_pairs_mut().append_pair("pageToken", token);
                }
            }
            let request = HttpRequest::get(url.as_str())
                .and_then(|request| {
                    request.header("authorization", &format!("Bearer {}", secret.expose()))
                })
                .and_then(|request| request.header("accept", "application/json"))
                .map_err(|_| invalid())?
                .with_max_response_bytes(MAX_BYTES);
            let response = self
                .http
                .send(request, cancellation.clone())
                .await
                .map_err(|error| match error {
                    TransportError::Cancelled => CatalogFetchError::cancelled(),
                    _ => CatalogFetchError::new(
                        CatalogFailureKind::Network,
                        "model catalog request failed",
                    ),
                })?;
            match response.status {
                200 => {}
                401 | 403 => return Err(unauthorized()),
                _ => {
                    return Err(CatalogFetchError::new(
                        CatalogFailureKind::Unavailable,
                        "model catalog is unavailable",
                    ));
                }
            }
            if response.body.len() > MAX_BYTES
                || !response
                    .content_type
                    .as_deref()
                    .is_some_and(json_content_type)
            {
                return Err(invalid());
            }
            let (page, next) = match self.spec.catalog_dialect {
                CatalogDialect::Together => {
                    let rows: Vec<TogetherRow> =
                        serde_json::from_slice(&response.body).map_err(|_| invalid())?;
                    if rows.len() > MAX_MODELS {
                        return Err(invalid());
                    }
                    let mut page = Vec::new();
                    for row in rows {
                        validate_id(&row.id)?;
                        if !ids.insert(row.id.clone()) {
                            return Err(invalid());
                        }
                        if row.r#type.as_deref() != Some("chat") {
                            continue;
                        }
                        let mut model = documented_model(self.spec, &row.id);
                        model.context_window = positive(row.context_length)?;
                        page.push(model);
                    }
                    (page, None)
                }
                CatalogDialect::Xai => {
                    let envelope: XaiEnvelope =
                        serde_json::from_slice(&response.body).map_err(|_| invalid())?;
                    if envelope.models.len() > MAX_MODELS {
                        return Err(invalid());
                    }
                    let mut page = Vec::new();
                    for row in envelope.models {
                        validate_id(&row.id)?;
                        if !ids.insert(row.id.clone()) {
                            return Err(invalid());
                        }
                        let mut model = documented_model(self.spec, &row.id);
                        model.capabilities.image_input =
                            support(row.input_modalities.as_ref().map(|modalities| {
                                modalities.iter().any(|modality| modality == "image")
                            }));
                        page.push(model);
                    }
                    (page, None)
                }
                CatalogDialect::Mistral => {
                    let envelope: MistralEnvelope =
                        serde_json::from_slice(&response.body).map_err(|_| invalid())?;
                    if envelope.data.len() > MAX_MODELS {
                        return Err(invalid());
                    }
                    let mut page = Vec::new();
                    for row in envelope.data {
                        validate_id(&row.id)?;
                        if !ids.insert(row.id.clone()) {
                            return Err(invalid());
                        }
                        let Some(capabilities) = row.capabilities else {
                            continue;
                        };
                        if capabilities.completion_chat != Some(true) || row.archived == Some(true)
                        {
                            continue;
                        }
                        let mut model = ModelDescriptor::unknown(row.id);
                        model.context_window = positive(row.max_context_length)?;
                        model.capabilities.tools = support(capabilities.function_calling);
                        model.capabilities.image_input = support(capabilities.vision);
                        page.push(model);
                    }
                    (page, None)
                }
                CatalogDialect::Groq => {
                    let envelope: GroqEnvelope =
                        serde_json::from_slice(&response.body).map_err(|_| invalid())?;
                    if envelope.data.len() > MAX_MODELS {
                        return Err(invalid());
                    }
                    let mut page = Vec::new();
                    for row in envelope.data {
                        validate_id(&row.id)?;
                        if !ids.insert(row.id.clone()) {
                            return Err(invalid());
                        }
                        if row.active == Some(false) {
                            continue;
                        }
                        let mut model = documented_model(self.spec, &row.id);
                        model.context_window = positive(row.context_window)?;
                        page.push(model);
                    }
                    (page, None)
                }
                CatalogDialect::Fireworks => {
                    let envelope: FireworksEnvelope =
                        serde_json::from_slice(&response.body).map_err(|_| invalid())?;
                    if envelope.models.len() > 200 {
                        return Err(invalid());
                    }
                    let mut page = Vec::new();
                    for row in envelope.models {
                        validate_id(&row.name)?;
                        if !row.name.starts_with("accounts/") || !ids.insert(row.name.clone()) {
                            return Err(invalid());
                        }
                        if row.supports_serverless != Some(true)
                            || row.state.as_deref() != Some("READY")
                            || row.conversation_config.is_none()
                        {
                            continue;
                        }
                        let mut model = ModelDescriptor::unknown(row.name);
                        if let Some(name) = row.display_name.filter(|name| !name.is_empty()) {
                            validate_id(&name)?;
                            model.display_name = name;
                        }
                        model.context_window = positive(row.context_length)?;
                        if let Some(date) = row.deprecation_date {
                            model.lifecycle = date.lifecycle()?;
                        }
                        model.capabilities.tools = support(row.supports_tools);
                        model.capabilities.image_input = support(row.supports_image_input);
                        page.push(model);
                    }
                    (
                        page,
                        envelope.next_page_token.filter(|token| !token.is_empty()),
                    )
                }
            };
            models.extend(page);
            if ids.len() > MAX_MODELS {
                return Err(invalid());
            }
            let Some(next) = next else {
                return Ok(models);
            };
            if next.len() > 2048
                || next.chars().any(char::is_control)
                || !tokens.insert(next.clone())
            {
                return Err(invalid());
            }
            token = Some(next);
        }
        Err(invalid())
    }
}

#[async_trait]
impl ModelCatalog for CompatibleCatalog {
    fn provider(&self) -> ProviderDescriptor {
        self.spec.descriptor()
    }
    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        let cancellation = cancellation.child_token();
        let _guard = cancellation.clone().drop_guard();
        tokio::time::timeout(Duration::from_secs(20), self.fetch_pages(cancellation))
            .await
            .map_err(|_| {
                CatalogFetchError::new(
                    CatalogFailureKind::Network,
                    "model catalog request timed out",
                )
            })?
    }
}

fn invalid() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::InvalidResponse,
        "compatible model catalog is invalid",
    )
}
fn unauthorized() -> CatalogFetchError {
    CatalogFetchError::new(
        CatalogFailureKind::Unauthorized,
        "model catalog credential is unavailable or rejected",
    )
}
fn validate_id(id: &str) -> Result<(), CatalogFetchError> {
    if id.is_empty() || id.trim() != id || id.len() > 256 || id.chars().any(char::is_control) {
        return Err(invalid());
    }
    Ok(())
}
fn positive(value: Option<u64>) -> Result<Option<u64>, CatalogFetchError> {
    if value == Some(0) {
        Err(invalid())
    } else {
        Ok(value)
    }
}
fn support(value: Option<bool>) -> CapabilitySupport {
    match value {
        Some(true) => CapabilitySupport::Supported,
        Some(false) => CapabilitySupport::Unsupported,
        None => CapabilitySupport::Unknown,
    }
}

pub(crate) fn documented_model(spec: CompatibleSpec, model: &str) -> ModelDescriptor {
    let mut descriptor = ModelDescriptor::unknown(model);
    // Exact model rows in the provider's tool-use documentation, reviewed 2026-09-05.
    let tools = match spec.catalog_dialect {
        CatalogDialect::Groq => matches!(
            model,
            "openai/gpt-oss-20b"
                | "openai/gpt-oss-120b"
                | "openai/gpt-oss-safeguard-20b"
                | "qwen/qwen3.6-27b"
                | "qwen/qwen3.8-27b"
                | "minimaxai/minimax-m2.7"
                | "llama-3.3-70b-versatile"
                | "llama-3.1-8b-instant"
        ),
        CatalogDialect::Fireworks => model == "accounts/fireworks/models/kimi-k2-instruct-0905",
        CatalogDialect::Mistral => false,
        CatalogDialect::Together => model == "meta-llama/Llama-3.3-70B-Instruct-Turbo",
        CatalogDialect::Xai => matches!(model, "grok-4.6" | "grok-4.3" | "grok-4.3-latest"),
    };
    if tools {
        descriptor.capabilities.tools = CapabilitySupport::Supported;
    }
    descriptor
}

#[derive(Deserialize)]
struct GroqEnvelope {
    data: Vec<GroqRow>,
}
#[derive(Deserialize)]
struct GroqRow {
    id: String,
    active: Option<bool>,
    context_window: Option<u64>,
}

#[derive(Deserialize)]
struct MistralEnvelope {
    data: Vec<MistralRow>,
}
#[derive(Deserialize)]
struct MistralRow {
    id: String,
    archived: Option<bool>,
    max_context_length: Option<u64>,
    capabilities: Option<MistralCapabilities>,
}
#[derive(Deserialize)]
struct MistralCapabilities {
    completion_chat: Option<bool>,
    function_calling: Option<bool>,
    vision: Option<bool>,
}

#[derive(Deserialize)]
struct TogetherRow {
    id: String,
    r#type: Option<String>,
    context_length: Option<u64>,
}
#[derive(Deserialize)]
struct XaiEnvelope {
    models: Vec<XaiRow>,
}
#[derive(Deserialize)]
struct XaiRow {
    id: String,
    input_modalities: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FireworksEnvelope {
    models: Vec<FireworksRow>,
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FireworksRow {
    name: String,
    display_name: Option<String>,
    state: Option<String>,
    conversation_config: Option<serde_json::Map<String, serde_json::Value>>,
    supports_serverless: Option<bool>,
    supports_tools: Option<bool>,
    supports_image_input: Option<bool>,
    context_length: Option<u64>,
    deprecation_date: Option<FireworksDate>,
}

#[derive(Deserialize)]
struct FireworksDate {
    #[serde(default)]
    year: i32,
    #[serde(default)]
    month: u32,
    #[serde(default)]
    day: u32,
}

impl FireworksDate {
    fn lifecycle(self) -> Result<heycode_llm::ModelLifecycle, CatalogFetchError> {
        if !(0..=9999).contains(&self.year) || self.month > 12 || self.day > 31 {
            return Err(invalid());
        }
        if self.year == 0 && self.month == 0 && self.day == 0 {
            return Ok(heycode_llm::ModelLifecycle::unknown());
        }
        let retirement = if self.year == 0 || self.month == 0 || self.day == 0 {
            None
        } else {
            // Date-only serverless shutdowns use the same UTC-day boundary as other catalogs.
            let date = chrono::NaiveDate::from_ymd_opt(self.year, self.month, self.day)
                .ok_or_else(invalid)?;
            let instant = date
                .and_hms_opt(0, 0, 0)
                .ok_or_else(invalid)?
                .and_utc()
                .timestamp_millis();
            Some(u64::try_from(instant).map_err(|_| invalid())?)
        };
        Ok(heycode_llm::ModelLifecycle::deprecated(
            retirement,
            Vec::new(),
        ))
    }
}

fn json_content_type(value: &str) -> bool {
    let media_type = value.split(';').next().unwrap_or_default().trim();
    media_type.eq_ignore_ascii_case("application/json")
        || media_type.to_ascii_lowercase().starts_with("application/")
            && media_type.to_ascii_lowercase().ends_with("+json")
}
