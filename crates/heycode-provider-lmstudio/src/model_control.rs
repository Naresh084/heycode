//! Explicit LM Studio model load/unload boundary.
//!
//! Planning is pure and never sends a request. Loading consumes a non-Clone
//! plan, always asks LM Studio to echo the applied configuration, and verifies
//! every user-specified value before returning a receipt. Inference selection
//! under this boundary accepts loaded instances only; downloaded weights never
//! trigger JIT loading as a side effect.

use std::sync::Arc;

use heycode_credentials::CredentialsService;
use heycode_http::{HttpRequest, HttpResponse, HttpService, TransportError};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

use crate::{
    LmStudioAuth, LmStudioConfig, LmStudioLoadedInstance, LmStudioModelKind, LmStudioModelRecord,
};

const RESPONSE_LIMIT: usize = 64 * 1024;
const LOAD_PATH: &str = "/api/v1/models/load";
const UNLOAD_PATH: &str = "/api/v1/models/unload";

/// User-controlled LM Studio load-time settings.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LmStudioLoadSettings {
    context_length: Option<u64>,
    eval_batch_size: Option<u64>,
    flash_attention: Option<bool>,
    num_experts: Option<u64>,
    offload_kv_cache_to_gpu: Option<bool>,
}

impl LmStudioLoadSettings {
    /// Empty settings: LM Studio applies its configured defaults, which the
    /// echoed response still records.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            context_length: None,
            eval_batch_size: None,
            flash_attention: None,
            num_experts: None,
            offload_kv_cache_to_gpu: None,
        }
    }

    /// Set an explicit non-zero context length.
    ///
    /// # Errors
    /// Zero is rejected rather than delegated as an ambiguous default.
    pub fn with_context_length(mut self, value: u64) -> Result<Self, LmStudioControlError> {
        nonzero(value)?;
        self.context_length = Some(value);
        Ok(self)
    }

    /// Set an explicit non-zero evaluation batch size.
    ///
    /// # Errors
    /// Zero is rejected.
    pub fn with_eval_batch_size(mut self, value: u64) -> Result<Self, LmStudioControlError> {
        nonzero(value)?;
        self.eval_batch_size = Some(value);
        Ok(self)
    }

    /// Enable or disable Flash Attention explicitly.
    #[must_use]
    pub const fn with_flash_attention(mut self, value: bool) -> Self {
        self.flash_attention = Some(value);
        self
    }

    /// Set an explicit non-zero MoE expert count.
    ///
    /// # Errors
    /// Zero is rejected.
    pub fn with_num_experts(mut self, value: u64) -> Result<Self, LmStudioControlError> {
        nonzero(value)?;
        self.num_experts = Some(value);
        Ok(self)
    }

    /// Select GPU-vs-CPU KV-cache placement explicitly.
    #[must_use]
    pub const fn with_offload_kv_cache_to_gpu(mut self, value: bool) -> Self {
        self.offload_kv_cache_to_gpu = Some(value);
        self
    }

    /// Explicit context length.
    #[must_use]
    pub const fn context_length(&self) -> Option<u64> {
        self.context_length
    }

    /// Explicit evaluation batch size.
    #[must_use]
    pub const fn eval_batch_size(&self) -> Option<u64> {
        self.eval_batch_size
    }

    /// Explicit Flash Attention setting.
    #[must_use]
    pub const fn flash_attention(&self) -> Option<bool> {
        self.flash_attention
    }

    /// Explicit MoE expert count.
    #[must_use]
    pub const fn num_experts(&self) -> Option<u64> {
        self.num_experts
    }

    /// Explicit KV-cache offload setting.
    #[must_use]
    pub const fn offload_kv_cache_to_gpu(&self) -> Option<bool> {
        self.offload_kv_cache_to_gpu
    }

    fn body(&self, model: &str) -> serde_json::Value {
        let mut body = serde_json::json!({"model":model,"echo_load_config":true});
        let object = body.as_object_mut();
        if let Some(object) = object {
            insert_option(object, "context_length", self.context_length);
            insert_option(object, "eval_batch_size", self.eval_batch_size);
            insert_option(object, "flash_attention", self.flash_attention);
            insert_option(object, "num_experts", self.num_experts);
            insert_option(
                object,
                "offload_kv_cache_to_gpu",
                self.offload_kv_cache_to_gpu,
            );
        }
        body
    }

    fn matches_echo(&self, echoed: &serde_json::Map<String, serde_json::Value>) -> bool {
        matches_option(echoed, "context_length", self.context_length)
            && matches_option(echoed, "eval_batch_size", self.eval_batch_size)
            && matches_option(echoed, "flash_attention", self.flash_attention)
            && matches_option(echoed, "num_experts", self.num_experts)
            && matches_option(
                echoed,
                "offload_kv_cache_to_gpu",
                self.offload_kv_cache_to_gpu,
            )
    }
}

/// Validated one-shot load operation. Deliberately not Clone.
#[derive(Debug, PartialEq, Eq)]
pub struct LmStudioLoadPlan {
    model: String,
    settings: LmStudioLoadSettings,
}

impl LmStudioLoadPlan {
    /// Prepare one explicit model load without performing I/O.
    ///
    /// # Errors
    /// A requested context larger than the model's published maximum fails.
    pub fn prepare(
        record: &LmStudioModelRecord,
        settings: LmStudioLoadSettings,
    ) -> Result<Self, LmStudioControlError> {
        if let (Some(requested), Some(maximum)) =
            (settings.context_length(), record.max_context_length)
            && requested > maximum
        {
            return Err(LmStudioControlError::ContextExceedsModel);
        }
        Ok(Self {
            model: record.key.clone(),
            settings,
        })
    }

    /// Model key the user explicitly selected.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

/// Validated one-shot unload operation. Deliberately not Clone.
#[derive(Debug, PartialEq, Eq)]
pub struct LmStudioUnloadPlan {
    instance_id: String,
}

impl LmStudioUnloadPlan {
    /// Select one exact loaded instance to unload.
    ///
    /// # Errors
    /// Blank, untrimmed, control-bearing or oversized ids are rejected.
    pub fn new(instance_id: impl Into<String>) -> Result<Self, LmStudioControlError> {
        let instance_id = instance_id.into();
        if !safe_id(&instance_id) {
            return Err(LmStudioControlError::InvalidInstanceId);
        }
        Ok(Self { instance_id })
    }
}

/// Successful explicit load receipt.
#[derive(Debug, Clone, PartialEq)]
pub struct LmStudioLoadReceipt {
    instance_id: String,
    kind: LmStudioModelKind,
    load_time_seconds: f64,
    settings: LmStudioLoadSettings,
}

impl LmStudioLoadReceipt {
    /// Loaded instance id.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Loaded model kind.
    #[must_use]
    pub fn kind(&self) -> LmStudioModelKind {
        self.kind.clone()
    }

    /// Provider-reported load duration.
    #[must_use]
    pub const fn load_time_seconds(&self) -> f64 {
        self.load_time_seconds
    }

    /// User settings verified against the echoed configuration.
    #[must_use]
    pub const fn settings(&self) -> &LmStudioLoadSettings {
        &self.settings
    }
}

/// Successful explicit unload receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LmStudioUnloadReceipt {
    instance_id: String,
}

impl LmStudioUnloadReceipt {
    /// Unloaded instance id.
    #[must_use]
    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }
}

/// Provider-local explicit model management client.
#[derive(Clone)]
pub struct LmStudioModelControl {
    http: HttpService,
    credentials: Option<Arc<CredentialsService>>,
    config: LmStudioConfig,
}

impl LmStudioModelControl {
    /// Bind the same endpoint/auth configuration used by detection/catalogs.
    #[must_use]
    pub const fn new(
        http: HttpService,
        credentials: Option<Arc<CredentialsService>>,
        config: LmStudioConfig,
    ) -> Self {
        Self {
            http,
            credentials,
            config,
        }
    }

    /// Prepare a load without performing I/O.
    ///
    /// # Errors
    /// Same as [`LmStudioLoadPlan::prepare`].
    pub fn prepare_load(
        &self,
        record: &LmStudioModelRecord,
        settings: LmStudioLoadSettings,
    ) -> Result<LmStudioLoadPlan, LmStudioControlError> {
        LmStudioLoadPlan::prepare(record, settings)
    }

    /// Require an already-loaded model for inference selection.
    ///
    /// This method never sends a request and therefore cannot trigger LM
    /// Studio's JIT loading behavior.
    ///
    /// # Errors
    /// Downloaded-only models require an explicit load first.
    pub fn require_loaded<'record>(
        &self,
        record: &'record LmStudioModelRecord,
    ) -> Result<&'record [LmStudioLoadedInstance], LmStudioControlError> {
        if record.loaded_instances.is_empty() {
            return Err(LmStudioControlError::ExplicitLoadRequired);
        }
        Ok(&record.loaded_instances)
    }

    /// Consume one explicit plan and load the selected model.
    ///
    /// # Errors
    /// Cancellation, auth/transport/status/response failure, or an echoed
    /// configuration that differs from the plan fails without a receipt.
    pub async fn load(
        &self,
        plan: LmStudioLoadPlan,
        cancellation: CancellationToken,
    ) -> Result<LmStudioLoadReceipt, LmStudioControlError> {
        if cancellation.is_cancelled() {
            return Err(LmStudioControlError::Cancelled);
        }
        let body = plan.settings.body(&plan.model);
        let response = self.post(LOAD_PATH, body, cancellation).await?;
        let decoded: LoadResponse = decode_json(&response)?;
        if decoded.status != "loaded"
            || !safe_id(&decoded.instance_id)
            || !decoded.load_time_seconds.is_finite()
            || decoded.load_time_seconds < 0.0
        {
            return Err(LmStudioControlError::InvalidResponse);
        }
        let kind = match decoded.kind.as_str() {
            "llm" => LmStudioModelKind::Llm,
            "embedding" => LmStudioModelKind::Embedding,
            _ => return Err(LmStudioControlError::InvalidResponse),
        };
        if !plan.settings.matches_echo(&decoded.load_config) {
            return Err(LmStudioControlError::SettingsMismatch);
        }
        Ok(LmStudioLoadReceipt {
            instance_id: decoded.instance_id,
            kind,
            load_time_seconds: decoded.load_time_seconds,
            settings: plan.settings,
        })
    }

    /// Consume one explicit instance selection and unload it.
    ///
    /// # Errors
    /// Cancellation, auth/transport/status/response failure, or a mismatched
    /// echoed instance id fails without a receipt.
    pub async fn unload(
        &self,
        plan: LmStudioUnloadPlan,
        cancellation: CancellationToken,
    ) -> Result<LmStudioUnloadReceipt, LmStudioControlError> {
        if cancellation.is_cancelled() {
            return Err(LmStudioControlError::Cancelled);
        }
        let expected = plan.instance_id;
        let response = self
            .post(
                UNLOAD_PATH,
                serde_json::json!({"instance_id":expected}),
                cancellation,
            )
            .await?;
        let decoded: UnloadResponse = decode_json(&response)?;
        if decoded.instance_id != expected || !safe_id(&decoded.instance_id) {
            return Err(LmStudioControlError::InvalidResponse);
        }
        Ok(LmStudioUnloadReceipt {
            instance_id: decoded.instance_id,
        })
    }

    async fn post(
        &self,
        path: &str,
        body: serde_json::Value,
        cancellation: CancellationToken,
    ) -> Result<HttpResponse, LmStudioControlError> {
        let url = format!("{}{}", self.config.endpoint().base_url(), path);
        let encoded =
            serde_json::to_vec(&body).map_err(|_| LmStudioControlError::InvalidSettings)?;
        let mut request = HttpRequest::post(url, encoded)
            .and_then(|request| request.header("accept", "application/json"))
            .and_then(|request| request.header("content-type", "application/json"))
            .map(|request| request.with_max_response_bytes(RESPONSE_LIMIT))
            .map_err(|_| LmStudioControlError::InvalidSettings)?;
        if let Some(authorization) = self.authorization()? {
            request = request
                .header("authorization", &authorization)
                .map_err(|_| LmStudioControlError::CredentialUnavailable)?;
        }
        let response = tokio::time::timeout(
            self.config.catalog_timeout(),
            self.http.send(request, cancellation.clone()),
        )
        .await
        .map_err(|_| LmStudioControlError::Network)?
        .map_err(map_transport)?;
        if cancellation.is_cancelled() {
            return Err(LmStudioControlError::Cancelled);
        }
        match response.status {
            200..=299 => Ok(response),
            401 | 403 => Err(LmStudioControlError::Unauthorized),
            _ => Err(LmStudioControlError::Unavailable),
        }
    }

    fn authorization(&self) -> Result<Option<String>, LmStudioControlError> {
        let LmStudioAuth::BearerToken(query) = self.config.auth() else {
            return Ok(None);
        };
        let credentials = self
            .credentials
            .as_ref()
            .ok_or(LmStudioControlError::CredentialUnavailable)?;
        let secret = credentials
            .resolve(query)
            .map_err(|_| LmStudioControlError::CredentialUnavailable)?
            .ok_or(LmStudioControlError::CredentialUnavailable)?;
        Ok(Some(format!("Bearer {}", secret.expose())))
    }
}

impl std::fmt::Debug for LmStudioModelControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LmStudioModelControl")
            .field(
                "authenticated",
                &matches!(self.config.auth(), LmStudioAuth::BearerToken(_)),
            )
            .finish()
    }
}

/// Stable explicit-model-control failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LmStudioControlError {
    /// A numeric setting was zero or otherwise invalid.
    #[error("LM Studio load settings are invalid")]
    InvalidSettings,
    /// Requested context exceeds the model's published architecture limit.
    #[error("LM Studio load context exceeds the model maximum")]
    ContextExceedsModel,
    /// Inference selection named a downloaded-only model.
    #[error("LM Studio model must be loaded explicitly before inference")]
    ExplicitLoadRequired,
    /// One exact loaded instance id was not usable.
    #[error("LM Studio unload requires one valid instance id")]
    InvalidInstanceId,
    /// Credential lookup could not produce an operation credential.
    #[error("LM Studio model control credential is unavailable")]
    CredentialUnavailable,
    /// Server rejected the credential.
    #[error("LM Studio model control authorization failed")]
    Unauthorized,
    /// Server could not perform the operation.
    #[error("LM Studio model control is unavailable")]
    Unavailable,
    /// Transport or deadline failed.
    #[error("LM Studio model control network operation failed")]
    Network,
    /// Caller cancellation settled the operation.
    #[error("LM Studio model control was cancelled")]
    Cancelled,
    /// Response shape did not match the documented endpoint.
    #[error("LM Studio model control response is invalid")]
    InvalidResponse,
    /// Echoed applied settings differ from the explicit plan.
    #[error("LM Studio applied settings differ from the explicit load plan")]
    SettingsMismatch,
}

#[derive(Deserialize)]
struct LoadResponse {
    #[serde(rename = "type")]
    kind: String,
    instance_id: String,
    load_time_seconds: f64,
    status: String,
    load_config: serde_json::Map<String, serde_json::Value>,
}

#[derive(Deserialize)]
struct UnloadResponse {
    instance_id: String,
}

fn decode_json<T: for<'de> Deserialize<'de>>(
    response: &HttpResponse,
) -> Result<T, LmStudioControlError> {
    if !response
        .content_type
        .as_deref()
        .is_some_and(|value| value == "application/json" || value.ends_with("+json"))
    {
        return Err(LmStudioControlError::InvalidResponse);
    }
    serde_json::from_slice(&response.body).map_err(|_| LmStudioControlError::InvalidResponse)
}

fn map_transport(error: TransportError) -> LmStudioControlError {
    match error {
        TransportError::Cancelled => LmStudioControlError::Cancelled,
        TransportError::Http {
            status: 401 | 403, ..
        } => LmStudioControlError::Unauthorized,
        TransportError::Http { .. } => LmStudioControlError::Unavailable,
        _ => LmStudioControlError::Network,
    }
}

fn nonzero(value: u64) -> Result<(), LmStudioControlError> {
    if value == 0 {
        Err(LmStudioControlError::InvalidSettings)
    } else {
        Ok(())
    }
}

fn safe_id(value: &str) -> bool {
    !value.is_empty()
        && value.trim() == value
        && value.len() <= 512
        && !value.chars().any(char::is_control)
}

fn insert_option<T: Into<serde_json::Value>>(
    object: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: Option<T>,
) {
    if let Some(value) = value {
        object.insert(key.to_owned(), value.into());
    }
}

fn matches_option<T: Into<serde_json::Value>>(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
    expected: Option<T>,
) -> bool {
    expected.is_none_or(|expected| object.get(key) == Some(&expected.into()))
}
