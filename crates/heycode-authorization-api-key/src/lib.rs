//! Masked API-key authorization flow and provider validation.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use heycode_authorization::{
    AuthorizationDescriptor, AuthorizationFlow, AuthorizationFlowFailure, AuthorizationFlowId,
    AuthorizationGrant, AuthorizationMethod, AuthorizationOperationId, AuthorizationRequest,
    AuthorizationService, SERVICE_AUTHORIZATION,
};
use heycode_core::{
    Context, CoreError, CoreResult, Plugin, PluginContributionKind, PluginDescriptor,
};
use heycode_credentials::{CredentialQuery, CredentialSecret};
use tokio_util::sync::CancellationToken;

/// Interactive masked secret prompt service.
pub const SERVICE_SECRET_PROMPT: heycode_core::ServiceKey =
    heycode_core::ServiceKey::new("secret-prompt");

/// Masked secret-input prompt request consumed by U06/TUI implementations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretPromptRequest {
    /// Human prompt.
    pub prompt: String,
    /// Safe validation feedback for a fresh, empty retry field.
    pub error: Option<String>,
    /// Credential query being entered (reference/kind only).
    pub query: CredentialQuery,
    /// Safe caller correlation for one dialog operation.
    pub operation: Option<AuthorizationOperationId>,
    /// Must remain true for API key flows.
    pub masked: bool,
}

/// Human secret-input boundary. Implementations never echo/store the value.
#[async_trait]
pub trait SecretPrompt: Send + Sync {
    /// Whether this interactive surface can collect a corrected credential.
    fn supports_retry(&self) -> bool {
        false
    }

    /// Collect one secret.
    ///
    /// # Errors
    /// Cancellation/input failures return safe text only.
    async fn prompt(
        &self,
        request: SecretPromptRequest,
        cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String>;
}

/// Safe event sent to TUI/front-end adapters. It never contains input text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecretPromptNotification {
    /// A masked input dialog should open.
    Requested {
        /// Monotonic prompt id.
        id: u64,
        /// Human prompt.
        prompt: String,
        /// Safe validation feedback; never includes the credential.
        error: Option<String>,
        /// Non-secret reference/kind.
        query: CredentialQuery,
        /// Safe caller correlation, when supplied.
        operation: Option<AuthorizationOperationId>,
        /// Whether rendering must mask (always true for API-key flows).
        masked: bool,
    },
    /// Dialog settled or was cancelled.
    Resolved {
        /// Prompt id.
        id: u64,
        /// Whether a secret was answered (never the secret itself).
        answered: bool,
    },
}

struct PromptInner {
    bus: heycode_core::EventBus,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, tokio::sync::oneshot::Sender<CredentialSecret>>>,
}

struct PendingPromptGuard {
    inner: std::sync::Weak<PromptInner>,
    id: u64,
}

impl Drop for PendingPromptGuard {
    fn drop(&mut self) {
        let Some(inner) = self.inner.upgrade() else {
            return;
        };
        let removed = inner
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&self.id))
            .is_some();
        if removed {
            inner.bus.emit(SecretPromptNotification::Resolved {
                id: self.id,
                answered: false,
            });
        }
    }
}

/// Event-driven masked input broker used by authorization flows and the TUI.
#[derive(Clone)]
pub struct InteractiveSecretPrompt {
    inner: Arc<PromptInner>,
}

impl InteractiveSecretPrompt {
    /// Build over the composed world's shared event bus.
    #[must_use]
    pub fn new(bus: heycode_core::EventBus) -> Self {
        Self {
            inner: Arc::new(PromptInner {
                bus,
                next_id: AtomicU64::new(1),
                pending: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Subscribe to safe prompt notifications.
    #[must_use]
    pub fn subscribe(&self) -> tokio::sync::mpsc::UnboundedReceiver<SecretPromptNotification> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.inner.bus.on::<SecretPromptNotification>(move |event| {
            let _ = sender.send(event.clone());
        });
        receiver
    }

    /// Subscribe for one owning plugin lifetime.
    ///
    /// Context rollback/shutdown removes the exact listener rather than
    /// retaining a closed per-operation sender in the event bus.
    #[must_use]
    pub fn subscribe_effect(
        &self,
        context: &Context,
    ) -> tokio::sync::mpsc::UnboundedReceiver<SecretPromptNotification> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.inner
            .bus
            .on_effect::<SecretPromptNotification>(context, move |event| {
                let _ = sender.send(event.clone());
            });
        receiver
    }

    /// Answer one prompt. Returns false for stale/unknown ids.
    pub fn answer(&self, id: u64, secret: CredentialSecret) -> bool {
        let sender = self
            .inner
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&id));
        let Some(sender) = sender else {
            return false;
        };
        let answered = sender.send(secret).is_ok();
        self.inner
            .bus
            .emit(SecretPromptNotification::Resolved { id, answered });
        answered
    }

    /// Cancel one prompt. Returns false for stale/unknown ids.
    pub fn cancel(&self, id: u64) -> bool {
        let removed = self
            .inner
            .pending
            .lock()
            .ok()
            .and_then(|mut pending| pending.remove(&id))
            .is_some();
        if removed {
            self.inner.bus.emit(SecretPromptNotification::Resolved {
                id,
                answered: false,
            });
        }
        removed
    }
}

#[async_trait]
impl SecretPrompt for InteractiveSecretPrompt {
    fn supports_retry(&self) -> bool {
        true
    }

    async fn prompt(
        &self,
        request: SecretPromptRequest,
        cancellation: CancellationToken,
    ) -> Result<CredentialSecret, String> {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        if id == u64::MAX {
            return Err("secret prompt id space exhausted".to_owned());
        }
        let (sender, receiver) = tokio::sync::oneshot::channel();
        self.inner
            .pending
            .lock()
            .map_err(|_| "secret prompt registry unavailable".to_owned())?
            .insert(id, sender);
        let _pending = PendingPromptGuard {
            inner: Arc::downgrade(&self.inner),
            id,
        };
        self.inner.bus.emit(SecretPromptNotification::Requested {
            id,
            prompt: request.prompt,
            error: request.error,
            query: request.query,
            operation: request.operation,
            masked: request.masked,
        });
        tokio::select! {
            () = cancellation.cancelled() => {
                Err("secret input cancelled".to_owned())
            }
            result = receiver => {
                result.map_err(|_| "secret input cancelled".to_owned())
            }
        }
    }
}

/// Required live-validation failure taxonomy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiKeyValidationFailure {
    /// Key was rejected (401/403).
    Unauthorized,
    /// Validation endpoint/base/protocol is wrong.
    Host,
    /// Authentication worked but configured model is absent.
    Model,
    /// Transport, timeout, rate limit, or provider 5xx.
    Network,
    /// Caller cancelled validation.
    Cancelled,
}

impl ApiKeyValidationFailure {
    /// Stable machine code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unauthorized => "unauthorized",
            Self::Host => "host",
            Self::Model => "model",
            Self::Network => "network",
            Self::Cancelled => "cancelled",
        }
    }

    fn message(self) -> &'static str {
        match self {
            Self::Unauthorized => "API key was rejected",
            Self::Host => "validation endpoint or response is invalid",
            Self::Model => "configured model is not available to this credential",
            Self::Network => "validation could not reach a healthy provider service",
            Self::Cancelled => "validation was cancelled",
        }
    }

    fn flow_failure(self) -> AuthorizationFlowFailure {
        AuthorizationFlowFailure::new(self.code(), self.message())
    }
}

/// Provider-specific API-key validator.
#[async_trait]
pub trait ApiKeyValidator: Send + Sync {
    /// Validate without logging/storing the secret.
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure>;
}

/// Configuration for one provider-specific API-key flow.
#[derive(Debug, Clone)]
pub struct ApiKeyFlowConfig {
    /// Registry id.
    pub id: AuthorizationFlowId,
    /// Human label.
    pub label: String,
    /// Exact query this flow owns.
    pub query: CredentialQuery,
    /// Masked-entry prompt text.
    pub prompt: String,
}

/// Masked input → live validate → validated grant.
#[derive(Clone)]
pub struct ApiKeyAuthorizationFlow {
    config: ApiKeyFlowConfig,
    prompt: Arc<dyn SecretPrompt>,
    validator: Arc<dyn ApiKeyValidator>,
}

impl ApiKeyAuthorizationFlow {
    /// Construct one flow.
    #[must_use]
    pub fn new(
        config: ApiKeyFlowConfig,
        prompt: Arc<dyn SecretPrompt>,
        validator: Arc<dyn ApiKeyValidator>,
    ) -> Self {
        Self {
            config,
            prompt,
            validator,
        }
    }

    /// Stable id.
    #[must_use]
    pub fn id(&self) -> &AuthorizationFlowId {
        &self.config.id
    }
}

#[async_trait]
impl AuthorizationFlow for ApiKeyAuthorizationFlow {
    fn descriptor(&self) -> AuthorizationDescriptor {
        AuthorizationDescriptor {
            id: self.config.id.clone(),
            label: self.config.label.clone(),
            method: AuthorizationMethod::ApiKey,
            interactive: true,
            query: self.config.query.clone(),
        }
    }

    async fn validate_existing(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), AuthorizationFlowFailure> {
        self.validator
            .validate(secret, cancellation)
            .await
            .map_err(ApiKeyValidationFailure::flow_failure)
    }

    async fn authorize(
        &self,
        request: AuthorizationRequest,
    ) -> Result<AuthorizationGrant, AuthorizationFlowFailure> {
        if request.query != self.config.query {
            return Err(AuthorizationFlowFailure::new(
                "reference",
                "authorization flow does not own this credential reference",
            ));
        }
        let mut feedback = None;
        let secret = loop {
            let secret = self
                .prompt
                .prompt(
                    SecretPromptRequest {
                        prompt: self.config.prompt.clone(),
                        error: feedback.take(),
                        query: request.query.clone(),
                        operation: request.operation,
                        masked: true,
                    },
                    request.cancellation.clone(),
                )
                .await
                .map_err(|message| AuthorizationFlowFailure::new("input", message))?;
            if request.cancellation.is_cancelled() {
                return Err(ApiKeyValidationFailure::Cancelled.flow_failure());
            }
            match self
                .validator
                .validate(&secret, request.cancellation.clone())
                .await
            {
                Ok(()) => break secret,
                Err(ApiKeyValidationFailure::Unauthorized) if self.prompt.supports_retry() => {
                    feedback =
                        Some("API key is invalid or revoked. Enter a valid key to retry.".into());
                }
                Err(error) => return Err(error.flow_failure()),
            }
            // The rejected secret is dropped before collecting the replacement.
        };
        if request.cancellation.is_cancelled() {
            return Err(ApiKeyValidationFailure::Cancelled.flow_failure());
        }
        let checked_at_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| AuthorizationFlowFailure::new("clock", "system clock is before epoch"))?
            .as_millis()
            .try_into()
            .map_err(|_| AuthorizationFlowFailure::new("clock", "system clock overflow"))?;
        Ok(AuthorizationGrant::validated(secret, checked_at_ms))
    }
}

/// Official OpenRouter API origin.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Official DeepSeek API origin.
pub const DEEPSEEK_BASE_URL: &str = "https://api.deepseek.com";

/// HTTP validator for bearer-auth key/model endpoints.
pub struct HttpApiKeyValidator {
    client: reqwest::Client,
    auth_url: reqwest::Url,
    models_url: Option<reqwest::Url>,
    required_model: Option<String>,
    current_key_response: bool,
}

impl HttpApiKeyValidator {
    /// Construct an explicit validation plan (mock/proxy/provider use).
    ///
    /// # Errors
    /// Invalid URLs/client construction classify as `host`.
    pub fn new(
        auth_url: String,
        models_url: Option<String>,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let auth_url = reqwest::Url::parse(&auth_url)
            .map_err(|_| ApiKeyValidationFailure::Host.flow_failure())?;
        let models_url = models_url
            .map(|url| {
                reqwest::Url::parse(&url).map_err(|_| ApiKeyValidationFailure::Host.flow_failure())
            })
            .transpose()?;
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(10))
            .build()
            .map_err(|_| ApiKeyValidationFailure::Host.flow_failure())?;
        Ok(Self {
            client,
            auth_url,
            models_url,
            required_model,
            current_key_response: false,
        })
    }

    /// Official OpenRouter current-key endpoint plus optional model catalog.
    ///
    /// # Errors
    /// URL/client construction failure.
    pub fn openrouter(required_model: Option<String>) -> Result<Self, AuthorizationFlowFailure> {
        Self::openrouter_at(OPENROUTER_BASE_URL, required_model)
    }

    /// OpenRouter-shaped endpoints under a configured base URL (a proxy or
    /// gateway). A key configured for a gateway must never be sent to the
    /// official host to be "checked".
    ///
    /// # Errors
    /// URL/client construction failure.
    pub fn openrouter_at(
        base_url: &str,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let base_url = base_url.trim_end_matches('/');
        let mut validator = Self::new(
            format!("{base_url}/key"),
            required_model
                .as_ref()
                .map(|_| format!("{base_url}/models")),
            required_model,
        )?;
        validator.current_key_response = true;
        Ok(validator)
    }

    /// Official DeepSeek authenticated models endpoint.
    ///
    /// # Errors
    /// URL/client construction failure.
    pub fn deepseek(required_model: Option<String>) -> Result<Self, AuthorizationFlowFailure> {
        Self::deepseek_at(DEEPSEEK_BASE_URL, required_model)
    }

    /// DeepSeek-shaped models endpoint under a configured base URL.
    ///
    /// # Errors
    /// URL/client construction failure.
    pub fn deepseek_at(
        base_url: &str,
        required_model: Option<String>,
    ) -> Result<Self, AuthorizationFlowFailure> {
        let base_url = base_url.trim_end_matches('/');
        Self::new(format!("{base_url}/models"), None, required_model)
    }

    /// Endpoints this validator will contact: the key/auth probe and, when a
    /// model is required, the catalog listing.
    #[must_use]
    pub fn endpoints(&self) -> (&str, Option<&str>) {
        (
            self.auth_url.as_str(),
            self.models_url.as_ref().map(reqwest::Url::as_str),
        )
    }

    async fn request(
        &self,
        url: reqwest::Url,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<serde_json::Value, ApiKeyValidationFailure> {
        let send = self.client.get(url).bearer_auth(secret.expose()).send();
        let response = tokio::select! {
            () = cancellation.cancelled() => return Err(ApiKeyValidationFailure::Cancelled),
            result = send => result.map_err(|_| ApiKeyValidationFailure::Network)?,
        };
        match response.status().as_u16() {
            200..=299 => response
                .json::<serde_json::Value>()
                .await
                .map_err(|_| ApiKeyValidationFailure::Host),
            401 | 403 => Err(ApiKeyValidationFailure::Unauthorized),
            404 => Err(ApiKeyValidationFailure::Host),
            429 | 500..=599 => Err(ApiKeyValidationFailure::Network),
            _ => Err(ApiKeyValidationFailure::Host),
        }
    }
}

#[async_trait]
impl ApiKeyValidator for HttpApiKeyValidator {
    async fn validate(
        &self,
        secret: &CredentialSecret,
        cancellation: CancellationToken,
    ) -> Result<(), ApiKeyValidationFailure> {
        let auth_body = self
            .request(self.auth_url.clone(), secret, cancellation.clone())
            .await?;
        let valid_shape = if self.current_key_response {
            auth_body
                .get("data")
                .is_some_and(serde_json::Value::is_object)
        } else {
            auth_body.is_array()
                || auth_body
                    .get("data")
                    .is_some_and(serde_json::Value::is_array)
                || auth_body
                    .get("models")
                    .is_some_and(serde_json::Value::is_array)
        };
        if auth_body.get("error").is_some() || !valid_shape {
            return Err(ApiKeyValidationFailure::Host);
        }
        let Some(required_model) = self.required_model.as_ref() else {
            return Ok(());
        };
        let model_body = match self.models_url.as_ref() {
            Some(url) => self.request(url.clone(), secret, cancellation).await?,
            None => auth_body,
        };
        let found = model_body
            .get("data")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|models| {
                models.iter().any(|model| {
                    model.get("id").and_then(serde_json::Value::as_str)
                        == Some(required_model.as_str())
                })
            });
        if found {
            Ok(())
        } else {
            Err(ApiKeyValidationFailure::Model)
        }
    }
}

/// Register API-key flows into the authorization service.
#[must_use]
pub fn api_key_authorization_plugin(flows: Vec<ApiKeyAuthorizationFlow>) -> Box<dyn Plugin> {
    struct ApiKeyPlugin(Vec<ApiKeyAuthorizationFlow>);
    impl Plugin for ApiKeyPlugin {
        fn name(&self) -> &'static str {
            "authorization-api-key"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "authorization-api-key",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Provider],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            self.0
                .iter()
                .map(|flow| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::AuthorizationFlow,
                        flow.id().as_str(),
                    )
                })
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_AUTHORIZATION]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let authorization = context
                .get::<AuthorizationService>(SERVICE_AUTHORIZATION)
                .ok_or_else(|| CoreError::other("authorization missing"))?;
            for flow in &self.0 {
                authorization
                    .register(context, Arc::new(flow.clone()))
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            Ok(())
        }
    }
    Box::new(ApiKeyPlugin(flows))
}

/// Publish the interactive secret prompt broker for TUI/front-end adapters.
#[must_use]
pub fn secret_prompt_plugin(prompt: InteractiveSecretPrompt) -> Box<dyn Plugin> {
    struct SecretPromptPlugin(InteractiveSecretPrompt);
    impl Plugin for SecretPromptPlugin {
        fn name(&self) -> &'static str {
            "secret-prompt"
        }

        fn descriptor(&self) -> PluginDescriptor {
            PluginDescriptor::built_in(
                "secret-prompt",
                env!("CARGO_PKG_VERSION"),
                &[PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[SERVICE_SECRET_PROMPT]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            context.provide(SERVICE_SECRET_PROMPT, "secret-prompt", self.0.clone())
        }
    }
    Box::new(SecretPromptPlugin(prompt))
}
