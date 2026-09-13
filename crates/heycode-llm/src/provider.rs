//! The provider seam: one trait every LLM backend implements.

use std::sync::Arc;

use crate::error::LlmError;
use crate::vocab::{ChatRequest, StreamChunk};
use tokio_util::sync::CancellationToken;

/// Item stream of a streaming completion: vocabulary chunks in order, or
/// layer errors. Well-formed streams end `Usage?, Finish` with nothing after.
pub type ChunkStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<StreamChunk, LlmError>> + Send>>;

/// Stable identity of a provider for routing and defaults.
#[derive(Debug, Clone)]
pub struct ProviderInfo {
    /// Registry-unique provider name (the spec-table name).
    pub name: String,
    /// Model used when the caller has resolved no explicit model.
    pub default_model: String,
}

/// Exact selected-model and native-route facts used to materialize provider
/// request options for one request.
///
/// This context is deliberately read-only. N01 selects routes before this
/// hook, the provider derives only its own secret-free options, and P10 may
/// still inspect or revise the resulting durable draft before resolution.
#[derive(Debug, Clone, Copy)]
pub struct ProviderOptionContext<'a> {
    model: &'a crate::ModelDescriptor,
    native_tool_routes: &'a [heycode_core::NativeToolRoute],
}

impl<'a> ProviderOptionContext<'a> {
    /// Bind one catalog-selected canonical model and the exact N01 route set.
    #[must_use]
    pub const fn new(
        model: &'a crate::ModelDescriptor,
        native_tool_routes: &'a [heycode_core::NativeToolRoute],
    ) -> Self {
        Self {
            model,
            native_tool_routes,
        }
    }

    /// Canonical selected model descriptor for this request.
    #[must_use]
    pub const fn model(self) -> &'a crate::ModelDescriptor {
        self.model
    }

    /// Exact already-selected native-tool routes in durable order.
    #[must_use]
    pub const fn native_tool_routes(self) -> &'a [heycode_core::NativeToolRoute] {
        self.native_tool_routes
    }
}

/// A chat-completion backend. Implementations translate the neutral
/// vocabulary to their wire protocol and back; the request's `model` is
/// authoritative and must reach the wire unchanged.
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
    /// Replay attestation for a legacy request before any output. Strict adapters
    /// use their resolved RetrySpec instead. The conservative default prevents
    /// changing routes after a request whose external effects are unknown.
    fn fallback_safety(&self, _request: &ChatRequest) -> crate::RetrySafety {
        crate::RetrySafety::Never
    }

    /// Identity of this provider instance.
    fn info(&self) -> ProviderInfo;

    /// Optional non-secret credential reference used by setup/connect
    /// consumers. Provider implementations own this metadata; callers never
    /// infer it from the provider id.
    fn credential_reference(&self) -> Option<&str> {
        None
    }

    /// Response headers this provider publishes rate-limit facts in.
    ///
    /// There is no cross-provider standard beyond `Retry-After`, so each
    /// provider declares its own spelling and the default declares **nothing**.
    /// Declaring names that were not verified against the provider would make
    /// `/usage` display a guess, which is exactly what P12 forbids — reporting
    /// no window is the correct answer where no evidence exists.
    fn rate_limit_headers(&self) -> crate::RateLimitHeaders {
        crate::RateLimitHeaders::none()
    }

    /// Safe provider descriptor. Adapters override when protocol evidence is
    /// known; the default preserves identity and reports protocol unknown.
    fn descriptor(&self) -> crate::ProviderDescriptor {
        let info = self.info();
        crate::ProviderDescriptor {
            id: info.name.to_owned(),
            display_name: info.name.to_owned(),
            protocols: vec![crate::ProviderProtocol::Unknown],
        }
    }

    /// Describe one model. Live/catalog-backed adapters override; unknown is
    /// conservative and never invents capability support.
    fn describe_model(&self, model: &str) -> crate::ModelDescriptor {
        crate::ModelDescriptor::unknown(model)
    }

    /// Native resolved-call adapter for production dispatch, when this Provider
    /// has exact model/catalog evidence for the new boundary. Once present,
    /// Consumers must fail loud instead of falling back to [`Self::stream`].
    fn inference_adapter(&self) -> Option<&dyn crate::InferenceAdapter> {
        None
    }

    /// Prepare one exact provider instance after catalog/model/native-route
    /// selection and before request options, adapter resolution, P10 or the
    /// durable C02/C05 request commit.
    ///
    /// Most providers are ready at composition and return `None`. A provider
    /// whose endpoint or account-scoped route requires asynchronous discovery
    /// returns a prepared operation instance. The caller owns `cancellation`
    /// and must await this future on every outcome; implementations must not
    /// spawn or retain work beyond it.
    ///
    /// # Errors
    /// Cancellation, unavailable account metadata, route drift or preparation
    /// failure aborts before any request header or provider transport exists.
    async fn prepare_inference(
        &self,
        _context: ProviderOptionContext<'_>,
        cancellation: CancellationToken,
    ) -> Result<Option<Arc<dyn Provider>>, LlmError> {
        if cancellation.is_cancelled() {
            return Err(LlmError::Provider(crate::ProviderFailure::new(
                crate::ProviderErrorClass::Cancelled,
                crate::ProviderFailureOrigin::Local,
            )));
        }
        Ok(None)
    }

    /// Hidden experimental audio adapter for an exact evidenced model/format.
    ///
    /// This is deliberately separate from [`Self::inference_adapter`]. An
    /// implementation that does not opt in advertises no audio path, and no
    /// protocol or catalog capability is inferred upward from this default.
    fn experimental_audio_adapter(&self) -> Option<&dyn crate::ExperimentalAudioAdapter> {
        None
    }

    /// Provider-owned, secret-free options that must be committed in the
    /// resolved request before native adapter dispatch.
    fn request_options(&self) -> Vec<heycode_core::ProviderRequestOption> {
        Vec::new()
    }

    /// Materialize provider-owned options for one exact model/route selection.
    ///
    /// Providers whose policy is request-invariant inherit the legacy static
    /// options. Providers that expose N01-selected native tools or model-bound
    /// controls override this method and must derive only from `context`; they
    /// must not enable a complete provider tool set merely because it was
    /// configured at composition time.
    ///
    /// # Errors
    /// Unsupported, unproven or structurally invalid selected-model policy
    /// fails before a request header or transport operation exists.
    fn request_options_for(
        &self,
        _context: ProviderOptionContext<'_>,
    ) -> Result<Vec<heycode_core::ProviderRequestOption>, crate::ResolveError> {
        Ok(self.request_options())
    }

    /// Stream one completion. Errors surface as stream items, never as a
    /// failed call: the returned value is always a live stream.
    fn stream(&self, request: ChatRequest) -> ChunkStream;
}
