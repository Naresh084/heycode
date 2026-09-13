//! P08 production activation of provider-advertised inference adapters.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt as _;
use heycode_agent::{
    AgentOptions, AutoApprove, agent_options_plugin, agent_plugin, approval_plugin,
    commands_plugin, native_runtime_plugin,
};
use heycode_core::{
    CallId, Layer, NativeToolImplementationKind, NativeToolRoute, Next, Plugin, ProviderStateItem,
    ProviderStateKind, ServerToolCall, ServerToolResult, ServerToolSource, UrlCitation,
};
use heycode_llm::{
    AdapterOwnedAuth, AuthenticationBinding, CapabilitySupport, CatalogFetchError,
    CatalogRefreshMode, CatalogRegistry, ChatRequest, ChunkStream, ExperimentalAudioAdapter,
    ExperimentalAudioDescriptor, ExperimentalAudioEvent, ExperimentalAudioOutput,
    ExperimentalAudioStream, InferenceAdapter, InferenceEvent, InferenceInput, InferenceStream,
    InferenceTarget, LlmSelection, ModelCapabilities, ModelCatalog, ModelDescriptor,
    ModelLifecycle, NativeCompactionAdapter, NativeCompactionCheckpoint, NativeCompactionError,
    NativeCompactionFuture, Provider, ProviderDescriptor, ProviderErrorClass, ProviderFailure,
    ProviderFailureOrigin, ProviderInfo, ProviderInterception, ProviderInterceptionCode,
    ProviderOptionContext, ProviderProtocol, ProviderRequestDecision, ProviderResponseDecision,
    ReasoningEffortId, RequestDraft, ResolveError, ResolveSpec, ResolvedCall,
    ResolvedExperimentalAudioCall, StreamChunk, llm_plugin, model_catalog_plugin, resolve_request,
};
use heycode_prompt::prompt_plugin;
use heycode_runtime::{
    RuntimeConfiguration, RuntimeErrorCode, RuntimeInput, RuntimeResume, RuntimeStart,
    runtime_registry_plugin,
};
use heycode_session::{Session, SessionEventKind, session_plugin};
use heycode_tools::tools_plugin;
use image::ImageEncoder as _;
use tokio_util::sync::CancellationToken;

const PROVIDER: &str = "adapter-test";
const MODEL: &str = "adapter-test/model";

fn descriptor() -> ProviderDescriptor {
    ProviderDescriptor {
        id: PROVIDER.to_owned(),
        display_name: "Adapter Test".to_owned(),
        protocols: vec![ProviderProtocol::OpenAiChatCompletions],
    }
}

fn model(image_input: CapabilitySupport, document_input: CapabilitySupport) -> ModelDescriptor {
    ModelDescriptor {
        pricing: heycode_llm::ModelPricing::unknown(),
        performance: heycode_llm::ModelPerformance::unknown(),
        id: MODEL.to_owned(),
        display_name: "Adapter Model".to_owned(),
        aliases: vec!["adapter-alias".to_owned()],
        created_at_ms: None,
        context_window: Some(64_000),
        max_output_tokens: Some(4_096),
        lifecycle: ModelLifecycle::stable(),
        capabilities: ModelCapabilities {
            tools: CapabilitySupport::Supported,
            reasoning: CapabilitySupport::Supported,
            native_web: CapabilitySupport::Supported,
            image_input,
            document_input,
            native_compaction: CapabilitySupport::Supported,
            ..ModelCapabilities::unknown()
        },
        reasoning: None,
    }
}

struct StaticCatalog {
    image_input: CapabilitySupport,
    document_input: CapabilitySupport,
}

#[async_trait]
impl ModelCatalog for StaticCatalog {
    fn provider(&self) -> ProviderDescriptor {
        descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        if cancellation.is_cancelled() {
            return Err(CatalogFetchError::cancelled());
        }
        Ok(vec![model(self.image_input, self.document_input)])
    }
}

struct CancellationCatalog {
    started: Arc<tokio::sync::Notify>,
    release: Arc<tokio::sync::Notify>,
    settled: Arc<AtomicUsize>,
}

#[async_trait]
impl ModelCatalog for CancellationCatalog {
    fn provider(&self) -> ProviderDescriptor {
        descriptor()
    }

    async fn fetch(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Vec<ModelDescriptor>, CatalogFetchError> {
        self.started.notify_one();
        let result = tokio::select! {
            () = cancellation.cancelled() => Err(CatalogFetchError::cancelled()),
            () = self.release.notified() => Ok(vec![model(
                CapabilitySupport::Supported,
                CapabilitySupport::Unsupported,
            )]),
        };
        self.settled.fetch_add(1, Ordering::SeqCst);
        result
    }
}

struct AdapterProvider {
    behavior: AdapterBehavior,
    prepared_provider: Option<Arc<AdapterProvider>>,
    preparation_calls: AtomicUsize,
    preparation_contexts: Mutex<Vec<(String, Vec<NativeToolRoute>)>>,
    audio_descriptor: Option<ExperimentalAudioDescriptor>,
    legacy_calls: AtomicUsize,
    adapter_calls: AtomicUsize,
    drafts: Mutex<Vec<RequestDraft>>,
    audio_calls: Mutex<Vec<Vec<Vec<u8>>>>,
    operation_tokens: Mutex<Vec<CancellationToken>>,
    option_contexts: Mutex<Vec<(String, Vec<NativeToolRoute>)>>,
    cancel_on_resolve: Mutex<Option<CancellationToken>>,
    started: tokio::sync::Notify,
}

#[derive(Clone, Copy)]
enum AdapterBehavior {
    Reply,
    PreparedReply,
    AuthDrift,
    Hang,
    Reject,
    StreamError,
    CancelDuringResolve,
    PauseThenReply,
    AlwaysPause,
    ManyPauses,
    OrphanServerResult,
    ContextualOptions,
    AudioReply,
    AudioOutput,
    AudioHang,
    AudioUnsupported,
    AudioUnproven,
}

impl AdapterProvider {
    fn new(behavior: AdapterBehavior) -> Self {
        let prepared_provider = matches!(behavior, AdapterBehavior::PreparedReply)
            .then(|| Arc::new(Self::new(AdapterBehavior::Reply)));
        Self {
            behavior,
            prepared_provider,
            preparation_calls: AtomicUsize::new(0),
            preparation_contexts: Mutex::new(Vec::new()),
            audio_descriptor: audio_descriptor_for(behavior),
            legacy_calls: AtomicUsize::new(0),
            adapter_calls: AtomicUsize::new(0),
            drafts: Mutex::new(Vec::new()),
            audio_calls: Mutex::new(Vec::new()),
            operation_tokens: Mutex::new(Vec::new()),
            option_contexts: Mutex::new(Vec::new()),
            cancel_on_resolve: Mutex::new(None),
            started: tokio::sync::Notify::new(),
        }
    }

    fn spec() -> ResolveSpec {
        ResolveSpec {
            protocol: ProviderProtocol::OpenAiChatCompletions,
            target: InferenceTarget::Http {
                base_url: "https://adapter.test/v1".to_owned(),
            },
            authentication: AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
            default_max_output_tokens: None,
            reasoning_efforts: vec![
                ReasoningEffortId::new("low").unwrap(),
                ReasoningEffortId::new("high").unwrap(),
            ],
            default_reasoning_effort: None,
        }
    }

    fn response_state(text: &str) -> ProviderStateItem {
        ProviderStateItem::new(
            PROVIDER,
            MODEL,
            ProviderProtocol::OpenAiChatCompletions,
            ProviderStateKind::ChatAssistantMessage,
            serde_json::json!({"role":"assistant","content":text}),
        )
        .unwrap()
    }
}

#[tokio::test]
async fn selected_reasoning_effort_reaches_draft_and_durable_request_header() {
    let world = world(AdapterBehavior::Reply);
    let selection = world.agent.selection();
    world.agent.set_inference_route(
        selection.provider_name,
        selection.model,
        Some(ReasoningEffortId::new("high").unwrap()),
    );

    world.agent.send("use high effort").await.unwrap();

    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(
        drafts[0]
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffortId::as_str),
        Some("high")
    );
    drop(drafts);
    let requests =
        heycode_session::project_requests(world.session.lock().unwrap().events()).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header.options.reasoning_effort.as_deref(),
        Some("high")
    );
    assert!(!requests[0].header.options.defaulted_reasoning_effort);
}

fn audio_descriptor_for(behavior: AdapterBehavior) -> Option<ExperimentalAudioDescriptor> {
    let input = match behavior {
        AdapterBehavior::AudioUnsupported => CapabilitySupport::Unsupported,
        AdapterBehavior::AudioUnproven => CapabilitySupport::Unknown,
        AdapterBehavior::AudioReply | AdapterBehavior::AudioOutput | AdapterBehavior::AudioHang => {
            CapabilitySupport::Supported
        }
        _ => return None,
    };
    let output = if matches!(
        behavior,
        AdapterBehavior::AudioOutput | AdapterBehavior::AudioHang
    ) {
        CapabilitySupport::Supported
    } else {
        CapabilitySupport::Unknown
    };
    Some(
        ExperimentalAudioDescriptor::new(
            PROVIDER,
            MODEL,
            ProviderProtocol::OpenAiChatCompletions,
            InferenceTarget::Http {
                base_url: "https://adapter.test/v1".to_owned(),
            },
            AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new()),
        )
        .unwrap()
        .with_input(input, ["audio/wav"], 4, 32 * 1024 * 1024)
        .unwrap()
        .with_output(output, ["audio/wav"])
        .unwrap(),
    )
}

#[async_trait]
impl Provider for AdapterProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: PROVIDER.to_owned(),
            default_model: MODEL.to_owned(),
        }
    }

    fn descriptor(&self) -> ProviderDescriptor {
        descriptor()
    }

    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }

    async fn prepare_inference(
        &self,
        context: ProviderOptionContext<'_>,
        cancellation: CancellationToken,
    ) -> Result<Option<Arc<dyn Provider>>, heycode_llm::LlmError> {
        if !matches!(self.behavior, AdapterBehavior::PreparedReply) {
            return Ok(None);
        }
        if cancellation.is_cancelled() {
            return Err(heycode_llm::LlmError::Provider(ProviderFailure::new(
                ProviderErrorClass::Cancelled,
                ProviderFailureOrigin::Local,
            )));
        }
        self.preparation_calls.fetch_add(1, Ordering::SeqCst);
        self.preparation_contexts.lock().unwrap().push((
            context.model().id.clone(),
            context.native_tool_routes().to_vec(),
        ));
        Ok(self
            .prepared_provider
            .as_ref()
            .map(|provider| provider.clone() as Arc<dyn Provider>))
    }

    fn experimental_audio_adapter(&self) -> Option<&dyn ExperimentalAudioAdapter> {
        self.audio_descriptor
            .as_ref()
            .map(|_| self as &dyn ExperimentalAudioAdapter)
    }

    fn request_options_for(
        &self,
        context: ProviderOptionContext<'_>,
    ) -> Result<Vec<heycode_core::ProviderRequestOption>, ResolveError> {
        if !matches!(self.behavior, AdapterBehavior::ContextualOptions) {
            return Ok(Vec::new());
        }
        self.option_contexts.lock().unwrap().push((
            context.model().id.clone(),
            context.native_tool_routes().to_vec(),
        ));
        Ok(vec![heycode_core::ProviderRequestOption::new(
            PROVIDER,
            "contextual",
            serde_json::json!({
                "model":context.model().id,
                "routes":context.native_tool_routes().iter().map(|route| route.implementation()).collect::<Vec<_>>()
            }),
        )
        .unwrap()])
    }

    fn stream(&self, _request: ChatRequest) -> ChunkStream {
        self.legacy_calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::iter([
            Ok(StreamChunk::TextDelta("legacy".to_owned())),
            Ok(StreamChunk::Finish(heycode_llm::FinishReason::Stop)),
        ]))
    }
}

impl ExperimentalAudioAdapter for AdapterProvider {
    fn descriptor(&self) -> &ExperimentalAudioDescriptor {
        self.audio_descriptor.as_ref().unwrap()
    }

    fn stream(&self, call: ResolvedExperimentalAudioCall) -> ExperimentalAudioStream {
        self.audio_calls.lock().unwrap().push(
            call.inputs()
                .iter()
                .map(|input| input.bytes().to_vec())
                .collect(),
        );
        self.adapter_calls.fetch_add(1, Ordering::SeqCst);
        self.started.notify_one();
        let output = ExperimentalAudioEvent::AudioOutput(
            ExperimentalAudioOutput::new(
                heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
                heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
                pcm_wav(8_000, 1, 8_000, b"RAW-ASSISTANT-AUDIO"),
            )
            .unwrap(),
        );
        if matches!(self.behavior, AdapterBehavior::AudioHang) {
            return Box::pin(futures::stream::iter([Ok(output)]).chain(futures::stream::pending()));
        }
        let mut events = vec![Ok(ExperimentalAudioEvent::TextDelta(
            "audio-adapter".to_owned(),
        ))];
        if matches!(self.behavior, AdapterBehavior::AudioOutput) {
            events.push(Ok(output));
        }
        events.push(Ok(ExperimentalAudioEvent::Usage(
            heycode_core::TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 2,
            },
        )));
        events.push(Ok(ExperimentalAudioEvent::Finish(
            heycode_llm::FinishReason::Stop,
        )));
        Box::pin(futures::stream::iter(events))
    }
}

fn pcm_wav(sample_rate_hz: u32, channels: u16, frames: u32, marker: &[u8]) -> Vec<u8> {
    let bits_per_sample = 16_u16;
    let block_align = channels * (bits_per_sample / 8);
    let data_len = frames * u32::from(block_align);
    let mut data = vec![0_u8; usize::try_from(data_len).unwrap()];
    let copied = marker.len().min(data.len());
    data[..copied].copy_from_slice(&marker[..copied]);
    let mut bytes = Vec::with_capacity(usize::try_from(data_len + 44).unwrap());
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36_u32 + data_len).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&channels.to_le_bytes());
    bytes.extend_from_slice(&sample_rate_hz.to_le_bytes());
    bytes.extend_from_slice(&(sample_rate_hz * u32::from(block_align)).to_le_bytes());
    bytes.extend_from_slice(&block_align.to_le_bytes());
    bytes.extend_from_slice(&bits_per_sample.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&data_len.to_le_bytes());
    bytes.extend_from_slice(&data);
    bytes
}

impl InferenceAdapter for AdapterProvider {
    fn reasoning_effort_options(
        &self,
        _model: &ModelDescriptor,
    ) -> Result<Option<heycode_llm::ReasoningEffortOptions>, ResolveError> {
        let spec = Self::spec();
        heycode_llm::ReasoningEffortOptions::new(
            spec.reasoning_efforts,
            spec.default_reasoning_effort,
        )
        .map(Some)
    }

    fn descriptor(&self) -> ProviderDescriptor {
        descriptor()
    }

    fn authentication_binding(&self) -> AuthenticationBinding {
        if matches!(self.behavior, AdapterBehavior::AuthDrift) {
            AuthenticationBinding::None
        } else {
            AuthenticationBinding::AdapterOwned(AdapterOwnedAuth::new())
        }
    }

    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        self.drafts.lock().unwrap().push(draft.clone());
        if matches!(self.behavior, AdapterBehavior::CancelDuringResolve)
            && let Some(cancellation) = self.cancel_on_resolve.lock().unwrap().take()
        {
            cancellation.cancel();
        }
        if matches!(self.behavior, AdapterBehavior::Reject) {
            return Err(ResolveError::InvalidRequest {
                field: "test",
                message: "test adapter rejected the request".to_owned(),
            });
        }
        if matches!(self.behavior, AdapterBehavior::PreparedReply) {
            // Mirrors the shipped `LazyVertexProvider`: the instance held in
            // the registry advertises an adapter but is inert, so ONLY the
            // provider `prepare_inference` hands back may resolve. Without
            // this the harness cannot notice a caller that skipped
            // preparation, because resolving the unprepared instance works.
            return Err(ResolveError::InvalidAdapter {
                field: "prepare_inference",
                message: "lazy provider must be prepared before resolution".to_owned(),
            });
        }
        resolve_request(&descriptor(), draft, model, &Self::spec())
    }

    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        InferenceAdapter::stream_cancellable(self, call, CancellationToken::new())
    }

    fn stream_cancellable(
        &self,
        resolved: ResolvedCall,
        cancellation: CancellationToken,
    ) -> InferenceStream {
        let call = self.adapter_calls.fetch_add(1, Ordering::SeqCst) + 1;
        self.operation_tokens.lock().unwrap().push(cancellation);
        self.started.notify_one();
        if resolved.purpose() == heycode_llm::CallPurpose::Compaction {
            // A summarization request answers in plain text: the portable
            // strategy refuses provider state or tool output as a summary.
            return Box::pin(futures::stream::iter([
                Ok(InferenceEvent::TextDelta(format!("summary-{call}"))),
                Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Stop)),
            ]));
        }
        if matches!(self.behavior, AdapterBehavior::Hang) {
            return Box::pin(futures::stream::pending());
        }
        if matches!(self.behavior, AdapterBehavior::StreamError) {
            return Box::pin(futures::stream::iter([
                Ok(InferenceEvent::ServerToolCall {
                    output_index: 0,
                    call: ServerToolCall::new(
                        CallId::from_raw("srvtoolu_uncommitted"),
                        "web_search",
                        "web_search",
                        serde_json::json!({"query":"must not commit"}),
                    )
                    .unwrap(),
                }),
                Ok(InferenceEvent::ProviderState(Self::response_state(
                    "must-not-commit",
                ))),
                Err(heycode_llm::LlmError::Provider(ProviderFailure::new(
                    ProviderErrorClass::Server,
                    ProviderFailureOrigin::ProviderEvent,
                ))),
            ]));
        }
        if matches!(self.behavior, AdapterBehavior::PauseThenReply) && call == 1 {
            return Box::pin(futures::stream::iter([
                Ok(InferenceEvent::ResponseStarted {
                    response_id: "response-paused".to_owned(),
                }),
                Ok(InferenceEvent::ServerToolCall {
                    output_index: 0,
                    call: ServerToolCall::new(
                        CallId::from_raw("srvtoolu_1"),
                        "web_search",
                        "web_search",
                        serde_json::json!({"query":"rust"}),
                    )
                    .unwrap(),
                }),
                Ok(InferenceEvent::ProviderState(Self::response_state(
                    "paused-state",
                ))),
                Ok(InferenceEvent::Usage(heycode_core::TokenUsage {
                    prompt_tokens: 5,
                    completion_tokens: 1,
                })),
                Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Pause)),
            ]));
        }
        if matches!(self.behavior, AdapterBehavior::PauseThenReply) {
            return Box::pin(futures::stream::iter([
                Ok(InferenceEvent::ResponseStarted {
                    response_id: "response-continued".to_owned(),
                }),
                Ok(InferenceEvent::ServerToolResult {
                    output_index: 0,
                    result: ServerToolResult::success(
                        CallId::from_raw("srvtoolu_1"),
                        Some(1),
                        vec![
                            ServerToolSource::new("https://example.test/rust", Some("Rust"))
                                .unwrap(),
                        ],
                    )
                    .unwrap(),
                }),
                Ok(InferenceEvent::Citation {
                    output_index: 1,
                    citation: UrlCitation::new(
                        "https://example.test/rust",
                        Some("Rust"),
                        Some("Rust source"),
                        None,
                        None,
                    )
                    .unwrap(),
                }),
                Ok(InferenceEvent::TextDelta("adapter-continued".to_owned())),
                Ok(InferenceEvent::ProviderState(Self::response_state(
                    "adapter-continued",
                ))),
                Ok(InferenceEvent::Usage(heycode_core::TokenUsage {
                    prompt_tokens: 9,
                    completion_tokens: 2,
                })),
                Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Stop)),
            ]));
        }
        if matches!(self.behavior, AdapterBehavior::AlwaysPause)
            || (matches!(self.behavior, AdapterBehavior::ManyPauses) && call <= 12)
        {
            let id = format!("srvtoolu_{call}");
            return Box::pin(futures::stream::iter([
                Ok(InferenceEvent::ServerToolCall {
                    output_index: 0,
                    call: ServerToolCall::new(
                        CallId::from_raw(id),
                        "web_search",
                        "web_search",
                        serde_json::json!({"query":"again"}),
                    )
                    .unwrap(),
                }),
                Ok(InferenceEvent::ProviderState(Self::response_state(
                    &format!("paused-{call}"),
                ))),
                Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Pause)),
            ]));
        }
        if matches!(self.behavior, AdapterBehavior::OrphanServerResult) {
            return Box::pin(futures::stream::iter([
                Ok(InferenceEvent::ServerToolResult {
                    output_index: 0,
                    result: ServerToolResult::success(
                        CallId::from_raw("srvtoolu_missing"),
                        Some(0),
                        Vec::new(),
                    )
                    .unwrap(),
                }),
                Ok(InferenceEvent::ProviderState(Self::response_state(
                    "must-not-commit",
                ))),
                Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Stop)),
            ]));
        }
        let text = format!("adapter-{call}");
        Box::pin(futures::stream::iter([
            Ok(InferenceEvent::ResponseStarted {
                response_id: format!("response-{call}"),
            }),
            Ok(InferenceEvent::TextDelta(text.clone())),
            Ok(InferenceEvent::ProviderState(Self::response_state(&text))),
            Ok(InferenceEvent::ResponseMetadata(
                heycode_core::ProviderResponseMetadata::new(
                    Some(heycode_core::ProviderCacheUsage::new(7, 2, 3, 1).unwrap()),
                    Vec::new(),
                    None,
                )
                .unwrap(),
            )),
            Ok(InferenceEvent::Usage(heycode_core::TokenUsage {
                prompt_tokens: 7,
                completion_tokens: 2,
            })),
            Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Stop)),
        ]))
    }

    fn native_compaction(&self) -> Option<&dyn NativeCompactionAdapter> {
        Some(self)
    }
}

impl NativeCompactionAdapter for AdapterProvider {
    fn compact(
        &self,
        call: ResolvedCall,
        cancellation: CancellationToken,
    ) -> NativeCompactionFuture<'_> {
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(NativeCompactionError::Cancelled);
            }
            if call.purpose() != heycode_llm::CallPurpose::Compaction
                || call.native_features() != [heycode_llm::NativeFeature::Compaction]
            {
                return Err(NativeCompactionError::InvalidCheckpoint);
            }
            NativeCompactionCheckpoint::new(
                vec![Self::response_state("native-checkpoint")],
                Some(heycode_core::TokenUsage {
                    prompt_tokens: 21,
                    completion_tokens: 1,
                }),
            )
        })
    }
}

struct World {
    agent: Arc<heycode_agent::Agent>,
    session: Arc<std::sync::Mutex<Session>>,
    provider: Arc<AdapterProvider>,
    catalogs: Arc<CatalogRegistry>,
    attachments: Arc<heycode_attachments::AttachmentStore>,
    commands: Arc<heycode_agent::CommandRegistry>,
    interception: Arc<ProviderInterception>,
    telemetry: Arc<heycode_telemetry::TelemetryService>,
    _context: heycode_core::Context,
    _root: tempfile::TempDir,
}

impl World {
    fn assert_no_open_records(&self) {
        let session = self
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let repair = heycode_session::project_repair(session.events());
        assert!(
            repair.is_clean(),
            "a handled adapter failure must close every started record: {:?}",
            repair.open()
        );
    }
}

fn native_runtime(world: &World) -> Arc<dyn heycode_runtime::AgentRuntime> {
    world
        ._context
        .get::<heycode_runtime::AgentRuntimeRegistry>(heycode_runtime::SERVICE_RUNTIMES)
        .unwrap()
        .get("native")
        .unwrap()
        .unwrap()
}

fn durable_session_id(world: &World) -> heycode_core::SessionId {
    world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .id()
        .clone()
}

#[tokio::test]
async fn native_runtime_applies_initial_resume_and_live_model_effort_exactly() {
    let world = world(AdapterBehavior::Reply);
    let runtime = native_runtime(&world);
    let controls = runtime.descriptor().configuration_capabilities();
    assert_eq!(controls.system_prompt, CapabilitySupport::Unsupported);
    assert_eq!(controls.tools, CapabilitySupport::Unsupported);
    assert!(controls.model.is_supported());
    assert!(controls.reasoning_effort.is_supported());

    let unsupported = RuntimeConfiguration::new()
        .with_system_prompt("caller-owned prompt")
        .unwrap();
    let rejected = match runtime
        .start(
            RuntimeStart::new(durable_session_id(&world), world._root.path())
                .unwrap()
                .with_configuration(unsupported),
            CancellationToken::new(),
        )
        .await
    {
        Ok(_) => panic!("native start must reject caller-owned system prompts"),
        Err(error) => error,
    };
    assert_eq!(rejected.code(), RuntimeErrorCode::Unsupported);
    assert_eq!(world.agent.selection().model, "adapter-alias");

    let initial = RuntimeConfiguration::new()
        .with_model(MODEL)
        .unwrap()
        .with_reasoning_effort("high")
        .unwrap();
    let session = runtime
        .start(
            RuntimeStart::new(durable_session_id(&world), world._root.path())
                .unwrap()
                .with_configuration(initial.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        session
            .configure(RuntimeConfiguration::new(), CancellationToken::new())
            .await
            .unwrap(),
        initial
    );
    session
        .send(
            RuntimeInput::new("initial configuration").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let resumed_configuration = RuntimeConfiguration::new()
        .with_model("adapter-alias")
        .unwrap()
        .with_reasoning_effort("low")
        .unwrap();
    let resumed = runtime
        .resume(
            RuntimeResume::new(
                durable_session_id(&world),
                world._root.path(),
                session.id().clone(),
            )
            .unwrap()
            .with_configuration(resumed_configuration.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        resumed
            .configure(RuntimeConfiguration::new(), CancellationToken::new())
            .await
            .unwrap(),
        resumed_configuration
    );
    resumed
        .send(
            RuntimeInput::new("resumed configuration").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let update = RuntimeConfiguration::new()
        .with_model(MODEL)
        .unwrap()
        .with_reasoning_effort("high")
        .unwrap();
    assert_eq!(
        resumed
            .configure(update.clone(), CancellationToken::new())
            .await
            .unwrap(),
        update
    );
    resumed
        .send(
            RuntimeInput::new("live configuration").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(drafts.len(), 3);
    assert_eq!(drafts[0].model, MODEL);
    assert_eq!(
        drafts[0]
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffortId::as_str),
        Some("high")
    );
    assert_eq!(drafts[1].model, "adapter-alias");
    assert_eq!(
        drafts[1]
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffortId::as_str),
        Some("low")
    );
    assert_eq!(drafts[2].model, MODEL);
    assert_eq!(
        drafts[2]
            .reasoning_effort
            .as_ref()
            .map(ReasoningEffortId::as_str),
        Some("high")
    );
}

#[tokio::test]
async fn native_runtime_rejects_unknown_effort_before_publishing_model_or_effort() {
    let world = world(AdapterBehavior::Reply);
    let runtime = native_runtime(&world);
    let initial = RuntimeConfiguration::new()
        .with_model(MODEL)
        .unwrap()
        .with_reasoning_effort("low")
        .unwrap();
    let session = runtime
        .start(
            RuntimeStart::new(durable_session_id(&world), world._root.path())
                .unwrap()
                .with_configuration(initial.clone()),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let update = RuntimeConfiguration::new()
        .with_model("adapter-alias")
        .unwrap()
        .with_reasoning_effort("unadvertised")
        .unwrap();
    let error = session
        .configure(update, CancellationToken::new())
        .await
        .unwrap_err();
    assert_eq!(error.code(), RuntimeErrorCode::Unsupported);
    assert_eq!(world.agent.selection().model, MODEL);
    assert_eq!(world.agent.reasoning_effort().unwrap().as_str(), "low");
    assert_eq!(
        session
            .configure(RuntimeConfiguration::new(), CancellationToken::new())
            .await
            .unwrap(),
        initial
    );
}

#[tokio::test]
async fn native_runtime_rejects_configuration_while_a_turn_is_active() {
    let world = world(AdapterBehavior::Hang);
    let runtime = native_runtime(&world);
    let session = runtime
        .start(
            RuntimeStart::new(durable_session_id(&world), world._root.path()).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let running = {
        let session = session.clone();
        tokio::spawn(async move {
            session
                .send(RuntimeInput::new("hang").unwrap(), CancellationToken::new())
                .await
        })
    };
    world.provider.started.notified().await;
    let error = session
        .configure(
            RuntimeConfiguration::new()
                .with_reasoning_effort("high")
                .unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), RuntimeErrorCode::Conflict);
    session.cancel(CancellationToken::new()).await.unwrap();
    assert_eq!(
        running.await.unwrap().unwrap_err().code(),
        RuntimeErrorCode::Cancelled
    );
}

#[tokio::test]
async fn provider_options_materialize_after_exact_model_and_native_route_selection() {
    let world = world(AdapterBehavior::ContextualOptions);
    let native = world
        ._context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    native
        .register(
            &world._context,
            heycode_native_tools::NativeToolImplementation::new(
                "web_search",
                "adapter-test:web_search",
                NativeToolImplementationKind::Provider,
                Some(PROVIDER.to_owned()),
                100,
            )
            .unwrap(),
        )
        .unwrap();

    world.agent.send("use the selected route").await.unwrap();

    let contexts = world.provider.option_contexts.lock().unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].0, MODEL);
    assert_eq!(contexts[0].1.len(), 2);
    let selected = contexts[0]
        .1
        .iter()
        .find(|route| route.logical() == "web_search")
        .unwrap();
    assert_eq!(selected.implementation(), "adapter-test:web_search");
    drop(contexts);
    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(drafts[0].provider_options.len(), 1);
    assert_eq!(drafts[0].provider_options[0].kind(), "contextual");
    assert_eq!(drafts[0].provider_options[0].data()["model"], MODEL);
    assert!(
        drafts[0].provider_options[0].data()["routes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|route| route == "adapter-test:web_search")
    );
}

#[tokio::test]
async fn provider_readiness_runs_after_model_and_native_selection_and_dispatches_prepared_route() {
    let world = world(AdapterBehavior::PreparedReply);
    let native = world
        ._context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    native
        .register(
            &world._context,
            heycode_native_tools::NativeToolImplementation::new(
                "web_search",
                "adapter-test:web_search",
                NativeToolImplementationKind::Provider,
                Some(PROVIDER.to_owned()),
                100,
            )
            .unwrap(),
        )
        .unwrap();

    let report = world.agent.send("prepare exact route").await.unwrap();

    assert_eq!(report.text, "adapter-1");
    assert_eq!(world.provider.preparation_calls.load(Ordering::SeqCst), 1);
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    let contexts = world.provider.preparation_contexts.lock().unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].0, MODEL);
    assert!(contexts[0].1.iter().any(|route| {
        route.logical() == "web_search" && route.implementation() == "adapter-test:web_search"
    }));
    drop(contexts);
    let prepared = world.provider.prepared_provider.as_ref().unwrap();
    assert_eq!(prepared.adapter_calls.load(Ordering::SeqCst), 1);
    assert_eq!(prepared.drafts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn provider_native_strategy_commits_one_checkpoint_and_exact_route_replays_it() {
    let world = world_with_capabilities(
        AdapterBehavior::Reply,
        CapabilitySupport::Unsupported,
        CapabilitySupport::Unsupported,
    );
    {
        let mut session = world.session.lock().unwrap();
        for (turn, user, assistant) in [
            (0, "old question", "old answer"),
            (1, "recent question", "recent answer"),
        ] {
            session
                .append(SessionEventKind::UserMessage {
                    text: user.to_owned(),
                })
                .unwrap();
            session
                .append(SessionEventKind::TurnStart { turn })
                .unwrap();
            session
                .append(SessionEventKind::AssistantMessage {
                    turn,
                    step: 0,
                    content: assistant.to_owned(),
                    reasoning: None,
                    tool_calls: None,
                    usage: None,
                })
                .unwrap();
            session
                .append(SessionEventKind::TurnEnd {
                    turn,
                    reason: heycode_session::TurnEndReason::Stop,
                })
                .unwrap();
        }
    }

    let outcome = world
        .agent
        .compact(
            heycode_agent::NativeCompaction::ID,
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        heycode_agent::CompactionOutcome::Applied { folded, .. } if folded > 0
    ));

    let session = world.session.lock().unwrap();
    let checkpoints = session
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::NativeCompactionApplied {
                strategy,
                items,
                usage,
                ..
            } => Some((strategy, items, usage)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(checkpoints.len(), 1);
    assert_eq!(checkpoints[0].0, heycode_agent::NativeCompaction::ID);
    assert_eq!(checkpoints[0].1[0].data()["content"], "native-checkpoint");
    assert_eq!(checkpoints[0].2.unwrap().prompt_tokens, 21);

    let inputs = heycode_session::project_inputs_for_route(
        session.events(),
        PROVIDER,
        MODEL,
        ProviderProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    assert!(matches!(
        &inputs[0],
        heycode_session::ProjectedInput::ProviderState(item)
            if item.data()["content"] == "native-checkpoint"
    ));
    assert!(inputs.iter().any(|input| matches!(
        input,
        heycode_session::ProjectedInput::Message(message)
            if message.content == "recent question"
    )));
    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(
        drafts.last().unwrap().purpose,
        heycode_llm::CallPurpose::Compaction
    );
    assert_eq!(
        drafts.last().unwrap().native_features,
        [heycode_llm::NativeFeature::Compaction]
    );
}

fn execution_plugin(cwd: &std::path::Path) -> Box<dyn Plugin> {
    heycode_exec::local_execution_plugin(
        heycode_exec::LocalShellConfig::platform(
            cwd.to_path_buf(),
            std::time::Duration::from_secs(30),
        )
        .unwrap(),
    )
}

fn world(behavior: AdapterBehavior) -> World {
    world_with_image_support(behavior, CapabilitySupport::Supported)
}

fn world_with_image_support(behavior: AdapterBehavior, image_support: CapabilitySupport) -> World {
    world_with_capabilities(behavior, image_support, CapabilitySupport::Unsupported)
}

fn world_with_document_support(
    behavior: AdapterBehavior,
    document_support: CapabilitySupport,
) -> World {
    world_with_capabilities(behavior, CapabilitySupport::Supported, document_support)
}

fn world_with_capabilities(
    behavior: AdapterBehavior,
    image_input: CapabilitySupport,
    document_input: CapabilitySupport,
) -> World {
    world_with_catalog(
        behavior,
        Arc::new(StaticCatalog {
            image_input,
            document_input,
        }),
    )
}

fn world_with_catalog(behavior: AdapterBehavior, catalog: Arc<dyn ModelCatalog>) -> World {
    world_with_catalog_and_budget(behavior, catalog, None)
}

fn world_with_budget(behavior: AdapterBehavior, policy: heycode_agent::LoopBudgetPolicy) -> World {
    world_with_catalog_and_budget(
        behavior,
        Arc::new(StaticCatalog {
            image_input: CapabilitySupport::Supported,
            document_input: CapabilitySupport::Unsupported,
        }),
        Some(policy),
    )
}

fn world_with_catalog_and_budget(
    behavior: AdapterBehavior,
    catalog: Arc<dyn ModelCatalog>,
    budget: Option<heycode_agent::LoopBudgetPolicy>,
) -> World {
    world_with_catalog_budget_and_subagents(behavior, catalog, budget, false)
}

fn world_with_catalog_budget_and_subagents(
    behavior: AdapterBehavior,
    catalog: Arc<dyn ModelCatalog>,
    budget: Option<heycode_agent::LoopBudgetPolicy>,
    subagents: bool,
) -> World {
    let root = tempfile::tempdir().unwrap();
    let provider = Arc::new(AdapterProvider::new(behavior));
    let mut plugins: Vec<Box<dyn Plugin>> = vec![
        session_plugin(root.path().to_path_buf()),
        heycode_attachments::local_attachment_plugin(
            heycode_attachments::AttachmentStoreConfig::new(
                root.path().join("attachments"),
                heycode_attachments::DEFAULT_MAX_ATTACHMENT_BYTES,
            )
            .unwrap(),
        ),
        prompt_plugin(),
        execution_plugin(root.path()),
        heycode_exec::local_filesystem_plugin(),
        heycode_native_tools::native_tools_plugin(),
        heycode_web::web_registry_plugin(),
        heycode_web::web_extract_plugin(),
        tools_plugin(heycode_tools::ToolsConfig::default()),
        model_catalog_plugin(std::time::Duration::from_secs(300)),
        heycode_llm::token_counters_plugin(),
        heycode_telemetry::telemetry_plugin(),
        llm_plugin(
            LlmSelection {
                provider_name: PROVIDER.to_owned(),
                model: "adapter-alias".to_owned(),
            },
            vec![provider.clone()],
        ),
        heycode_agent::provider_telemetry_plugin(),
        approval_plugin(Arc::new(AutoApprove)),
        commands_plugin(),
        agent_options_plugin(AgentOptions {
            cwd: Some(root.path().to_path_buf()),
            ..AgentOptions::default()
        }),
        heycode_agent::compactions_plugin(),
        runtime_registry_plugin(),
        agent_plugin(),
        native_runtime_plugin(),
        heycode_agent::agent_attachments_plugin(),
        heycode_agent::agent_documents_plugin(),
    ];
    if subagents {
        let position = plugins
            .iter()
            .position(|plugin| plugin.name() == "agent")
            .unwrap();
        plugins.insert(
            position,
            heycode_agent::subagent_plugin(root.path().to_path_buf(), 3),
        );
    }
    if let Some(policy) = budget {
        plugins.push(heycode_agent::loop_budget_plugin(policy));
    }
    let context = heycode_core::compose(&plugins).unwrap();
    let agent = context
        .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
        .unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(heycode_session::SERVICE_SESSION)
        .unwrap();
    let catalogs = context
        .get::<CatalogRegistry>(heycode_llm::SERVICE_MODELS)
        .unwrap();
    catalogs.register(&context, catalog).unwrap();
    let attachments = context
        .get::<heycode_attachments::AttachmentStore>(heycode_attachments::SERVICE_ATTACHMENTS)
        .unwrap();
    let commands = context
        .get::<heycode_agent::CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
        .unwrap();
    let interception = context
        .get::<ProviderInterception>(heycode_llm::SERVICE_PROVIDER_INTERCEPTION)
        .unwrap();
    let telemetry = context
        .get::<heycode_telemetry::TelemetryService>(heycode_telemetry::SERVICE_TELEMETRY)
        .unwrap();
    World {
        agent,
        session,
        provider,
        catalogs,
        attachments,
        commands,
        interception,
        telemetry,
        _context: context,
        _root: root,
    }
}

struct LimitRequest;

#[async_trait]
impl Layer<ProviderRequestDecision> for LimitRequest {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        input.set_max_output_tokens(Some(321));
        next.run(input).await
    }
}

struct RejectRequest;

#[async_trait]
impl Layer<ProviderRequestDecision> for RejectRequest {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        _next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        input.reject(ProviderInterceptionCode::new("request-policy").unwrap());
        Ok(())
    }
}

struct ReplaceResponseText;

#[async_trait]
impl Layer<ProviderResponseDecision> for ReplaceResponseText {
    async fn handle(
        &self,
        input: &mut ProviderResponseDecision,
        mut next: Next<'_, ProviderResponseDecision>,
    ) -> anyhow::Result<()> {
        if let Some(InferenceEvent::TextDelta(text)) = input.event_mut() {
            *text = "filtered-by-response-layer".to_owned();
        }
        next.run(input).await
    }
}

struct RejectResponse;

#[async_trait]
impl Layer<ProviderResponseDecision> for RejectResponse {
    async fn handle(
        &self,
        input: &mut ProviderResponseDecision,
        mut next: Next<'_, ProviderResponseDecision>,
    ) -> anyhow::Result<()> {
        if matches!(input.event(), Some(InferenceEvent::TextDelta(_))) {
            input.reject(ProviderInterceptionCode::new("response-policy").unwrap());
            return Ok(());
        }
        next.run(input).await
    }
}

struct ParkRequest {
    entered: Arc<tokio::sync::Notify>,
    settled: Arc<AtomicUsize>,
}

struct CorruptNativeRoutes;

#[async_trait]
impl Layer<ProviderRequestDecision> for CorruptNativeRoutes {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        next.run(input).await?;
        input.native_tool_routes_mut().push(
            NativeToolRoute::new(
                "web_search",
                "client:unregistered",
                NativeToolImplementationKind::Client,
                None,
            )
            .unwrap(),
        );
        Ok(())
    }
}

#[async_trait]
impl Layer<ProviderRequestDecision> for ParkRequest {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        self.entered.notify_one();
        input.cancellation().cancelled().await;
        self.settled.fetch_add(1, Ordering::SeqCst);
        next.run(input).await
    }
}

#[tokio::test]
async fn request_waterfall_edit_is_validated_logged_and_then_dispatched() {
    let world = world(AdapterBehavior::Reply);
    world
        .interception
        .register_request(&world._context, LimitRequest);

    world.agent.send("intercept request").await.unwrap();

    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].max_output_tokens, Some(321));
    let requests =
        heycode_session::project_requests(world.session.lock().unwrap().events()).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header.options.max_output_tokens, Some(321));
}

#[tokio::test]
async fn request_waterfall_rejection_precedes_resolution_log_and_transport() {
    let world = world(AdapterBehavior::Reply);
    world
        .interception
        .register_request(&world._context, RejectRequest);

    let error = world.agent.send("blocked request").await.unwrap_err();

    assert!(error.to_string().contains("request-policy"), "{error}");
    assert!(world.provider.drafts.lock().unwrap().is_empty());
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    assert!(
        !world
            .session
            .lock()
            .unwrap()
            .events()
            .iter()
            .any(|event| { matches!(&event.kind, SessionEventKind::RequestHeader { .. }) })
    );
    world.assert_no_open_records();
}

#[tokio::test]
async fn response_waterfall_edit_is_the_only_text_published_and_committed() {
    let world = world(AdapterBehavior::Reply);
    world
        .interception
        .register_response(&world._context, ReplaceResponseText);

    let report = world.agent.send("intercept response").await.unwrap();

    assert_eq!(report.text, "filtered-by-response-layer");
    let session = world.session.lock().unwrap();
    let chunks = session
        .events()
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AssistantChunk {
                text: Some(text), ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(chunks, ["filtered-by-response-layer"]);
    assert!(!session.events().iter().any(|event| matches!(
        &event.kind,
        SessionEventKind::AssistantMessage { content, .. } if content == "adapter-1"
    )));
}

#[tokio::test]
async fn response_waterfall_rejection_cancels_transport_and_commits_no_output() {
    let world = world(AdapterBehavior::Reply);
    world
        .interception
        .register_response(&world._context, RejectResponse);

    let error = world.agent.send("blocked response").await.unwrap_err();

    assert!(error.to_string().contains("response-policy"), "{error}");
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 1);
    assert!(
        world.provider.operation_tokens.lock().unwrap()[0].is_cancelled(),
        "a response refusal must settle the one provider operation owner"
    );
    assert!(!world.session.lock().unwrap().events().iter().any(|event| {
        matches!(
            &event.kind,
            SessionEventKind::AssistantChunk { .. }
                | SessionEventKind::AssistantMessage { .. }
                | SessionEventKind::AssistantProviderItem { .. }
        )
    }));
    world.assert_no_open_records();
}

#[tokio::test]
async fn caller_cancellation_settles_a_parked_request_layer_before_abort() {
    let world = world(AdapterBehavior::Reply);
    let entered = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(AtomicUsize::new(0));
    world.interception.register_request(
        &world._context,
        ParkRequest {
            entered: entered.clone(),
            settled: settled.clone(),
        },
    );
    let caller = CancellationToken::new();
    let cancellation = caller.clone();
    let agent = world.agent.clone();
    let task =
        tokio::spawn(async move { agent.send_cancellable("park request", cancellation).await });
    tokio::time::timeout(std::time::Duration::from_secs(5), entered.notified())
        .await
        .unwrap();

    caller.cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert_eq!(report.reason, "aborted");
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    assert!(world.provider.drafts.lock().unwrap().is_empty());
    world.assert_no_open_records();
}

#[tokio::test]
async fn authentication_preview_drift_fails_before_request_commit_or_transport() {
    let world = world(AdapterBehavior::AuthDrift);

    let error = world.agent.send("auth drift").await.unwrap_err();

    assert!(
        error.to_string().contains("authentication binding"),
        "{error}"
    );
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    assert!(
        !world
            .session
            .lock()
            .unwrap()
            .events()
            .iter()
            .any(|event| { matches!(&event.kind, SessionEventKind::RequestHeader { .. }) })
    );
    world.assert_no_open_records();
}

#[tokio::test]
async fn native_tool_request_consumer_rejects_a_downstream_injected_route() {
    let world = world(AdapterBehavior::Reply);
    world
        .interception
        .register_request(&world._context, CorruptNativeRoutes);

    let error = world.agent.send("route drift").await.unwrap_err();

    assert!(
        error.to_string().contains("native-tool-route-drift"),
        "{error}"
    );
    assert!(world.provider.drafts.lock().unwrap().is_empty());
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    world.assert_no_open_records();
}

#[tokio::test]
async fn response_telemetry_consumer_records_one_body_free_provider_failure() {
    let world = world(AdapterBehavior::StreamError);

    let _error = world.agent.send("provider fails").await.unwrap_err();

    assert_eq!(
        world
            .telemetry
            .count(heycode_telemetry::TelemetryEventName::RequestFailed),
        1
    );
}

#[tokio::test]
async fn loop_budget_plugin_replaces_the_legacy_native_pause_cap() {
    let policy =
        heycode_agent::LoopBudgetPolicy::new(2, 1_000_000, std::time::Duration::from_secs(60), 100)
            .unwrap()
            .with_unknown_usage(heycode_agent::LoopUnknownUsagePolicy::AllowLowerBound);
    let world = world_with_budget(AdapterBehavior::AlwaysPause, policy);

    let error = world
        .agent
        .send("pause forever")
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("max_steps_per_turn"), "{error}");
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 2);
    let reason = world
        .session
        .lock()
        .unwrap()
        .events()
        .iter()
        .rev()
        .find_map(|event| match event.kind {
            SessionEventKind::TurnEnd { reason, .. } => Some(reason),
            _ => None,
        })
        .unwrap();
    assert_eq!(reason, heycode_session::TurnEndReason::MaxSteps);
}

fn png() -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(&[255, 0, 0, 255], 1, 1, image::ExtendedColorType::Rgba8)
        .unwrap();
    bytes
}

fn pdf(text: &str) -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{Document, Object, Stream, dictionary};

    let mut document = Document::with_version("1.5");
    let pages_id = document.new_object_id();
    let font_id = document.add_object(dictionary! {
        "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Helvetica",
    });
    let resources_id = document.add_object(dictionary! {
        "Font" => dictionary! {"F1" => font_id},
    });
    let content = Content {
        operations: vec![
            Operation::new("BT", Vec::new()),
            Operation::new("Tf", vec![Object::Name(b"F1".to_vec()), 12.into()]),
            Operation::new("Td", vec![20.into(), 50.into()]),
            Operation::new("Tj", vec![Object::string_literal(text)]),
            Operation::new("ET", Vec::new()),
        ],
    };
    let content_id = document.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = document.add_object(dictionary! {
        "Type" => "Page", "Parent" => pages_id, "Contents" => content_id,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 200.into(), 100.into()],
    });
    document.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        }),
    );
    let catalog_id = document.add_object(dictionary! {"Type" => "Catalog", "Pages" => pages_id});
    document.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    document.save_to(&mut bytes).unwrap();
    bytes
}

#[tokio::test]
async fn attach_command_admits_image_and_emits_add_or_clear_composer_actions() {
    let world = world(AdapterBehavior::Reply);
    let path = world._root.path().join("command.png");
    std::fs::write(&path, png()).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    world
        .agent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    let command = world.commands.get("attach").unwrap().unwrap();
    assert_eq!(command.descriptor().source().plugin(), "agent-attachments");
    assert_eq!(command.descriptor().synopsis(), "/attach <path...>");
    command
        .execute(&world.agent, path.to_str().unwrap())
        .await
        .unwrap();
    command.execute(&world.agent, "clear").await.unwrap();
    let events = events.lock().unwrap();
    assert!(events.iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::AttachmentComposerRequested {
            action: heycode_agent::AttachmentComposerAction::Add(metadata)
        } if metadata.display_name() == Some("command.png")
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::AttachmentComposerRequested {
            action: heycode_agent::AttachmentComposerAction::Clear
        }
    )));
}

#[tokio::test]
async fn document_command_admits_only_supported_document_content() {
    let world = world(AdapterBehavior::Reply);
    let path = world._root.path().join("guide.pdf");
    std::fs::write(&path, pdf("Command PDF")).unwrap();
    let events = Arc::new(Mutex::new(Vec::new()));
    let sink = events.clone();
    world
        .agent
        .ui()
        .on::<heycode_agent::UiEvent>(move |event| sink.lock().unwrap().push(event.clone()));
    let command = world.commands.get("document").unwrap().unwrap();
    assert_eq!(command.descriptor().source().plugin(), "agent-documents");
    command
        .execute(&world.agent, path.to_str().unwrap())
        .await
        .unwrap();
    assert!(events.lock().unwrap().iter().any(|event| matches!(
        event,
        heycode_agent::UiEvent::AttachmentComposerRequested {
            action: heycode_agent::AttachmentComposerAction::Add(metadata)
        } if metadata.media_type().as_str() == "application/pdf"
            && metadata.display_name() == Some("guide.pdf")
    )));
}

#[tokio::test]
async fn image_attachment_is_preflighted_durable_and_dispatched_by_content() {
    let world = world(AdapterBehavior::Reply);
    let admission = world
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(png(), Some("image/png"), Some("pixel.png"))
                .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();
    let report = world
        .agent
        .send_with_attachments("describe", vec![admission.metadata().clone()])
        .await
        .unwrap();
    assert_eq!(report.text, "adapter-1");
    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(
        drafts[0].input_modalities,
        [
            heycode_llm::InputModality::Text,
            heycode_llm::InputModality::Image,
        ]
    );
    let image_message = drafts[0].inputs.iter().find_map(|input| match input {
        InferenceInput::Message(message) if !message.images.is_empty() => Some(message),
        _ => None,
    });
    let image_message = image_message.expect("image message must reach adapter resolution");
    assert_eq!(image_message.content, "describe");
    assert_eq!(image_message.images.len(), 1);
    assert_eq!(image_message.images[0].media_type().as_str(), "image/png");
    assert_eq!(image_message.images[0].bytes(), png());
    drop(drafts);

    let events = world.session.lock().unwrap().events().to_vec();
    assert!(matches!(
        events[0].kind,
        SessionEventKind::AttachmentAdded { .. }
    ));
    assert!(matches!(
        events[1].kind,
        SessionEventKind::UserAttachments { .. }
    ));
    assert!(matches!(
        events[2].kind,
        SessionEventKind::UserMessage { .. }
    ));
    let requests = heycode_session::project_requests(&events).unwrap();
    assert_eq!(
        requests[0].header.options.input_modalities,
        ["text", "image"]
    );
    let selected = requests[0].inputs.iter().find_map(|input| match input {
        heycode_session::ProjectedInput::Message(message) if !message.attachments.is_empty() => {
            Some(message)
        }
        _ => None,
    });
    assert_eq!(
        selected.unwrap().attachments,
        [admission.metadata().clone()]
    );
}

#[tokio::test]
async fn unsupported_or_unproven_image_capability_refuses_before_user_admission() {
    for support in [CapabilitySupport::Unsupported, CapabilitySupport::Unknown] {
        let world = world_with_image_support(AdapterBehavior::Reply, support);
        let admission = world
            .attachments
            .admit(
                heycode_attachments::AttachmentInput::new(
                    png(),
                    Some("image/png"),
                    Some("pixel.png"),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .unwrap();
        let before = world.session.lock().unwrap().events().len();
        let error = world
            .agent
            .send_with_attachments("describe", vec![admission.metadata().clone()])
            .await
            .unwrap_err()
            .to_string();
        match support {
            CapabilitySupport::Unsupported => assert!(error.contains("does not support")),
            CapabilitySupport::Unknown => assert!(error.contains("unproven")),
            CapabilitySupport::Supported => unreachable!(),
        }
        assert_eq!(world.session.lock().unwrap().events().len(), before);
        assert!(world.provider.drafts.lock().unwrap().is_empty());
        assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn hidden_supported_audio_reaches_only_the_exact_audio_adapter_after_durable_association() {
    let world = world(AdapterBehavior::AudioReply);
    let bytes = pcm_wav(8_000, 1, 8_000, b"RAW-USER-AUDIO");
    let admission = world
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(
                bytes.clone(),
                Some("audio/wav"),
                Some("question.wav"),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();

    world
        .agent
        .send_with_attachments("transcribe", vec![admission.metadata().clone()])
        .await
        .unwrap();

    let calls = world.provider.audio_calls.lock().unwrap();
    assert_eq!(calls.as_slice(), &[vec![bytes]]);
    drop(calls);
    assert!(world.provider.drafts.lock().unwrap().is_empty());
    assert_eq!(world.provider.legacy_calls.load(Ordering::SeqCst), 0);
    let events = world.session.lock().unwrap().events().to_vec();
    let selection = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::UserAttachments { .. }))
        .unwrap();
    let request = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::RequestHeader { .. }))
        .unwrap();
    assert!(selection < request);
    let modalities = events.iter().find_map(|event| match &event.kind {
        SessionEventKind::RequestHeader { header, .. } => {
            Some(header.options.input_modalities.clone())
        }
        _ => None,
    });
    assert_eq!(modalities.unwrap(), ["text", "audio"]);
}

#[tokio::test]
async fn absent_unsupported_and_unproven_audio_paths_refuse_before_user_association() {
    for (behavior, expected) in [
        (AdapterBehavior::Reply, "not composed"),
        (AdapterBehavior::AudioUnsupported, "unsupported"),
        (AdapterBehavior::AudioUnproven, "unproven"),
    ] {
        let world = world(behavior);
        let admission = world
            .attachments
            .admit(
                heycode_attachments::AttachmentInput::new(
                    pcm_wav(8_000, 1, 8_000, b"INPUT"),
                    Some("audio/wav"),
                    Some("question.wav"),
                )
                .unwrap(),
                CancellationToken::new(),
            )
            .unwrap();
        let before = world.session.lock().unwrap().events().len();

        let error = world
            .agent
            .send_with_attachments("transcribe", vec![admission.metadata().clone()])
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(expected), "{error}");
        assert_eq!(world.session.lock().unwrap().events().len(), before);
        assert!(world.provider.audio_calls.lock().unwrap().is_empty());
        assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    }
}

#[tokio::test]
async fn audio_output_commits_bytes_then_association_and_cancellation_drops_pending_output() {
    let world = world(AdapterBehavior::AudioOutput);
    let admission = world
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(
                pcm_wav(8_000, 1, 8_000, b"INPUT"),
                Some("audio/wav"),
                Some("question.wav"),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();
    world
        .agent
        .send_with_attachments("answer aloud", vec![admission.metadata().clone()])
        .await
        .unwrap();
    let events = world.session.lock().unwrap().events().to_vec();
    let output_index = events
        .iter()
        .position(|event| matches!(event.kind, SessionEventKind::AssistantAudio { .. }))
        .unwrap();
    assert!(matches!(
        events[output_index - 1].kind,
        SessionEventKind::AttachmentAdded { .. }
    ));
    let SessionEventKind::AssistantAudio { attachments, .. } = &events[output_index].kind else {
        unreachable!()
    };
    let output = &attachments[0];
    assert_eq!(output.media_type().as_str(), "audio/wav");
    assert_eq!(output.audio().unwrap().duration_ms(), 1_000);
    assert_eq!(
        world
            .attachments
            .read(output, CancellationToken::new())
            .unwrap(),
        pcm_wav(8_000, 1, 8_000, b"RAW-ASSISTANT-AUDIO")
    );

    let cancelled = world_with_capabilities(
        AdapterBehavior::AudioHang,
        CapabilitySupport::Supported,
        CapabilitySupport::Unsupported,
    );
    let admission = cancelled
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(
                pcm_wav(8_000, 1, 8_000, b"INPUT"),
                Some("audio/wav"),
                Some("question.wav"),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();
    let caller = CancellationToken::new();
    let agent = cancelled.agent.clone();
    let selected = admission.metadata().clone();
    let operation = caller.clone();
    let send = tokio::spawn(async move {
        agent
            .send_with_attachments_cancellable("answer aloud", vec![selected], operation)
            .await
    });
    cancelled.provider.started.notified().await;
    caller.cancel();
    let report = tokio::time::timeout(std::time::Duration::from_secs(1), send)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(report.reason, "aborted");
    assert!(
        !cancelled
            .session
            .lock()
            .unwrap()
            .events()
            .iter()
            .any(|event| { matches!(event.kind, SessionEventKind::AssistantAudio { .. }) })
    );
}

#[tokio::test]
async fn supported_pdf_uses_an_explicit_native_route_and_exact_document_input() {
    let world = world_with_document_support(AdapterBehavior::Reply, CapabilitySupport::Supported);
    let admission = world
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(
                pdf("Native PDF"),
                Some("application/pdf"),
                Some("guide.pdf"),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();

    world
        .agent
        .send_with_attachments("summarize", vec![admission.metadata().clone()])
        .await
        .unwrap();

    let drafts = world.provider.drafts.lock().unwrap();
    let message = drafts[0].inputs.iter().find_map(|input| match input {
        InferenceInput::Message(message) if !message.documents.is_empty() => Some(message),
        _ => None,
    });
    let message = message.expect("native PDF must reach the strict adapter");
    assert_eq!(message.documents.len(), 1);
    assert_eq!(message.documents[0].filename(), "guide.pdf");
    assert_eq!(message.documents[0].bytes(), pdf("Native PDF"));
    assert_eq!(
        drafts[0].input_modalities,
        [
            heycode_llm::InputModality::Text,
            heycode_llm::InputModality::Document,
        ]
    );
    drop(drafts);

    let events = world.session.lock().unwrap().events().to_vec();
    let SessionEventKind::UserAttachments {
        attachments,
        document_routes,
    } = &events[1].kind
    else {
        panic!("missing durable native document selection")
    };
    assert_eq!(attachments, &[admission.metadata().clone()]);
    assert_eq!(document_routes.len(), 1);
    assert_eq!(
        document_routes[0].kind(),
        heycode_core::DocumentInputRouteKind::Native
    );
}

#[tokio::test]
async fn unsupported_pdf_uses_bounded_extraction_and_records_both_objects() {
    let world = world_with_document_support(AdapterBehavior::Reply, CapabilitySupport::Unsupported);
    let admission = world
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(
                pdf("Portable PDF"),
                Some("application/pdf"),
                Some("guide.pdf"),
            )
            .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();

    world
        .agent
        .send_with_attachments("summarize", vec![admission.metadata().clone()])
        .await
        .unwrap();

    let drafts = world.provider.drafts.lock().unwrap();
    let message = drafts[0].inputs.iter().find_map(|input| match input {
        InferenceInput::Message(message)
            if message.content.contains("<document route=\"extracted\"") =>
        {
            Some(message)
        }
        _ => None,
    });
    let message = message.expect("extracted PDF text must reach the adapter");
    assert!(
        message.content.contains("Portable PDF"),
        "{}",
        message.content
    );
    assert!(message.documents.is_empty());
    assert_eq!(
        drafts[0].input_modalities,
        [heycode_llm::InputModality::Text]
    );
    drop(drafts);

    let events = world.session.lock().unwrap().events().to_vec();
    assert!(matches!(
        events[0].kind,
        SessionEventKind::AttachmentAdded { .. }
    ));
    assert!(matches!(
        events[1].kind,
        SessionEventKind::AttachmentAdded { .. }
    ));
    let SessionEventKind::UserAttachments {
        attachments,
        document_routes,
    } = &events[2].kind
    else {
        panic!("missing durable extracted document selection")
    };
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].media_type().as_str(), "text/plain");
    assert_eq!(document_routes.len(), 1);
    assert_eq!(
        document_routes[0].kind(),
        heycode_core::DocumentInputRouteKind::Extracted
    );
    assert_eq!(document_routes[0].source(), admission.metadata());
    assert_eq!(document_routes[0].selected(), &attachments[0]);
}

#[tokio::test]
async fn cancelling_image_preflight_leaves_shared_refresh_registry_owned() {
    let started = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let settled = Arc::new(AtomicUsize::new(0));
    let world = world_with_catalog(
        AdapterBehavior::Reply,
        Arc::new(CancellationCatalog {
            started: started.clone(),
            release: release.clone(),
            settled: settled.clone(),
        }),
    );
    let admission = world
        .attachments
        .admit(
            heycode_attachments::AttachmentInput::new(png(), Some("image/png"), Some("pixel.png"))
                .unwrap(),
            CancellationToken::new(),
        )
        .unwrap();
    let before = world.session.lock().unwrap().events().len();
    let agent = world.agent.clone();
    let attachment = admission.metadata().clone();
    let send = tokio::spawn(async move {
        agent
            .send_with_attachments("describe", vec![attachment])
            .await
    });
    started.notified().await;

    world.agent.token().cancel();
    let error = tokio::time::timeout(std::time::Duration::from_secs(1), send)
        .await
        .expect("image preflight did not settle")
        .unwrap()
        .unwrap_err()
        .to_string();

    assert!(
        error.contains("cancelled before attachment admission"),
        "{error}"
    );
    assert_eq!(settled.load(Ordering::SeqCst), 0);
    assert_eq!(world.session.lock().unwrap().events().len(), before);
    assert!(world.provider.drafts.lock().unwrap().is_empty());

    release.notify_one();
    let shared = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        world.catalogs.refresh(
            PROVIDER,
            CatalogRefreshMode::PreferCache,
            CancellationToken::new(),
        ),
    )
    .await
    .expect("registry-owned catalog refresh did not settle")
    .unwrap();
    assert_eq!(settled.load(Ordering::SeqCst), 1);
    assert_eq!(
        shared.snapshot.models[0].capabilities.image_input,
        CapabilitySupport::Supported
    );
}

#[tokio::test]
async fn advertised_adapter_refreshes_evidence_and_commits_durable_verified_request_state() {
    let world = world(AdapterBehavior::Reply);
    assert_eq!(world.agent.send("first").await.unwrap().text, "adapter-1");
    let catalog = world.catalogs.cached(PROVIDER).unwrap();
    assert_eq!(world.agent.send("second").await.unwrap().text, "adapter-2");
    assert_eq!(world.provider.legacy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 2);

    let events = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .to_vec();
    let requests = heycode_session::project_requests(&events).unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.header.provider, PROVIDER);
        assert_eq!(request.header.model, MODEL);
        assert_eq!(
            request.header.protocol,
            ProviderProtocol::OpenAiChatCompletions
        );
        assert_eq!(request.context.catalog_revision, Some(catalog.revision));
        assert_eq!(
            request.context.catalog_fetched_at_ms,
            Some(catalog.fetched_at_ms)
        );
        assert_eq!(request.context.context_window, Some(64_000));
        assert_eq!(request.context.max_output_tokens, Some(4_096));
    }
    assert!(requests[1].inputs.iter().any(
        |input| matches!(input, heycode_session::ProjectedInput::ProviderState(item)
            if item.data().get("content") == Some(&serde_json::json!("adapter-1")))
    ));

    let mut provider_items = events.iter().filter_map(|event| match &event.kind {
        SessionEventKind::AssistantProviderItem {
            request_id,
            output_index,
            item,
            ..
        } => Some((request_id, *output_index, item)),
        _ => None,
    });
    let first = provider_items.next().unwrap();
    let second = provider_items.next().unwrap();
    assert_eq!(first.1, 0);
    assert_eq!(second.1, 0);
    assert_eq!(first.0, &requests[0].request_id);
    assert_eq!(second.0, &requests[1].request_id);
    assert!(provider_items.next().is_none());
    let response_metadata = events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AssistantResponseMetadata {
                request_id,
                metadata,
                ..
            } => Some((request_id, metadata)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(response_metadata.len(), 2);
    assert_eq!(response_metadata[0].0, &requests[0].request_id);
    assert_eq!(response_metadata[1].0, &requests[1].request_id);
    assert_eq!(
        response_metadata[1]
            .1
            .cache_usage()
            .unwrap()
            .cache_read_tokens(),
        3
    );

    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(drafts.len(), 2);
    assert!(drafts[1].inputs.iter().any(|input| {
        matches!(input, InferenceInput::ProviderState(item)
            if item.data().get("content") == Some(&serde_json::json!("adapter-1")))
    }));
    assert!(!drafts[1].inputs.iter().any(|input| matches!(
        input,
        InferenceInput::Message(message)
            if message.role == heycode_llm::Role::Assistant && message.content == "adapter-1"
    )));
    drop(drafts);
    let envelope = world
        .agent
        .token_envelope()
        .expect("strict dispatch must publish its measured request envelope");
    assert_eq!(envelope.entries().len(), 7);
    assert!(matches!(
        envelope
            .entries()
            .iter()
            .find(|entry| {
                entry.contributor() == heycode_llm::EnvelopeContributor::ProviderState
            })
            .map(heycode_llm::EnvelopeEntry::tokens),
        Some(heycode_llm::ContributorTokens::Uncounted(
            heycode_llm::UncountedReason::Unmeasurable
        ))
    ));
}

#[tokio::test]
async fn caller_cancellation_cancels_the_exact_adapter_operation_and_settles_aborted() {
    let world = world(AdapterBehavior::Hang);
    world
        .catalogs
        .refresh(
            PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let started = world.provider.started.notified();
    let cancellation = CancellationToken::new();
    let task = tokio::spawn({
        let agent = world.agent.clone();
        let cancellation = cancellation.clone();
        async move { agent.send_cancellable("cancel me", cancellation).await }
    });
    started.await;
    cancellation.cancel();
    let report = task.await.unwrap().unwrap();

    assert_eq!(report.reason, "aborted");
    assert_eq!(world.provider.legacy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 1);
    assert!(
        world
            .provider
            .operation_tokens
            .lock()
            .unwrap()
            .iter()
            .all(CancellationToken::is_cancelled)
    );
    assert!(
        world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .any(|event| matches!(
                event.kind,
                SessionEventKind::TurnEnd {
                    reason: heycode_session::TurnEndReason::Aborted,
                    ..
                }
            ))
    );
}

#[tokio::test]
async fn advertised_adapter_resolution_failure_never_falls_back_to_legacy_streaming() {
    let world = world(AdapterBehavior::Reject);
    world
        .catalogs
        .refresh(
            PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let error = world.agent.send("reject me").await.unwrap_err();
    assert!(error.to_string().contains("test adapter rejected"));
    assert_eq!(world.provider.legacy_calls.load(Ordering::SeqCst), 0);
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    let events = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .to_vec();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::RequestHeader { .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::TurnEnd {
            reason: heycode_session::TurnEndReason::Error,
            ..
        }
    )));
    world.assert_no_open_records();
}

#[tokio::test]
async fn adapter_stream_error_remains_a_typed_llm_error_for_tui_downcasting() {
    let world = world(AdapterBehavior::StreamError);
    world
        .catalogs
        .refresh(
            PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let error = world.agent.send("provider fails").await.unwrap_err();
    let llm = error.downcast_ref::<heycode_llm::LlmError>().unwrap();
    assert_eq!(llm.class(), ProviderErrorClass::Server);
    assert_eq!(world.provider.legacy_calls.load(Ordering::SeqCst), 0);
    assert!(
        !world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::AssistantProviderItem { .. })),
        "provider state is model-visible only after terminal Finish"
    );
    assert!(
        !world
            .session
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .events()
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::ServerToolCall { .. })),
        "normalized server events commit only after terminal Finish"
    );
    world.assert_no_open_records();
}

#[tokio::test]
async fn invalid_server_event_group_fails_before_any_output_event_commits() {
    let world = world(AdapterBehavior::OrphanServerResult);
    let error = world.agent.send("invalid server result").await.unwrap_err();
    assert!(error.to_string().contains("unknown call"));
    let events = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .to_vec();
    assert!(!events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::ServerToolResult { .. } | SessionEventKind::AssistantProviderItem { .. }
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::TurnEnd {
            reason: heycode_session::TurnEndReason::Error,
            ..
        }
    )));
}

#[tokio::test]
async fn provider_pause_replays_exact_state_and_durably_settles_server_tool_events() {
    let world = world(AdapterBehavior::PauseThenReply);
    let report = world.agent.send("search").await.unwrap();
    assert_eq!(report.text, "adapter-continued");
    assert_eq!(report.reason, "stop");
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 2);

    let events = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .to_vec();
    let requests = heycode_session::project_requests(&events).unwrap();
    assert_eq!(requests.len(), 2);
    assert!(matches!(
        requests[0].server_tool_events.as_slice(),
        [heycode_session::ProjectedServerToolEvent::Call { call, .. }]
            if call.logical() == "web_search"
    ));
    assert!(matches!(
        requests[1].server_tool_events.as_slice(),
        [
            heycode_session::ProjectedServerToolEvent::Result { result, .. },
            heycode_session::ProjectedServerToolEvent::Citation { citation, .. }
        ] if result.call_id() == &CallId::from_raw("srvtoolu_1")
            && citation.title() == Some("Rust")
    ));
    assert!(
        world.provider.drafts.lock().unwrap()[1]
            .inputs
            .iter()
            .any(|input| matches!(input, InferenceInput::ProviderState(item)
            if item.data()["content"] == "paused-state"))
    );
    assert!(events.iter().any(|event| matches!(
        event.kind,
        SessionEventKind::TurnEnd {
            reason: heycode_session::TurnEndReason::Stop,
            ..
        }
    )));
    assert!(matches!(
        world
            .agent
            .token_envelope()
            .and_then(|envelope| {
                envelope
                    .entries()
                    .iter()
                    .find(|entry| {
                        entry.contributor() == heycode_llm::EnvelopeContributor::ProviderState
                    })
                    .cloned()
            })
            .map(|entry| entry.tokens().clone()),
        Some(heycode_llm::ContributorTokens::Uncounted(
            heycode_llm::UncountedReason::Unmeasurable
        ))
    ));
}

#[tokio::test]
async fn repeated_provider_pause_has_no_implicit_continuation_cap() {
    let world = world(AdapterBehavior::ManyPauses);
    let report = world.agent.send("loop").await.unwrap();
    assert_eq!(report.reason, "stop");
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 13);
    let session = world.session.lock().unwrap();
    assert_eq!(
        heycode_session::project_requests(session.events())
            .unwrap()
            .len(),
        13
    );
}

#[tokio::test]
async fn cancellation_won_during_resolution_commits_no_phantom_request() {
    let world = world(AdapterBehavior::CancelDuringResolve);
    world
        .catalogs
        .refresh(
            PROVIDER,
            CatalogRefreshMode::Force,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let cancellation = CancellationToken::new();
    *world.provider.cancel_on_resolve.lock().unwrap() = Some(cancellation.clone());

    let report = world
        .agent
        .send_cancellable("cancel while resolving", cancellation)
        .await
        .unwrap();
    assert_eq!(report.reason, "aborted");
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    assert_eq!(world.provider.legacy_calls.load(Ordering::SeqCst), 0);
    let events = world
        .session
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .events()
        .to_vec();
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::RequestHeader { .. }))
    );
    assert!(
        !events
            .iter()
            .any(|event| matches!(event.kind, SessionEventKind::RequestContext { .. }))
    );
}

/// AGENTS.md: "only the returned exact operation provider may supply
/// options/adapter/P10". Compaction is an inference request like any other, so
/// it must await `prepare_inference` before it borrows an adapter. A provider
/// whose registry instance is a lazy stub (Vertex) otherwise has a working
/// turn loop and a `/compact` that can never succeed — and, because
/// auto-compaction is on by default, every turn past the threshold fails too.
#[tokio::test]
async fn portable_compaction_prepares_the_provider_before_borrowing_its_adapter() {
    let world = world(AdapterBehavior::PreparedReply);
    seed_two_turns(&world);

    let outcome = world
        .agent
        .compact(
            heycode_agent::PortableCompaction::ID,
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        matches!(outcome, heycode_agent::CompactionOutcome::Applied { folded, .. } if folded > 0),
        "{outcome:?}"
    );
    assert_eq!(
        world.provider.preparation_calls.load(Ordering::SeqCst),
        1,
        "compaction must await prepare_inference"
    );
    let prepared = world.provider.prepared_provider.as_ref().unwrap();
    assert_eq!(
        prepared.adapter_calls.load(Ordering::SeqCst),
        1,
        "the summary must stream from the PREPARED provider"
    );
    assert_eq!(
        world.provider.adapter_calls.load(Ordering::SeqCst),
        0,
        "the inert registry instance must never be streamed"
    );
}

/// The native checkpoint route borrows an adapter too, so it owes the same
/// preparation. (Its P10 exemption is separate — see the interception test.)
#[tokio::test]
async fn native_compaction_prepares_the_provider_before_borrowing_its_adapter() {
    let world = world(AdapterBehavior::PreparedReply);
    seed_two_turns(&world);

    let outcome = world
        .agent
        .compact(
            heycode_agent::NativeCompaction::ID,
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();

    assert!(
        matches!(outcome, heycode_agent::CompactionOutcome::Applied { folded, .. } if folded > 0),
        "{outcome:?}"
    );
    assert_eq!(
        world.provider.preparation_calls.load(Ordering::SeqCst),
        1,
        "native compaction must await prepare_inference"
    );
}

fn seed_two_turns(world: &World) {
    let mut session = world.session.lock().unwrap();
    for (turn, user, assistant) in [
        (0, "old question", "old answer"),
        (1, "recent question", "recent answer"),
    ] {
        session
            .append(SessionEventKind::UserMessage {
                text: user.to_owned(),
            })
            .unwrap();
        session
            .append(SessionEventKind::TurnStart { turn })
            .unwrap();
        session
            .append(SessionEventKind::AssistantMessage {
                turn,
                step: 0,
                content: assistant.to_owned(),
                reasoning: None,
                tool_calls: None,
                usage: None,
            })
            .unwrap();
        session
            .append(SessionEventKind::TurnEnd {
                turn,
                reason: heycode_session::TurnEndReason::Stop,
            })
            .unwrap();
    }
}

/// Records every request the P10 seam saw, by purpose.
struct RecordPurposes(Arc<Mutex<Vec<heycode_llm::CallPurpose>>>);

#[async_trait]
impl Layer<ProviderRequestDecision> for RecordPurposes {
    async fn handle(
        &self,
        input: &mut ProviderRequestDecision,
        mut next: Next<'_, ProviderRequestDecision>,
    ) -> anyhow::Result<()> {
        self.0.lock().unwrap().push(input.draft().purpose);
        next.run(input).await
    }
}

/// A portable summary is an ordinary inference request that happens to carry
/// the entire transcript, so a P10 request layer must see it — otherwise a
/// policy layer that inspects every outbound request has a blind spot exactly
/// where the most context leaves the process. Native compaction stays outside
/// the seam by design (AGENTS.md), and this pins both halves.
#[tokio::test]
async fn portable_compaction_is_intercepted_while_native_compaction_is_exempt() {
    let world = world(AdapterBehavior::Reply);
    let seen: Arc<Mutex<Vec<heycode_llm::CallPurpose>>> = Arc::new(Mutex::new(Vec::new()));
    world
        .interception
        .register_request(&world._context, RecordPurposes(seen.clone()));
    seed_two_turns(&world);

    world
        .agent
        .compact(
            heycode_agent::PortableCompaction::ID,
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![heycode_llm::CallPurpose::Compaction],
        "the portable summary request must reach the P10 request seam"
    );

    seen.lock().unwrap().clear();
    seed_two_turns(&world);
    world
        .agent
        .compact(
            heycode_agent::NativeCompaction::ID,
            1,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        seen.lock().unwrap().is_empty(),
        "native compaction is explicitly outside the P10 seam: its opaque \
         checkpoint operation is not a draft a request layer can revise"
    );
}

#[tokio::test]
async fn context_budget_uses_resolved_model_and_is_saved_with_the_request() {
    let world = world(AdapterBehavior::Reply);
    world.agent.send("hello").await.unwrap();
    let budget = world.agent.context_budget().unwrap();
    assert_eq!(budget.model, MODEL);
    assert_eq!(budget.window, Some(64_000));
    assert_eq!(budget.limit_source, heycode_llm::ContextLimitSource::Model);
    assert!(budget.output_reserve > 0);
    let session = world.session.lock().unwrap();
    let saved = session
        .events()
        .iter()
        .find_map(|event| match &event.kind {
            SessionEventKind::RequestContext { context, .. } => context.budget.as_ref(),
            _ => None,
        })
        .unwrap();
    assert_eq!(saved.window, budget.window);
    assert_eq!(saved.compact_at, budget.compact_at);
    assert_eq!(
        saved.used,
        world.agent.token_envelope().unwrap().total().counted()
    );
}

#[tokio::test]
async fn an_oversized_first_request_is_refused_without_dispatch_or_repeated_compaction() {
    let world = world(AdapterBehavior::Reply);
    let error = world.agent.send(&"x".repeat(300_000)).await.unwrap_err();
    assert!(error.to_string().contains("usable model budget"), "{error}");
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        world.provider.drafts.lock().unwrap().len(),
        2,
        "one initial measurement and one check after the no-op compaction"
    );
    world.assert_no_open_records();
}

#[tokio::test]
async fn manual_portable_compaction_includes_tail_beyond_the_old_truncation_limit() {
    let world = world(AdapterBehavior::Reply);
    world
        .agent
        .send(&format!("{}TAIL-MUST-SURVIVE", "x".repeat(30_000)))
        .await
        .unwrap();
    world.agent.send("second").await.unwrap();
    world
        .agent
        .compact("portable-summary", 1, CancellationToken::new())
        .await
        .unwrap();
    let drafts = world.provider.drafts.lock().unwrap();
    let summaries: Vec<_> = drafts
        .iter()
        .filter(|draft| draft.purpose == heycode_llm::CallPurpose::Compaction)
        .collect();
    assert!(
        summaries.len() >= 3,
        "two bounded chunks and their combined summary"
    );
    assert!(
        summaries
            .iter()
            .any(|draft| draft.inputs.iter().any(|input| matches!(input,
                InferenceInput::Message(message) if message.content.contains("TAIL-MUST-SURVIVE")
            )))
    );
}

#[tokio::test]
async fn session_control_side_question_uses_prepared_strict_adapter_without_touching_main_history()
{
    let world = world(AdapterBehavior::PreparedReply);
    let before = serde_json::to_value(world.session.lock().unwrap().events()).unwrap();
    let answer = world
        .agent
        .side_question("Explain the current task briefly.")
        .await
        .unwrap();
    assert!(!answer.is_empty());
    assert_eq!(
        before,
        serde_json::to_value(world.session.lock().unwrap().events()).unwrap()
    );
    let prepared = world.provider.prepared_provider.as_ref().unwrap();
    let drafts = prepared.drafts.lock().unwrap();
    let draft = drafts.last().unwrap();
    assert!(draft.tools.is_empty());
    assert!(draft.native_tool_routes.is_empty());
    assert!(draft.native_features.is_empty());
    assert_eq!(draft.purpose, heycode_llm::CallPurpose::Conversation);
}

#[tokio::test]
async fn plan_rejects_mutating_provider_executed_routes_before_transport() {
    let mut world = world(AdapterBehavior::Reply);
    // Attach the production Plan plugin to this strict-adapter fixture. The shared
    // request waterfall is also the one inherited by every native child.
    heycode_agent::plan_plugin()
        .apply(&mut world._context)
        .unwrap();
    let native = world
        ._context
        .get::<heycode_native_tools::NativeToolRegistry>(heycode_native_tools::SERVICE_NATIVE_TOOLS)
        .unwrap();
    native
        .register(
            &world._context,
            heycode_native_tools::NativeToolImplementation::new(
                "bash",
                "adapter-test:shell",
                NativeToolImplementationKind::Provider,
                Some(PROVIDER.to_owned()),
                100,
            )
            .unwrap(),
        )
        .unwrap();
    let plan = world
        ._context
        .get::<heycode_agent::PlanHandle>(heycode_agent::SERVICE_PLAN)
        .unwrap();
    plan.0.set(true).await.unwrap();
    let error = world
        .agent
        .send("Inspect without hosted mutations")
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("plan-mode-mutating-provider-tool"),
        "{error:#}"
    );
    assert_eq!(world.provider.adapter_calls.load(Ordering::SeqCst), 0);
    assert!(world.provider.drafts.lock().unwrap().is_empty());
    world.assert_no_open_records();
}

#[tokio::test]
async fn custom_child_effort_model_and_instructions_reach_strict_adapter_and_durable_header() {
    let world = world_with_catalog_budget_and_subagents(
        AdapterBehavior::Reply,
        Arc::new(StaticCatalog {
            image_input: CapabilitySupport::Supported,
            document_input: CapabilitySupport::Unsupported,
        }),
        None,
        true,
    );
    let registry = world
        ._context
        .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
        .unwrap();
    let _preset = registry
        .register_preset_owned(
            heycode_agent::SubagentPreset::new(
                "configured",
                "Configured",
                "Strict child instructions",
                None,
                heycode_agent::SubagentSeed::Fresh,
                heycode_agent::SubagentContinuation::OneShot,
            )
            .unwrap()
            .with_config(heycode_agent::SubagentConfig {
                model: Some(MODEL.to_owned()),
                effort: Some("high".to_owned()),
                tools: Some(Vec::new()),
                ..Default::default()
            })
            .unwrap(),
        )
        .unwrap();
    let tools = world
        ._context
        .get::<heycode_tools::ToolRegistry>(heycode_tools::SERVICE_TOOLS)
        .unwrap();
    tools
        .get("task")
        .unwrap()
        .run(
            serde_json::json!({"agent":"configured","label":"strict","prompt":"Only user task","background":false}),
            &heycode_tools::ToolCtx::default(),
        )
        .await
        .unwrap();
    let drafts = world.provider.drafts.lock().unwrap();
    assert_eq!(drafts.len(), 1);
    assert_eq!(drafts[0].model, MODEL);
    assert_eq!(
        drafts[0].reasoning_effort.as_ref().unwrap().as_str(),
        "high"
    );
    assert!(
        drafts[0]
            .system
            .as_ref()
            .unwrap()
            .contains("Strict child instructions")
    );
    assert!(drafts[0].tools.is_empty());
    assert!(drafts[0].native_tool_routes.is_empty());
    assert!(drafts[0].inputs.iter().any(|input| matches!(input, InferenceInput::Message(message) if message.role == heycode_llm::Role::User && message.content == "Only user task")));
    drop(drafts);
    let parent = world.session.lock().unwrap().id().to_string();
    let child = std::fs::read_dir(world._root.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter_map(|entry| Session::open(entry.path()).ok())
        .find(|session| session.id().as_str() != parent)
        .unwrap();
    let requests = heycode_session::project_requests(child.events()).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].header.options.reasoning_effort.as_deref(),
        Some("high")
    );
    assert!(child.events().iter().any(|event| matches!(&event.kind, SessionEventKind::UserMessage { text } if text == "Only user task")));
}
