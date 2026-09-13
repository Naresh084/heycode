//! Transport-neutral typed client.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;

use crate::{
    APP_SERVER_PROTOCOL_VERSION, AppAuthorizationFlow, AppAuthorizationReceipt, AppCatalogRefresh,
    AppInitializeResult, AppLogoutResult, AppModelCatalog, AppPluginInventory, AppProviderCatalog,
    AppRouteSelection, AppRuntimeCatalog, AppServerError, AppServerErrorCode,
    AppServerNotification, AppSessionInfo, AppSettingsSnapshot, AppTurnResult,
    AppWorkspaceSelection,
};

const MAX_WIRE_BYTES: usize = 4 * 1024 * 1024;

/// Raw request/notification/response exchange used by the typed SDK.
///
/// Implementations own one request lifecycle, send zero or more complete JSON
/// notification frames in order, and return one complete JSON response frame.
/// They must not log raw frames because authorization answers may contain a
/// transient credential.
#[async_trait]
pub trait AppTransport: Send + Sync + 'static {
    /// Exchange one stable JSON-RPC request.
    ///
    /// # Errors
    /// Return only a body-free classified app-server error. Implementations
    /// must honor `cancellation` and settle owned I/O/tasks before returning.
    async fn exchange(
        &self,
        request: String,
        notifications: mpsc::Sender<String>,
        cancellation: CancellationToken,
    ) -> Result<String, AppServerError>;
}

/// Typed, cloneable client over one caller-supplied transport.
pub struct AppClient<T: AppTransport> {
    transport: Arc<T>,
    next_id: Arc<AtomicU64>,
    session: Arc<Mutex<Option<AppSessionInfo>>>,
}

impl<T: AppTransport> Clone for AppClient<T> {
    fn clone(&self) -> Self {
        Self {
            transport: self.transport.clone(),
            next_id: self.next_id.clone(),
            session: self.session.clone(),
        }
    }
}

impl<T: AppTransport> AppClient<T> {
    /// Bind a typed client to one transport.
    #[must_use]
    pub fn new(transport: Arc<T>) -> Self {
        Self {
            transport,
            next_id: Arc::new(AtomicU64::new(1)),
            session: Arc::new(Mutex::new(None)),
        }
    }

    /// Negotiate the stable protocol and inspect installed surfaces.
    ///
    /// # Errors
    /// Transport, response validation, or protocol-version mismatch.
    pub async fn initialize(&self) -> Result<AppInitializeResult, AppServerError> {
        let initialized: AppInitializeResult = self
            .typed_call(
                "initialize",
                serde_json::json!({}),
                None,
                CancellationToken::new(),
            )
            .await?;
        if initialized.protocol_version != APP_SERVER_PROTOCOL_VERSION {
            return Err(invalid());
        }
        Ok(initialized)
    }

    /// Initialize and open the host's current new session.
    ///
    /// Session creation authority belongs to the host composition; the SDK
    /// verifies and retains the returned typed identity.
    ///
    /// # Errors
    /// Initialization, transport, session admission, or response validation.
    pub async fn start(&self) -> Result<AppSessionInfo, AppServerError> {
        self.start_with_configuration(crate::AppRuntimeConfiguration::default())
            .await
    }

    /// Initialize and open the host session with explicit model-visible controls.
    ///
    /// # Errors
    /// Initialization, validation, transport, runtime admission, or response validation.
    pub async fn start_with_configuration(
        &self,
        configuration: crate::AppRuntimeConfiguration,
    ) -> Result<AppSessionInfo, AppServerError> {
        self.initialize().await?;
        let info = self.open_current(configuration).await?;
        *self.session.lock().await = Some(info.clone());
        Ok(info)
    }

    /// Open the host's current resumed session and require its exact id.
    ///
    /// Resume selection/path authority belongs to the host process. This
    /// method prevents a client from silently attaching to a different one.
    ///
    /// # Errors
    /// Initialization/open failure or returned session-id mismatch.
    pub async fn resume(
        &self,
        expected_session_id: &str,
    ) -> Result<AppSessionInfo, AppServerError> {
        self.resume_with_configuration(
            expected_session_id,
            crate::AppRuntimeConfiguration::default(),
        )
        .await
    }

    /// Open the current resumed session with explicit model-visible controls.
    ///
    /// # Errors
    /// Initialization/open failure or returned session-id mismatch.
    pub async fn resume_with_configuration(
        &self,
        expected_session_id: &str,
        configuration: crate::AppRuntimeConfiguration,
    ) -> Result<AppSessionInfo, AppServerError> {
        self.initialize().await?;
        let info = self.open_current(configuration).await?;
        if info.session_id != expected_session_id {
            return Err(invalid());
        }
        *self.session.lock().await = Some(info.clone());
        Ok(info)
    }

    /// Compatibility alias for [`Self::start`].
    ///
    /// # Errors
    /// The same failures as [`Self::start`].
    pub async fn open(&self) -> Result<AppSessionInfo, AppServerError> {
        self.start().await
    }

    async fn open_current(
        &self,
        configuration: crate::AppRuntimeConfiguration,
    ) -> Result<AppSessionInfo, AppServerError> {
        self.typed_call(
            "session/open",
            serde_json::json!({"configuration":configuration}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Atomically update supported runtime controls between turns.
    ///
    /// # Errors
    /// Missing session, active-turn conflict, unsupported field, transport, or invalid response.
    pub async fn configure(
        &self,
        configuration: crate::AppRuntimeConfiguration,
    ) -> Result<AppSessionInfo, AppServerError> {
        let session = self.session.lock().await.clone().ok_or_else(invalid)?;
        let info: AppSessionInfo = self
            .typed_call(
                "session/configure",
                serde_json::json!({
                    "sessionId":session.session_id,
                    "configuration":configuration,
                }),
                None,
                CancellationToken::new(),
            )
            .await?;
        if info.session_id != session.session_id {
            return Err(invalid());
        }
        *self.session.lock().await = Some(info.clone());
        Ok(info)
    }

    /// Discover provider-native model ids and exact reasoning-effort choices
    /// for the currently selected runtime.
    ///
    /// # Errors
    /// Runtime discovery, transport, or response validation failure.
    pub async fn runtime_models(
        &self,
    ) -> Result<Vec<crate::AppRuntimeModelConfiguration>, AppServerError> {
        self.typed_call(
            "runtime/models",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Run one complete turn while delivering validated typed notifications.
    ///
    /// The caller owns the bounded event channel and cancellation token. A
    /// clone may call [`Self::cancel`] concurrently; both futures must be
    /// awaited by the caller.
    ///
    /// # Errors
    /// Missing session, cancellation, transport/runtime failure, invalid
    /// response, event/session mismatch, or notification sequence gap.
    pub async fn turn(
        &self,
        text: &str,
        attachments: Vec<heycode_core::AttachmentMetadata>,
        events: mpsc::Sender<AppServerNotification>,
        cancellation: CancellationToken,
    ) -> Result<AppTurnResult, AppServerError> {
        let session = self.session.lock().await.clone().ok_or_else(invalid)?;
        self.typed_call(
            "turn/start",
            serde_json::json!({
                "sessionId":session.session_id,
                "text":text,
                "attachments":attachments,
            }),
            Some(events),
            cancellation,
        )
        .await
    }

    /// Cancel current session work.
    ///
    /// # Errors
    /// Closed/unavailable session, transport, or protocol failure.
    pub async fn cancel(&self) -> Result<(), AppServerError> {
        self.call(
            "turn/cancel",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Answer one runtime-originated permission request.
    ///
    /// # Errors
    /// Invalid/stale correlation, unavailable session, transport, or protocol failure.
    pub async fn respond_permission(
        &self,
        request_id: &str,
        decision: crate::AppPermissionDecision,
    ) -> Result<(), AppServerError> {
        let session = self.session.lock().await.clone().ok_or_else(invalid)?;
        self.call(
            "session/permission/respond",
            serde_json::json!({
                "sessionId":session.session_id,
                "requestId":request_id,
                "decision":decision,
            }),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Answer one runtime-originated human question.
    ///
    /// # Errors
    /// Invalid/stale correlation/answer, unavailable session, transport, or protocol failure.
    pub async fn respond_question(
        &self,
        request_id: &str,
        answer: &str,
    ) -> Result<(), AppServerError> {
        let session = self.session.lock().await.clone().ok_or_else(invalid)?;
        self.call(
            "session/question/respond",
            serde_json::json!({
                "sessionId":session.session_id,
                "requestId":request_id,
                "answer":answer,
            }),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Return explicitly selected labels without flattening them into custom text.
    /// # Errors
    /// Invalid or stale correlation, unsupported backend, or transport failure.
    pub async fn respond_question_selected(
        &self,
        request_id: &str,
        answers: &[String],
    ) -> Result<(), AppServerError> {
        let session = self.session.lock().await.clone().ok_or_else(invalid)?;
        self.call(
            "session/question/respond",
            serde_json::json!({
                "sessionId":session.session_id,"requestId":request_id,"selectedAnswers":answers,
            }),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Explicitly cancel one runtime-originated human question.
    ///
    /// # Errors
    /// Invalid/stale correlation, unavailable session, transport, or protocol failure.
    pub async fn cancel_question(&self, request_id: &str) -> Result<(), AppServerError> {
        let session = self.session.lock().await.clone().ok_or_else(invalid)?;
        self.call(
            "session/question/respond",
            serde_json::json!({
                "sessionId":session.session_id,
                "requestId":request_id,
                "cancelled":true,
            }),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Quiescent session close and local identity release.
    ///
    /// # Errors
    /// Closed/unavailable session, transport, or protocol failure.
    pub async fn close(&self) -> Result<(), AppServerError> {
        self.call(
            "session/close",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await?;
        self.session.lock().await.take();
        Ok(())
    }

    /// Read contributed authorization flows and credential-blind state.
    ///
    /// # Errors
    /// Missing controls, unavailable registry, transport, or invalid wire data.
    pub async fn authorization(&self) -> Result<Vec<AppAuthorizationFlow>, AppServerError> {
        self.typed_call(
            "authorization/list",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Run one route-bound authorization flow with typed control events.
    ///
    /// # Errors
    /// Invalid flow, cancellation, authorization/commit failure, unavailable
    /// controls, transport, or invalid wire data.
    pub async fn authorize(
        &self,
        flow_id: &str,
        events: mpsc::Sender<AppServerNotification>,
        cancellation: CancellationToken,
    ) -> Result<AppAuthorizationReceipt, AppServerError> {
        self.typed_call(
            "authorization/start",
            serde_json::json!({"flowId":flow_id}),
            Some(events),
            cancellation,
        )
        .await
    }

    /// Submit one bounded secret to the masked prompt broker.
    ///
    /// # Errors
    /// Invalid secret, stale prompt id, unavailable controls, or transport failure.
    pub async fn answer_authorization(
        &self,
        prompt_id: u64,
        secret: String,
    ) -> Result<(), AppServerError> {
        self.call(
            "authorization/answer",
            serde_json::json!({"promptId":prompt_id,"secret":secret}),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Cancel one active masked prompt.
    ///
    /// # Errors
    /// Stale prompt id, unavailable controls, or transport failure.
    pub async fn cancel_authorization(&self, prompt_id: u64) -> Result<(), AppServerError> {
        self.call(
            "authorization/cancel",
            serde_json::json!({"promptId":prompt_id}),
            None,
            CancellationToken::new(),
        )
        .await
        .map(|_| ())
    }

    /// Delete the uniquely owned authoritative credential for a provider.
    ///
    /// # Errors
    /// Unknown/ambiguous ownership, read-only shadowing, unavailable controls,
    /// transport, or invalid wire data.
    pub async fn logout(&self, provider: Option<&str>) -> Result<AppLogoutResult, AppServerError> {
        self.typed_call(
            "authorization/logout",
            serde_json::json!({"provider":provider}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read current routing plus provider-owned profiles.
    ///
    /// # Errors
    /// Missing controls, unavailable routing, transport, or invalid wire data.
    pub async fn providers(&self) -> Result<AppProviderCatalog, AppServerError> {
        self.typed_call(
            "providers/list",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Persist one provider and its provider-owned default model.
    ///
    /// # Errors
    /// Unknown provider, Settings failure, unavailable controls, or transport failure.
    pub async fn select_provider(
        &self,
        provider: &str,
    ) -> Result<AppRouteSelection, AppServerError> {
        self.typed_call(
            "providers/select",
            serde_json::json!({"provider":provider}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read or refresh one provider model catalog.
    ///
    /// # Errors
    /// Invalid provider, cancellation, unavailable controls, transport, or invalid wire data.
    pub async fn models(
        &self,
        provider: Option<&str>,
        refresh: AppCatalogRefresh,
    ) -> Result<AppModelCatalog, AppServerError> {
        self.typed_call(
            "models/list",
            serde_json::json!({"provider":provider,"refresh":refresh}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Persist a catalog-proven model for the current provider.
    ///
    /// # Errors
    /// Unselectable model, Settings failure, unavailable controls, or transport failure.
    pub async fn select_model(&self, model: &str) -> Result<AppRouteSelection, AppServerError> {
        self.typed_call(
            "models/select",
            serde_json::json!({"model":model}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read current routing plus every registered agent runtime.
    ///
    /// # Errors
    /// Missing controls, unavailable registry, transport, or invalid wire data.
    pub async fn runtimes(&self) -> Result<AppRuntimeCatalog, AppServerError> {
        self.typed_call(
            "runtimes/list",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Persist one agent runtime as the effective top-level loop owner.
    ///
    /// The next runtime session opens against it. A turn already running keeps
    /// the runtime it started with, so this is refused mid-turn rather than
    /// acknowledged.
    ///
    /// # Errors
    /// Unknown/inadmissible runtime, a durable session already linked to a
    /// different runtime, a turn in flight, Settings failure, unavailable
    /// controls, or transport failure.
    pub async fn select_runtime(&self, runtime: &str) -> Result<AppRouteSelection, AppServerError> {
        self.typed_call(
            "runtime/select",
            serde_json::json!({"runtime":runtime}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Select the absolute workspace every later runtime session opens against.
    ///
    /// The path is canonicalized and must be an existing directory. Only a
    /// delegated runtime can be relocated; the native loop's tool roots belong
    /// to the host composition, so selecting a different workspace under it is
    /// refused with [`AppServerErrorCode::Unsupported`] rather than stored and
    /// ignored.
    ///
    /// # Errors
    /// Non-absolute/traversing/missing path, a non-relocatable runtime, a turn
    /// in flight, unavailable controls, or transport failure.
    pub async fn select_workspace(
        &self,
        cwd: &std::path::Path,
    ) -> Result<AppWorkspaceSelection, AppServerError> {
        self.typed_call(
            "workspace/select",
            serde_json::json!({"cwd":cwd}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read the registry-owned redacted MCP snapshot.
    ///
    /// # Errors
    /// Missing/unavailable controls, transport, or invalid wire data.
    pub async fn mcp(&self) -> Result<Value, AppServerError> {
        self.call(
            "mcp/list",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read exact live plugin/contribution ownership.
    ///
    /// # Errors
    /// Missing/unavailable controls, transport, or invalid wire data.
    pub async fn plugins(&self) -> Result<AppPluginInventory, AppServerError> {
        self.typed_call(
            "plugins/list",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read every settings namespace under fail-closed exposure.
    ///
    /// # Errors
    /// Missing/unavailable controls, transport, or invalid wire data.
    pub async fn settings(&self) -> Result<Vec<AppSettingsSnapshot>, AppServerError> {
        self.typed_call(
            "settings/list",
            serde_json::json!({}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Read one settings namespace under fail-closed exposure.
    ///
    /// # Errors
    /// Invalid/unknown namespace, unavailable controls, transport, or invalid wire data.
    pub async fn setting(&self, namespace: &str) -> Result<AppSettingsSnapshot, AppServerError> {
        self.typed_call(
            "settings/get",
            serde_json::json!({"namespace":namespace}),
            None,
            CancellationToken::new(),
        )
        .await
    }

    /// Durably replace one wire-exposed user section with CAS.
    ///
    /// # Errors
    /// Invalid/unexposed namespace, revision conflict, persistence/validation,
    /// unavailable controls, transport, or invalid wire data.
    pub async fn replace_setting(
        &self,
        namespace: &str,
        user: Value,
        expected_revision: u64,
    ) -> Result<AppSettingsSnapshot, AppServerError> {
        self.typed_call(
            "settings/replace",
            serde_json::json!({
                "namespace":namespace,
                "user":user,
                "expectedRevision":expected_revision,
            }),
            None,
            CancellationToken::new(),
        )
        .await
    }

    async fn typed_call<R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: Value,
        events: Option<mpsc::Sender<AppServerNotification>>,
        cancellation: CancellationToken,
    ) -> Result<R, AppServerError> {
        let value = self.call(method, params, events, cancellation).await?;
        serde_json::from_value(value).map_err(|_| invalid())
    }

    async fn call(
        &self,
        method: &str,
        params: Value,
        events: Option<mpsc::Sender<AppServerNotification>>,
        cancellation: CancellationToken,
    ) -> Result<Value, AppServerError> {
        let id = self.next_request_id()?;
        let request = serde_json::json!({
            "jsonrpc":"2.0",
            "id":id,
            "method":method,
            "params":params,
        })
        .to_string();
        if request.len() > MAX_WIRE_BYTES {
            return Err(invalid());
        }
        let (raw_tx, mut raw_rx) = mpsc::channel(32);
        let exchange = self
            .transport
            .exchange(request, raw_tx, cancellation.clone());
        tokio::pin!(exchange);
        let mut first_sequence = true;
        let mut last_sequence = 0_u64;
        let mut notifications_open = true;
        let response = loop {
            tokio::select! {
                biased;
                maybe_raw = raw_rx.recv(), if notifications_open => {
                    match maybe_raw {
                        Some(raw) => self.forward_notification(
                            &raw,
                            events.as_ref(),
                            &mut first_sequence,
                            &mut last_sequence,
                        ).await?,
                        None => notifications_open = false,
                    }
                }
                result = &mut exchange => {
                    let response = result?;
                    while let Ok(raw) = raw_rx.try_recv() {
                        self.forward_notification(
                            &raw,
                            events.as_ref(),
                            &mut first_sequence,
                            &mut last_sequence,
                        ).await?;
                    }
                    break response;
                }
            }
        };
        parse_response(&response, id)
    }

    async fn forward_notification(
        &self,
        raw: &str,
        events: Option<&mpsc::Sender<AppServerNotification>>,
        first_sequence: &mut bool,
        last_sequence: &mut u64,
    ) -> Result<(), AppServerError> {
        if raw.is_empty() || raw.len() > MAX_WIRE_BYTES {
            return Err(invalid());
        }
        let notification =
            serde_json::from_str::<AppServerNotification>(raw).map_err(|_| invalid())?;
        if notification.jsonrpc != "2.0" {
            return Err(invalid());
        }
        match notification.method.as_str() {
            "session/event" => {
                let expected = self
                    .session
                    .lock()
                    .await
                    .as_ref()
                    .map(|session| session.session_id.clone())
                    .ok_or_else(invalid)?;
                if notification.params.session_id.as_deref() != Some(expected.as_str()) {
                    return Err(invalid());
                }
            }
            "control/event" if notification.params.session_id.is_none() => {}
            _ => return Err(invalid()),
        }
        if !*first_sequence && notification.params.sequence != last_sequence.saturating_add(1) {
            return Err(invalid());
        }
        *first_sequence = false;
        *last_sequence = notification.params.sequence;
        if let Some(events) = events {
            let _sent = events.send(notification).await;
        }
        Ok(())
    }

    fn next_request_id(&self) -> Result<u64, AppServerError> {
        self.next_id
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |id| id.checked_add(1))
            .map_err(|_| AppServerError::classified(AppServerErrorCode::Internal))
    }
}

fn parse_response(response: &str, expected_id: u64) -> Result<Value, AppServerError> {
    if response.is_empty() || response.len() > MAX_WIRE_BYTES {
        return Err(invalid());
    }
    let value = serde_json::from_str::<Value>(response).map_err(|_| invalid())?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id").and_then(Value::as_u64) != Some(expected_id)
    {
        return Err(invalid());
    }
    if let Some(error) = value.get("error") {
        let code = error.get("code").and_then(Value::as_i64);
        let classified = AppServerError::classified(match code {
            Some(-32602) => AppServerErrorCode::InvalidRequest,
            Some(-32601) => AppServerErrorCode::MethodNotFound,
            Some(-32800) => AppServerErrorCode::Cancelled,
            Some(-32001) => AppServerErrorCode::Conflict,
            Some(-32003) => AppServerErrorCode::Unsupported,
            Some(-32004) => AppServerErrorCode::Closed,
            Some(-32603) => AppServerErrorCode::Internal,
            _ => AppServerErrorCode::Unavailable,
        });
        // A detail the server did not declare safe by our rules is dropped,
        // never trusted: `with_detail` re-validates it.
        let detailed = error
            .get("data")
            .and_then(|data| data.get("detail"))
            .and_then(Value::as_str)
            .map_or_else(
                || classified.clone(),
                |detail| {
                    classified
                        .clone()
                        .with_detail(detail)
                        .unwrap_or(classified.clone())
                },
            );
        return Err(detailed);
    }
    value.get("result").cloned().ok_or_else(invalid)
}

const fn invalid() -> AppServerError {
    AppServerError::classified(AppServerErrorCode::InvalidRequest)
}
