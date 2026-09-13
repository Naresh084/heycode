//! C12 compaction strategies and their single durable commit owner.
//!
//! Strategies prepare a replacement but cannot commit it through this API.
//! The registry snapshots the append-only log, verifies that preparation made
//! no durable mutation, rejects a concurrent change, and appends exactly one
//! settlement event. Portable text and provider-native opaque state therefore
//! share one transaction without pretending they are interchangeable.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use heycode_core::EventBus;
use heycode_llm::{
    CallPurpose, CatalogRefreshMode, InferenceEvent, InferenceInput, InputModality, LlmSelection,
    NativeFeature, Provider, ProviderInterception, ProviderInterceptionError,
    ProviderOptionContext, ProviderRegistry, ProviderRequestContext, RequestDraft,
};
use heycode_session::{Session, SessionEvent, SessionEventKind};
use tokio_util::sync::CancellationToken;

use crate::ui::UiEvent;

/// Await the provider's caller-owned readiness hook for one compaction
/// request, honouring the caller's cancellation.
///
/// Compaction resolves catalog and model first, exactly like a turn, then
/// prepares, then borrows options/adapter/P10 from what preparation returned.
async fn prepare(
    provider: &dyn Provider,
    context: ProviderOptionContext<'_>,
    cancellation: &CancellationToken,
) -> Result<Option<Arc<dyn Provider>>, CompactionError> {
    let preparation_cancellation = cancellation.child_token();
    let preparation = provider.prepare_inference(context, preparation_cancellation.clone());
    tokio::pin!(preparation);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            preparation_cancellation.cancel();
            let _settled = preparation.await;
            Err(CompactionError::Cancelled)
        }
        result = &mut preparation => {
            result.map_err(|error| CompactionError::Failed(bounded(&error.to_string())))
        }
    }
}

const MAX_ID_BYTES: usize = 64;
const MAX_SUMMARY_BYTES: usize = 64 * 1024;
const MAX_SUMMARIZE_INPUT_CHARS: usize = 24_000;
const MAX_FOCUS_BYTES: usize = 4 * 1024;
const SUMMARY_SYSTEM_PROMPT: &str = "Summarize the conversation for a coding assistant continuing the work. Preserve the user's goals, decisions, files touched, and unfinished steps. Be concise and factual.";
const FOCUSED_SUMMARY_SYSTEM_SUFFIX: &str = " Give extra weight to the explicit human compaction focus in the next message when choosing summary detail, without inventing facts, weakening these retention requirements, or omitting critical unfinished work.";
const FOCUS_PREFIX: &str = "Explicit human compaction focus (JSON string): ";
const TRANSCRIPT_PREFIX: &str = "\n\nConversation to summarize:\n";

/// How a strategy reduces context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CompactionKind {
    /// The selected provider returns an exact opaque continuation checkpoint.
    Native,
    /// heycode folds history into a model-written portable summary.
    Portable,
    /// heycode drops old history and records an explicit loss marker.
    Prune,
}

impl CompactionKind {
    /// Stable wire/display name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Portable => "portable",
            Self::Prune => "prune",
        }
    }
}

/// Why a compaction request or registration was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionError {
    /// Empty, oversized, or non-kebab-case strategy id.
    InvalidId,
    /// A strategy with this id is already registered.
    Duplicate(CompactionStrategyId),
    /// The requested strategy is not registered.
    Unknown(String),
    /// Registry or route state was unavailable.
    Unavailable,
    /// Human focus was empty or exceeded its bounded UTF-8 representation.
    InvalidFocus,
    /// The selected strategy has no defined focus semantics.
    FocusUnsupported(CompactionStrategyId),
    /// Strategy/transport/commit failure with bounded safe text.
    Failed(String),
    /// The caller cancelled before settlement.
    Cancelled,
    /// A strategy mutated the log, returned an invalid plan, or raced another
    /// durable writer instead of reaching the registry-owned commit point.
    BrokenTransaction {
        /// Strategy whose settlement was inconsistent.
        strategy: CompactionStrategyId,
        /// Stable contract detail without provider/session content.
        detail: &'static str,
    },
}

impl std::fmt::Display for CompactionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidId => formatter.write_str("compaction strategy id is invalid"),
            Self::Duplicate(id) => write!(formatter, "compaction strategy `{id}` already exists"),
            Self::Unknown(id) => write!(formatter, "unknown compaction strategy `{id}`"),
            Self::Unavailable => formatter.write_str("compaction service is unavailable"),
            Self::InvalidFocus => write!(
                formatter,
                "compaction focus must contain 1 to {MAX_FOCUS_BYTES} UTF-8 bytes"
            ),
            Self::FocusUnsupported(id) => write!(
                formatter,
                "compaction strategy `{id}` does not support human focus instructions"
            ),
            Self::Failed(message) => write!(formatter, "compaction failed: {message}"),
            Self::Cancelled => formatter.write_str("compaction was cancelled"),
            Self::BrokenTransaction { strategy, detail } => write!(
                formatter,
                "compaction strategy `{strategy}` broke its transaction: {detail}"
            ),
        }
    }
}

impl std::error::Error for CompactionError {}

/// Validated compaction-strategy identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CompactionStrategyId(String);

impl CompactionStrategyId {
    /// Validate one kebab-case strategy id.
    ///
    /// # Errors
    /// Empty, oversized, or non-kebab-case input.
    pub fn new(value: impl Into<String>) -> Result<Self, CompactionError> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= MAX_ID_BYTES
            && value.split('-').all(|part| {
                !part.is_empty()
                    && part
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
            });
        if valid {
            Ok(Self(value))
        } else {
            Err(CompactionError::InvalidId)
        }
    }

    /// Exact id.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CompactionStrategyId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Static identity of one compaction strategy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompactionStrategyDescriptor {
    id: CompactionStrategyId,
    kind: CompactionKind,
}

impl CompactionStrategyDescriptor {
    /// Describe one strategy.
    #[must_use]
    pub const fn new(id: CompactionStrategyId, kind: CompactionKind) -> Self {
        Self { id, kind }
    }

    /// Registry identity.
    #[must_use]
    pub const fn id(&self) -> &CompactionStrategyId {
        &self.id
    }

    /// How this strategy reduces context.
    #[must_use]
    pub const fn kind(&self) -> CompactionKind {
        self.kind
    }
}

/// What one committed compaction run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompactionOutcome {
    /// Exactly one durable compaction settlement committed.
    Applied {
        /// Strategy that ran.
        strategy: CompactionStrategyId,
        /// Logged entries whose provider-visible content was replaced.
        folded: usize,
    },
    /// Nothing was worth compacting, and nothing durable changed.
    Noop {
        /// Strategy that declined.
        strategy: CompactionStrategyId,
        /// Stable safe reason.
        reason: &'static str,
    },
}

/// Replacement prepared by one strategy for the registry-owned commit.
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionReplacement {
    /// Portable model-visible text (also used by explicit prune markers).
    Summary(String),
    /// Exact provider-native continuation state and optional normalized usage.
    Native {
        /// Ordered same-route opaque items.
        items: Vec<heycode_core::ProviderStateItem>,
        /// Exact normalized usage, when the provider supplied it.
        usage: Option<heycode_core::TokenUsage>,
    },
}

/// A strategy's side-effect-free durable settlement proposal.
#[derive(Debug, Clone, PartialEq)]
pub enum CompactionPlan {
    /// Replace one exact prefix at the registry commit point.
    Applied {
        /// Number of durable events in the replaced prefix.
        folded: usize,
        /// Highest durable sequence replaced (inclusive).
        replaced_upto_seq: u64,
        /// Replacement representation.
        replacement: CompactionReplacement,
    },
    /// No durable mutation is needed.
    Noop {
        /// Stable safe reason.
        reason: &'static str,
    },
}

/// Read-only services available while a strategy prepares a replacement.
#[derive(Clone)]
pub struct CompactionContext {
    session: Arc<Mutex<Session>>,
    providers: Arc<ProviderRegistry>,
    provider_interception: Arc<ProviderInterception>,
    catalogs: Arc<heycode_llm::CatalogRegistry>,
    selection: Arc<std::sync::RwLock<LlmSelection>>,
    attachments: Arc<crate::agent::AttachmentSlot>,
    bus: EventBus,
}

impl std::fmt::Debug for CompactionContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompactionContext")
            .finish_non_exhaustive()
    }
}

impl CompactionContext {
    pub(crate) fn new(
        session: Arc<Mutex<Session>>,
        providers: Arc<ProviderRegistry>,
        provider_interception: Arc<ProviderInterception>,
        catalogs: Arc<heycode_llm::CatalogRegistry>,
        selection: Arc<std::sync::RwLock<LlmSelection>>,
        attachments: Arc<crate::agent::AttachmentSlot>,
        bus: EventBus,
    ) -> Self {
        Self {
            session,
            providers,
            provider_interception,
            catalogs,
            selection,
            attachments,
            bus,
        }
    }

    /// Clone the current validated durable event sequence for preparation.
    ///
    /// # Errors
    /// Poisoned durable session state fails loud.
    pub fn events(&self) -> Result<Vec<SessionEvent>, CompactionError> {
        self.session
            .lock()
            .map(|session| session.events().to_vec())
            .map_err(|_| CompactionError::Unavailable)
    }

    pub(crate) fn uses_strict_adapter(&self) -> bool {
        self.selection()
            .ok()
            .and_then(|selection| self.providers.get(&selection.provider_name))
            .is_some_and(|provider| provider.inference_adapter().is_some())
    }

    pub(crate) fn request_budget(
        &self,
        request: &heycode_llm::ChatRequest,
        total: &heycode_llm::EnvelopeTotal,
        policy: crate::CompactionPolicy,
        control: &crate::compact::AutoCompactionControl,
    ) -> heycode_llm::ContextBudget {
        let selection = self.selection().ok();
        let provider = selection
            .as_ref()
            .map(|s| s.provider_name.clone())
            .unwrap_or_default();
        let model = self.catalogs.cached(&provider).ok().and_then(|catalog| {
            catalog
                .resolve_model(&request.model, current_unix_ms().ok()?)
                .ok()
                .map(|resolved| resolved.descriptor)
        });
        let mut budget = heycode_llm::context_budget(
            provider,
            request.model.clone(),
            total,
            model.as_ref().and_then(|m| m.context_window),
            policy.context_window,
            request.max_tokens.map_or(0, u64::from),
            policy.threshold_ratio,
            policy.auto,
        );
        control.apply(&mut budget);
        budget
    }

    fn selection(&self) -> Result<LlmSelection, CompactionError> {
        self.selection
            .read()
            .map(|selection| selection.clone())
            .map_err(|_| CompactionError::Unavailable)
    }

    fn attachment_store(&self) -> Option<Arc<heycode_attachments::AttachmentStore>> {
        self.attachments
            .lock()
            .ok()
            .and_then(|slot| slot.as_ref().map(|(store, _)| store.clone()))
    }

    async fn portable_summary(
        &self,
        events: &[SessionEvent],
        focus: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<String, CompactionError> {
        let transcript = heycode_session::derive_messages(events);
        if transcript.is_empty() {
            return Err(CompactionError::Failed(
                "compaction prefix has no model-visible content".to_owned(),
            ));
        }
        let mut excerpt = String::new();
        for message in transcript {
            use std::fmt::Write as _;
            let _ = writeln!(&mut excerpt, "{:?}: {}", message.role, message.content);
        }
        // Every part of the prefix participates. Large histories are reduced
        // hierarchically; never silently discard the tail of the old context.
        let selection = self.selection()?;
        let model = self
            .catalogs
            .cached(&selection.provider_name)
            .ok()
            .and_then(|catalog| {
                catalog
                    .resolve_model(&selection.model, current_unix_ms().ok()?)
                    .ok()
                    .map(|resolved| resolved.descriptor)
            });
        let mut limit = model.as_ref().and_then(|m| m.context_window).map_or(
            MAX_SUMMARIZE_INPUT_CHARS,
            |window| {
                let reserve = model
                    .as_ref()
                    .and_then(|m| m.max_output_tokens)
                    .unwrap_or(2048)
                    .min(2048)
                    .min(window / 4);
                usize::try_from(window.saturating_sub(reserve).saturating_sub(1024))
                    .unwrap_or(usize::MAX)
                    .min(MAX_SUMMARIZE_INPUT_CHARS)
            },
        );
        if let Some(focus) = focus {
            limit = limit.saturating_sub(focused_request_overhead(focus));
        }
        if limit < 256 {
            return Err(CompactionError::Failed(
                "model context is too small for the compaction focus and summary".to_owned(),
            ));
        }
        for _ in 0..8 {
            if excerpt.len() <= limit {
                return self.summarize_excerpt(excerpt, focus, cancellation).await;
            }
            let previous_len = excerpt.len();
            let mut summaries = String::new();
            let mut rest = excerpt.as_str();
            while !rest.is_empty() {
                let mut end = rest.len().min(limit);
                while !rest.is_char_boundary(end) {
                    end -= 1;
                }
                let summary = self
                    .summarize_excerpt(rest[..end].to_owned(), focus, cancellation)
                    .await?;
                summaries.push_str(&summary);
                summaries.push('\n');
                rest = &rest[end..];
            }
            if summaries.len() >= previous_len {
                return Err(CompactionError::Failed(
                    "summaries did not reduce context; original history was retained".to_owned(),
                ));
            }
            excerpt = summaries;
        }
        Err(CompactionError::Failed(
            "summary reduction limit reached; original history was retained".to_owned(),
        ))
    }

    async fn summarize_excerpt(
        &self,
        excerpt: String,
        focus: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<String, CompactionError> {
        self.bus.emit(UiEvent::Status {
            verb: "Compacting…".to_owned(),
        });
        let (system, user) = focus.map_or_else(
            || (SUMMARY_SYSTEM_PROMPT.to_owned(), excerpt.clone()),
            |focus| {
                (
                    format!("{SUMMARY_SYSTEM_PROMPT}{FOCUSED_SUMMARY_SYSTEM_SUFFIX}"),
                    focused_user_message(focus, &excerpt),
                )
            },
        );
        let request = heycode_llm::ChatRequest {
            model: self.selection()?.model,
            messages: vec![
                heycode_llm::ChatMessage::system(system),
                heycode_llm::ChatMessage::user(user),
            ],
            tools: None,
            temperature: None,
            max_tokens: Some(2_048),
        };
        self.auxiliary_text(request, CallPurpose::Compaction, cancellation)
            .await
    }

    /// Shared bounded, tool-free provider dispatch for compaction and asides.
    /// It uses the exact prepared adapter and P10 request seam on every native
    /// provider. It emits no main-turn events and mutates no conversation log.
    pub(crate) async fn auxiliary_text(
        &self,
        request: heycode_llm::ChatRequest,
        purpose: CallPurpose,
        cancellation: &CancellationToken,
    ) -> Result<String, CompactionError> {
        let selection = self.selection()?;
        if request.model != selection.model
            || request
                .tools
                .as_ref()
                .is_some_and(|tools| !tools.is_empty())
        {
            return Err(CompactionError::Failed(
                "auxiliary request route changed or tools were supplied".to_owned(),
            ));
        }
        let provider = self
            .providers
            .get(&selection.provider_name)
            .ok_or_else(|| {
                CompactionError::Failed("selected provider is unavailable".to_owned())
            })?;
        let summary = if provider.inference_adapter().is_some() {
            let effective_at_ms = current_unix_ms()?;
            let refresh = self.catalogs.refresh(
                &selection.provider_name,
                CatalogRefreshMode::PreferCache,
                cancellation.child_token(),
            );
            let catalog = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(CompactionError::Cancelled),
                result = refresh => result.map_err(|_| CompactionError::Failed(
                    "model catalog refresh failed".to_owned()
                ))?,
            };
            let model = catalog
                .snapshot
                .resolve_model(&selection.model, effective_at_ms)
                .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?
                .descriptor;
            let max_output_tokens = Some(
                model
                    .max_output_tokens
                    .unwrap_or(2_048)
                    .min(2_048)
                    .min(model.context_window.map_or(2_048, |window| window / 4)),
            );
            // Same law as a turn request (AGENTS.md: "only the returned exact
            // operation provider may supply options/adapter/P10"). A provider
            // whose registry instance is a lazy stub resolves nothing until
            // it has been prepared, so borrowing the adapter first turns
            // `/compact` — and, with auto-compaction on, every later turn —
            // into a permanent failure.
            let option_context = ProviderOptionContext::new(&model, &[]);
            let prepared = prepare(provider.as_ref(), option_context, cancellation).await?;
            let operation_provider = prepared.as_deref().unwrap_or(provider.as_ref());
            let adapter = operation_provider.inference_adapter().ok_or_else(|| {
                CompactionError::Failed(
                    "prepared provider does not expose an inference adapter".to_owned(),
                )
            })?;
            let provider_options = operation_provider
                .request_options_for(option_context)
                .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
            let draft = RequestDraft {
                provider: selection.provider_name.clone(),
                model: selection.model.clone(),
                catalog_revision: Some(catalog.snapshot.revision),
                catalog_fetched_at_ms: Some(catalog.snapshot.fetched_at_ms),
                effective_at_ms,
                system: request
                    .messages
                    .first()
                    .map(|message| message.content.clone()),
                inputs: request
                    .messages
                    .iter()
                    .filter(|message| message.role != heycode_llm::Role::System)
                    .cloned()
                    .map(InferenceInput::Message)
                    .collect(),
                tools: Vec::new(),
                input_modalities: vec![InputModality::Text],
                reasoning_effort: None,
                structured_output: None,
                native_features: Vec::new(),
                native_tool_routes: Vec::new(),
                provider_options,
                temperature: None,
                max_output_tokens,
                purpose,
            };
            // A portable summary is an ordinary inference request carrying the
            // whole transcript, so P10 sees it exactly as it sees a turn.
            // (AGENTS.md places only *native* compaction outside this seam.)
            let request_context = ProviderRequestContext::new(
                draft.provider.clone(),
                draft.model.clone(),
                draft.purpose,
                adapter.authentication_binding(),
            )
            .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
            let draft = self
                .intercept_request(request_context, draft, cancellation)
                .await?;
            if !draft.tools.is_empty()
                || !draft.native_features.is_empty()
                || !draft.native_tool_routes.is_empty()
            {
                return Err(CompactionError::Failed(
                    "auxiliary text requests cannot use tools".to_owned(),
                ));
            }
            let call = adapter
                .resolve(draft, &model)
                .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
            collect_native_summary(
                adapter.stream_cancellable(call, cancellation.child_token()),
                cancellation,
                purpose != CallPurpose::Compaction,
            )
            .await?
        } else {
            collect_legacy_summary(provider.stream(request), cancellation).await?
        };
        validate_summary(summary)
    }

    /// Run one compaction draft through the P10 request seam.
    async fn intercept_request(
        &self,
        context: ProviderRequestContext,
        draft: RequestDraft,
        cancellation: &CancellationToken,
    ) -> Result<RequestDraft, CompactionError> {
        let interception_cancellation = cancellation.child_token();
        let interception = self.provider_interception.intercept_request(
            context,
            draft,
            interception_cancellation.clone(),
        );
        tokio::pin!(interception);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                interception_cancellation.cancel();
                let _settled = interception.await;
                Err(CompactionError::Cancelled)
            }
            result = &mut interception => match result {
                Ok(draft) => Ok(draft),
                Err(ProviderInterceptionError::Cancelled { .. }) => Err(CompactionError::Cancelled),
                Err(error) => Err(CompactionError::Failed(bounded(&error.to_string()))),
            },
        }
    }

    async fn native_checkpoint(
        &self,
        events: &[SessionEvent],
        cancellation: &CancellationToken,
    ) -> Result<heycode_llm::NativeCompactionCheckpoint, CompactionError> {
        let selection = self.selection()?;
        let provider = self
            .providers
            .get(&selection.provider_name)
            .ok_or_else(|| {
                CompactionError::Failed("selected provider is unavailable".to_owned())
            })?;
        let effective_at_ms = current_unix_ms()?;
        let refresh = self.catalogs.refresh(
            &selection.provider_name,
            CatalogRefreshMode::PreferCache,
            cancellation.child_token(),
        );
        let catalog = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(CompactionError::Cancelled),
            result = refresh => result.map_err(|_| CompactionError::Failed(
                "model catalog refresh failed".to_owned()
            ))?,
        };
        let model = catalog
            .snapshot
            .resolve_model(&selection.model, effective_at_ms)
            .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?
            .descriptor;
        // Preparation comes after catalog/model selection and before anything
        // is borrowed from the provider — the adapter, its descriptor, its
        // native compaction operation and its request options all belong to
        // the exact operation provider, never to the registry instance.
        let option_context = ProviderOptionContext::new(&model, &[]);
        let prepared = prepare(provider.as_ref(), option_context, cancellation).await?;
        let operation_provider = prepared.as_deref().unwrap_or(provider.as_ref());
        let adapter = operation_provider.inference_adapter().ok_or_else(|| {
            CompactionError::Failed("selected provider has no strict inference adapter".to_owned())
        })?;
        let compactor = adapter.native_compaction().ok_or_else(|| {
            CompactionError::Failed(
                "selected provider has no native compaction operation".to_owned(),
            )
        })?;
        let descriptor = adapter.descriptor();
        let protocol = match descriptor.protocols.as_slice() {
            [protocol] if *protocol != heycode_core::ProviderProtocol::Unknown => *protocol,
            _ => {
                return Err(CompactionError::Failed(
                    "native compaction adapter has no exact protocol".to_owned(),
                ));
            }
        };
        let provider_options = operation_provider
            .request_options_for(option_context)
            .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
        let projected =
            heycode_session::project_inputs_for_route(events, &descriptor.id, &model.id, protocol)
                .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
        let attachment_store = self.attachment_store();
        let inputs =
            crate::request_invariant::project_input_list(&projected, attachment_store.as_deref())
                .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
        let mut input_modalities = vec![InputModality::Text];
        if inputs.iter().any(
            |input| matches!(input, InferenceInput::Message(message) if !message.images.is_empty()),
        ) {
            input_modalities.push(InputModality::Image);
        }
        if inputs.iter().any(
            |input| matches!(input, InferenceInput::Message(message) if !message.documents.is_empty()),
        ) {
            input_modalities.push(InputModality::Document);
        }
        // DELIBERATELY NOT intercepted. AGENTS.md keeps "distinct native
        // compaction" outside the P10 seam: this draft carries a
        // provider-native `NativeFeature::Compaction` operation whose opaque
        // checkpoint no request layer can meaningfully revise. The portable
        // summary above, being an ordinary inference request, IS intercepted.
        let call = adapter
            .resolve(
                RequestDraft {
                    provider: selection.provider_name.clone(),
                    model: selection.model.clone(),
                    catalog_revision: Some(catalog.snapshot.revision),
                    catalog_fetched_at_ms: Some(catalog.snapshot.fetched_at_ms),
                    effective_at_ms,
                    system: None,
                    inputs,
                    tools: Vec::new(),
                    input_modalities,
                    reasoning_effort: None,
                    structured_output: None,
                    native_features: vec![NativeFeature::Compaction],
                    native_tool_routes: Vec::new(),
                    provider_options,
                    temperature: None,
                    max_output_tokens: Some(model.max_output_tokens.unwrap_or(8_192).min(8_192)),
                    purpose: CallPurpose::Compaction,
                },
                &model,
            )
            .map_err(|error| CompactionError::Failed(bounded(&error.to_string())))?;
        let expected_provider = call.provider().to_owned();
        let expected_model = call.model().to_owned();
        let expected_protocol = call.protocol();
        let operation = cancellation.child_token();
        let checkpoint = compactor.compact(call, operation.clone());
        tokio::pin!(checkpoint);
        let checkpoint = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                operation.cancel();
                let _settled = checkpoint.await;
                return Err(CompactionError::Cancelled);
            }
            result = &mut checkpoint => result.map_err(map_native_error)?,
        };
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        if checkpoint.provider() != expected_provider
            || checkpoint.model() != expected_model
            || checkpoint.protocol() != expected_protocol
        {
            return Err(CompactionError::Failed(
                "native compaction checkpoint route mismatch".to_owned(),
            ));
        }
        Ok(checkpoint)
    }
}

fn validate_focus(focus: Option<&str>) -> Result<Option<&str>, CompactionError> {
    let Some(focus) = focus.map(str::trim) else {
        return Ok(None);
    };
    if focus.is_empty() || focus.len() > MAX_FOCUS_BYTES {
        Err(CompactionError::InvalidFocus)
    } else {
        Ok(Some(focus))
    }
}

fn focused_user_message(focus: &str, excerpt: &str) -> String {
    let focus_json = serde_json::Value::String(focus.to_owned()).to_string();
    format!("{FOCUS_PREFIX}{focus_json}{TRANSCRIPT_PREFIX}{excerpt}")
}

fn focused_request_overhead(focus: &str) -> usize {
    FOCUSED_SUMMARY_SYSTEM_SUFFIX
        .len()
        .saturating_add(focused_user_message(focus, "").len())
}

/// One way of reducing a session's context.
///
/// Implementations prepare a plan only. Durable mutation is forbidden during
/// this call and is verified before the registry commits the plan.
#[async_trait]
pub trait CompactionStrategy: Send + Sync {
    /// Static identity and kind.
    fn descriptor(&self) -> &CompactionStrategyDescriptor;

    /// Prepare a replacement from a read-only durable snapshot.
    ///
    /// # Errors
    /// Strategy-specific failure or caller cancellation. The durable log must
    /// remain byte-for-byte unchanged on every return path.
    async fn prepare(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError>;

    /// Prepare with optional explicit human summary focus.
    ///
    /// Existing strategies remain source-compatible and reject focus unless
    /// they opt into precise semantics by overriding this method.
    async fn prepare_with_focus(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        focus: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError> {
        if focus.is_some() {
            return Err(CompactionError::FocusUnsupported(
                self.descriptor().id().clone(),
            ));
        }
        self.prepare(context, keep_recent_turns, cancellation).await
    }
}

struct StrategyEntry {
    strategy: Arc<dyn CompactionStrategy>,
    token: Arc<()>,
}

/// Effect-owned registry of compaction strategies.
#[derive(Clone, Default)]
pub struct CompactionRegistry {
    strategies: Arc<Mutex<BTreeMap<CompactionStrategyId, StrategyEntry>>>,
}

impl std::fmt::Debug for CompactionRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CompactionRegistry")
            .field(
                "strategies",
                &self.strategies.lock().map(|rows| rows.len()).unwrap_or(0),
            )
            .finish()
    }
}

impl CompactionRegistry {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one registry-lifetime strategy.
    ///
    /// # Errors
    /// Duplicate id or poisoned registry state.
    pub fn register(&self, strategy: Arc<dyn CompactionStrategy>) -> Result<(), CompactionError> {
        let _ = self.insert(strategy)?;
        Ok(())
    }

    /// Register one strategy as an owning Context effect.
    ///
    /// # Errors
    /// Duplicate id or poisoned registry state. No disposer is installed when
    /// insertion fails.
    pub fn register_effect(
        &self,
        context: &heycode_core::Context,
        strategy: Arc<dyn CompactionStrategy>,
    ) -> Result<(), CompactionError> {
        let (id, token) = self.insert(strategy)?;
        let strategies = Arc::downgrade(&self.strategies);
        context.effect(move || {
            let Some(strategies) = strategies.upgrade() else {
                return;
            };
            let Ok(mut strategies) = strategies.lock() else {
                return;
            };
            if strategies
                .get(&id)
                .is_some_and(|entry| Arc::ptr_eq(&entry.token, &token))
            {
                strategies.remove(&id);
            }
        });
        Ok(())
    }

    fn insert(
        &self,
        strategy: Arc<dyn CompactionStrategy>,
    ) -> Result<(CompactionStrategyId, Arc<()>), CompactionError> {
        let id = strategy.descriptor().id().clone();
        let mut strategies = self
            .strategies
            .lock()
            .map_err(|_| CompactionError::Unavailable)?;
        if strategies.contains_key(&id) {
            return Err(CompactionError::Duplicate(id));
        }
        let token = Arc::new(());
        strategies.insert(
            id.clone(),
            StrategyEntry {
                strategy,
                token: token.clone(),
            },
        );
        Ok((id, token))
    }

    /// Remove one strategy by id, returning whether a row was removed.
    pub fn remove(&self, id: &CompactionStrategyId) -> bool {
        self.strategies
            .lock()
            .map(|mut strategies| strategies.remove(id).is_some())
            .unwrap_or(false)
    }

    /// Registered descriptors in stable id order.
    #[must_use]
    pub fn descriptors(&self) -> Vec<CompactionStrategyDescriptor> {
        self.strategies
            .lock()
            .map(|strategies| {
                strategies
                    .values()
                    .map(|entry| entry.strategy.descriptor().clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Drop every strategy. Used by the owning effect's disposer.
    pub fn dispose(&self) {
        if let Ok(mut strategies) = self.strategies.lock() {
            strategies.clear();
        }
    }

    /// Run one named strategy and commit its plan exactly once.
    ///
    /// # Errors
    /// Unknown strategy, cancellation, strategy/transport failure, invalid
    /// plan, pre-commit mutation, concurrent durable mutation, or append
    /// failure.
    pub async fn compact(
        &self,
        context: &CompactionContext,
        strategy_id: &str,
        keep_recent_turns: u64,
        cancellation: &CancellationToken,
    ) -> Result<CompactionOutcome, CompactionError> {
        self.compact_with_focus(context, strategy_id, keep_recent_turns, None, cancellation)
            .await
    }

    /// Run one named strategy with optional explicit human summary focus.
    ///
    /// # Errors
    /// In addition to [`Self::compact`], empty/oversized focus and focus on a
    /// strategy without defined focus semantics are refused before commit.
    pub async fn compact_with_focus(
        &self,
        context: &CompactionContext,
        strategy_id: &str,
        keep_recent_turns: u64,
        focus: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<CompactionOutcome, CompactionError> {
        let focus = validate_focus(focus)?;
        let id = CompactionStrategyId::new(strategy_id)?;
        let strategy = self
            .strategies
            .lock()
            .map_err(|_| CompactionError::Unavailable)?
            .get(&id)
            .map(|entry| entry.strategy.clone())
            .ok_or_else(|| CompactionError::Unknown(strategy_id.to_owned()))?;
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        let kind = strategy.descriptor().kind();
        let before = durable_state(context)?;
        let plan = strategy
            .prepare_with_focus(context, keep_recent_turns, focus, cancellation)
            .await;
        let after_prepare = durable_state(context)?;
        if after_prepare != before {
            return Err(CompactionError::BrokenTransaction {
                strategy: id,
                detail: "strategy preparation must not mutate the durable log",
            });
        }
        let plan = plan?;
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        match plan {
            CompactionPlan::Noop { reason } => Ok(CompactionOutcome::Noop {
                strategy: id,
                reason,
            }),
            CompactionPlan::Applied {
                folded,
                replaced_upto_seq,
                replacement,
            } => {
                validate_plan(
                    context,
                    &before,
                    &id,
                    kind,
                    folded,
                    replaced_upto_seq,
                    &replacement,
                )?;
                let kind = match replacement {
                    CompactionReplacement::Summary(summary) => {
                        SessionEventKind::CompactionApplied {
                            summary,
                            replaced_upto_seq,
                        }
                    }
                    CompactionReplacement::Native { items, usage } => {
                        SessionEventKind::NativeCompactionApplied {
                            strategy: id.as_str().to_owned(),
                            replaced_upto_seq,
                            items,
                            usage,
                        }
                    }
                };
                {
                    let mut session = context
                        .session
                        .lock()
                        .map_err(|_| CompactionError::Unavailable)?;
                    if session.events().len() != before.events
                        || count_compactions(&session) != before.compactions
                    {
                        return Err(CompactionError::BrokenTransaction {
                            strategy: id,
                            detail: "durable log changed before the compaction commit",
                        });
                    }
                    session.append(kind).map_err(|_| {
                        CompactionError::Failed("durable compaction commit failed".to_owned())
                    })?;
                }
                let after = durable_state(context)?;
                if after.events != before.events.saturating_add(1)
                    || after.compactions != before.compactions.saturating_add(1)
                {
                    return Err(CompactionError::BrokenTransaction {
                        strategy: id,
                        detail: "registry commit must append one compaction settlement",
                    });
                }
                context.bus.emit(UiEvent::Info {
                    text: format!(
                        "compaction `{}` folded {folded} logged entries",
                        id.as_str()
                    ),
                });
                Ok(CompactionOutcome::Applied {
                    strategy: id,
                    folded,
                })
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct DurableState {
    events: usize,
    compactions: usize,
}

fn durable_state(context: &CompactionContext) -> Result<DurableState, CompactionError> {
    let session = context
        .session
        .lock()
        .map_err(|_| CompactionError::Unavailable)?;
    Ok(DurableState {
        events: session.events().len(),
        compactions: count_compactions(&session),
    })
}

fn count_compactions(session: &Session) -> usize {
    session
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event.kind,
                SessionEventKind::CompactionApplied { .. }
                    | SessionEventKind::NativeCompactionApplied { .. }
            )
        })
        .count()
}

fn validate_plan(
    context: &CompactionContext,
    before: &DurableState,
    strategy: &CompactionStrategyId,
    kind: CompactionKind,
    folded: usize,
    replaced_upto_seq: u64,
    replacement: &CompactionReplacement,
) -> Result<(), CompactionError> {
    let events = context.events()?;
    let covered = events
        .iter()
        .filter(|event| event.seq <= replaced_upto_seq)
        .count();
    if events.len() != before.events || folded == 0 || covered != folded {
        return Err(CompactionError::BrokenTransaction {
            strategy: strategy.clone(),
            detail: "applied plan must identify one exact nonempty durable prefix",
        });
    }
    match replacement {
        CompactionReplacement::Summary(_) if kind == CompactionKind::Native => {
            Err(CompactionError::BrokenTransaction {
                strategy: strategy.clone(),
                detail: "native strategy must produce provider-native state",
            })
        }
        CompactionReplacement::Native { .. } if kind != CompactionKind::Native => {
            Err(CompactionError::BrokenTransaction {
                strategy: strategy.clone(),
                detail: "portable or prune strategy must produce portable text",
            })
        }
        CompactionReplacement::Summary(summary)
            if summary.trim().is_empty() || summary.len() > MAX_SUMMARY_BYTES =>
        {
            Err(CompactionError::BrokenTransaction {
                strategy: strategy.clone(),
                detail: "portable replacement must be nonempty and bounded",
            })
        }
        CompactionReplacement::Native { items, .. } if !valid_native_items(items) => {
            Err(CompactionError::BrokenTransaction {
                strategy: strategy.clone(),
                detail: "native replacement must contain one bounded exact route",
            })
        }
        CompactionReplacement::Summary(_) | CompactionReplacement::Native { .. } => Ok(()),
    }
}

fn valid_native_items(items: &[heycode_core::ProviderStateItem]) -> bool {
    let Some(first) = items.first() else {
        return false;
    };
    items.len() <= 256
        && first.protocol() != heycode_core::ProviderProtocol::Unknown
        && items.iter().all(|item| {
            item.validate().is_ok()
                && item.provider() == first.provider()
                && item.model() == first.model()
                && item.protocol() == first.protocol()
        })
}

/// The portable model-written summary strategy.
pub struct PortableCompaction {
    descriptor: CompactionStrategyDescriptor,
}

impl PortableCompaction {
    /// Stable registry id.
    pub const ID: &'static str = "portable-summary";

    /// Build the portable strategy.
    ///
    /// # Errors
    /// Impossible static id validation failure.
    pub fn new() -> Result<Self, CompactionError> {
        Ok(Self {
            descriptor: CompactionStrategyDescriptor::new(
                CompactionStrategyId::new(Self::ID)?,
                CompactionKind::Portable,
            ),
        })
    }

    async fn prepare_summary(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        focus: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError> {
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        let events = context.events()?;
        let Some(boundary) = crate::compact::fold_boundary_of(&events, keep_recent_turns) else {
            return Ok(CompactionPlan::Noop {
                reason: "fewer turns than the keep window",
            });
        };
        let summary = context
            .portable_summary(&events[..boundary], focus, cancellation)
            .await?;
        Ok(CompactionPlan::Applied {
            folded: boundary,
            replaced_upto_seq: events[boundary - 1].seq,
            replacement: CompactionReplacement::Summary(summary),
        })
    }
}

#[async_trait]
impl CompactionStrategy for PortableCompaction {
    fn descriptor(&self) -> &CompactionStrategyDescriptor {
        &self.descriptor
    }

    async fn prepare(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError> {
        self.prepare_summary(context, keep_recent_turns, None, cancellation)
            .await
    }

    async fn prepare_with_focus(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        focus: Option<&str>,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError> {
        self.prepare_summary(context, keep_recent_turns, focus, cancellation)
            .await
    }
}

/// Exact text prune commits in place of a summary.
pub const PRUNE_MARKER: &str =
    "[earlier conversation was dropped to free context; it was not summarized]";

/// The explicit lossy prune strategy.
pub struct PruneCompaction {
    descriptor: CompactionStrategyDescriptor,
}

impl PruneCompaction {
    /// Stable registry id.
    pub const ID: &'static str = "prune-oldest";

    /// Build the prune strategy.
    ///
    /// # Errors
    /// Impossible static id validation failure.
    pub fn new() -> Result<Self, CompactionError> {
        Ok(Self {
            descriptor: CompactionStrategyDescriptor::new(
                CompactionStrategyId::new(Self::ID)?,
                CompactionKind::Prune,
            ),
        })
    }
}

#[async_trait]
impl CompactionStrategy for PruneCompaction {
    fn descriptor(&self) -> &CompactionStrategyDescriptor {
        &self.descriptor
    }

    async fn prepare(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError> {
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        let events = context.events()?;
        let Some(boundary) = crate::compact::fold_boundary_of(&events, keep_recent_turns) else {
            return Ok(CompactionPlan::Noop {
                reason: "fewer turns than the keep window",
            });
        };
        Ok(CompactionPlan::Applied {
            folded: boundary,
            replaced_upto_seq: events[boundary - 1].seq,
            replacement: CompactionReplacement::Summary(PRUNE_MARKER.to_owned()),
        })
    }
}

/// Provider-native opaque-checkpoint strategy.
pub struct NativeCompaction {
    descriptor: CompactionStrategyDescriptor,
}

impl NativeCompaction {
    /// Stable registry id.
    pub const ID: &'static str = "provider-native";

    /// Build the native strategy.
    ///
    /// # Errors
    /// Impossible static id validation failure.
    pub fn new() -> Result<Self, CompactionError> {
        Ok(Self {
            descriptor: CompactionStrategyDescriptor::new(
                CompactionStrategyId::new(Self::ID)?,
                CompactionKind::Native,
            ),
        })
    }
}

#[async_trait]
impl CompactionStrategy for NativeCompaction {
    fn descriptor(&self) -> &CompactionStrategyDescriptor {
        &self.descriptor
    }

    async fn prepare(
        &self,
        context: &CompactionContext,
        keep_recent_turns: u64,
        cancellation: &CancellationToken,
    ) -> Result<CompactionPlan, CompactionError> {
        if cancellation.is_cancelled() {
            return Err(CompactionError::Cancelled);
        }
        let events = context.events()?;
        let Some(boundary) = crate::compact::fold_boundary_of(&events, keep_recent_turns) else {
            return Ok(CompactionPlan::Noop {
                reason: "fewer turns than the keep window",
            });
        };
        let checkpoint = context
            .native_checkpoint(&events[..boundary], cancellation)
            .await?;
        let (items, usage) = checkpoint.into_parts();
        Ok(CompactionPlan::Applied {
            folded: boundary,
            replaced_upto_seq: events[boundary - 1].seq,
            replacement: CompactionReplacement::Native { items, usage },
        })
    }
}

/// Publish the default native/portable/prune registry.
#[must_use]
pub fn compactions_plugin() -> Box<dyn heycode_core::Plugin> {
    struct CompactionsPlugin;

    impl heycode_core::Plugin for CompactionsPlugin {
        fn name(&self) -> &'static str {
            "compactions"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Service],
            )
        }

        fn provides(&self) -> &'static [heycode_core::ServiceKey] {
            &[crate::SERVICE_COMPACTIONS]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let registry = CompactionRegistry::new();
            for strategy in [
                Arc::new(PortableCompaction::new().map_err(map_core)?)
                    as Arc<dyn CompactionStrategy>,
                Arc::new(NativeCompaction::new().map_err(map_core)?),
                Arc::new(PruneCompaction::new().map_err(map_core)?),
            ] {
                registry.register(strategy).map_err(map_core)?;
            }
            context.provide(crate::SERVICE_COMPACTIONS, self.name(), registry)?;
            let published = context
                .get::<CompactionRegistry>(crate::SERVICE_COMPACTIONS)
                .ok_or_else(|| heycode_core::CoreError::other("compactions service missing"))?;
            context.effect(move || published.dispose());
            Ok(())
        }
    }

    Box::new(CompactionsPlugin)
}

fn map_core(error: CompactionError) -> heycode_core::CoreError {
    heycode_core::CoreError::other(error.to_string())
}

async fn collect_legacy_summary(
    mut stream: heycode_llm::ChunkStream,
    cancellation: &CancellationToken,
) -> Result<String, CompactionError> {
    let mut summary = String::new();
    let mut finished = false;
    loop {
        let next = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(CompactionError::Cancelled),
            next = stream.next() => next,
        };
        let Some(item) = next else {
            break;
        };
        if finished {
            return Err(CompactionError::Failed(
                "summary stream emitted after finish".to_owned(),
            ));
        }
        match item.map_err(|error| CompactionError::Failed(bounded(&error.to_string())))? {
            heycode_llm::StreamChunk::TextDelta(text) => append_summary(&mut summary, &text)?,
            heycode_llm::StreamChunk::ReasoningDelta(_) | heycode_llm::StreamChunk::Usage(_) => {}
            heycode_llm::StreamChunk::ToolCallDelta { .. } => {
                return Err(CompactionError::Failed(
                    "summary request returned a tool call".to_owned(),
                ));
            }
            heycode_llm::StreamChunk::Finish(
                heycode_llm::FinishReason::Stop | heycode_llm::FinishReason::Length,
            ) => finished = true,
            heycode_llm::StreamChunk::Finish(
                heycode_llm::FinishReason::ToolCalls | heycode_llm::FinishReason::Pause,
            ) => {
                return Err(CompactionError::Failed(
                    "summary request did not settle as text".to_owned(),
                ));
            }
        }
    }
    if !finished {
        return Err(CompactionError::Failed(
            "summary stream ended before finish".to_owned(),
        ));
    }
    Ok(summary)
}

async fn collect_native_summary(
    mut stream: heycode_llm::InferenceStream,
    cancellation: &CancellationToken,
    discard_auxiliary_state: bool,
) -> Result<String, CompactionError> {
    let mut summary = String::new();
    let mut finished = false;
    loop {
        let next = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Err(CompactionError::Cancelled),
            next = stream.next() => next,
        };
        let Some(item) = next else {
            break;
        };
        if finished {
            return Err(CompactionError::Failed(
                "summary stream emitted after finish".to_owned(),
            ));
        }
        match item.map_err(|error| CompactionError::Failed(bounded(&error.to_string())))? {
            InferenceEvent::TextDelta(text) => append_summary(&mut summary, &text)?,
            InferenceEvent::ProviderState(_) if discard_auxiliary_state => {}
            InferenceEvent::ToolCallDelta { .. }
            | InferenceEvent::ServerToolCall { .. }
            | InferenceEvent::ServerToolResult { .. }
            | InferenceEvent::ServerToolUsage(_)
            | InferenceEvent::Citation { .. } => {
                return Err(CompactionError::Failed(
                    "summary request returned non-summary output".to_owned(),
                ));
            }
            InferenceEvent::ProviderState(state) => {
                // Every real adapter emits its replay envelope even for plain
                // text. A portable summary consumes only TextDelta; never
                // persist this envelope or treat it as a native checkpoint.
                if !summary_state_is_text(&state) {
                    return Err(CompactionError::Failed(
                        "summary request returned non-text provider state".to_owned(),
                    ));
                }
            }
            InferenceEvent::Finish(
                heycode_llm::FinishReason::Stop | heycode_llm::FinishReason::Length,
            ) => finished = true,
            InferenceEvent::Finish(
                heycode_llm::FinishReason::ToolCalls | heycode_llm::FinishReason::Pause,
            ) => {
                return Err(CompactionError::Failed(
                    "summary request did not settle as text".to_owned(),
                ));
            }
            InferenceEvent::ResponseStarted { .. }
            | InferenceEvent::ItemStarted { .. }
            | InferenceEvent::ReasoningDelta(_)
            | InferenceEvent::ItemFinished { .. }
            | InferenceEvent::ResponseFinished { .. }
            | InferenceEvent::ResponseMetadata(_)
            | InferenceEvent::Usage(_) => {}
        }
    }
    if !finished {
        return Err(CompactionError::Failed(
            "summary stream ended before finish".to_owned(),
        ));
    }
    Ok(summary)
}

fn summary_state_is_text(state: &heycode_core::ProviderStateItem) -> bool {
    use heycode_core::ProviderStateKind;
    if state.validate().is_err() {
        return false;
    }
    let data = state.data();
    let blocks = |field: &str, allowed: &[&str]| {
        data.get(field)
            .and_then(serde_json::Value::as_array)
            .is_some_and(|parts| {
                parts.iter().all(|part| {
                    part.get("type")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|kind| allowed.contains(&kind))
                })
            })
    };
    match state.kind() {
        ProviderStateKind::ChatAssistantMessage => {
            data.get("tool_calls")
                .is_none_or(|calls| calls.is_null() || calls.as_array().is_some_and(Vec::is_empty))
                && data
                    .get("function_call")
                    .is_none_or(serde_json::Value::is_null)
                && data
                    .get("content")
                    .is_none_or(|value| value.is_null() || value.is_string())
        }
        ProviderStateKind::ResponseOutputItem => {
            match data.get("type").and_then(serde_json::Value::as_str) {
                Some("reasoning") => true,
                Some("message") => blocks("content", &["output_text"]),
                _ => false,
            }
        }
        ProviderStateKind::AnthropicMessage => {
            blocks("content", &["text", "thinking", "redacted_thinking"])
        }
        ProviderStateKind::GeminiModelContent => data
            .get("parts")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|parts| {
                parts.iter().all(|part| {
                    part.as_object().is_some_and(|object| {
                        !object.is_empty()
                            && object.keys().all(|key| {
                                matches!(key.as_str(), "text" | "thought" | "thoughtSignature")
                            })
                    })
                })
            }),
        ProviderStateKind::BedrockConverseMessage => data
            .get("content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|parts| {
                parts.iter().all(|part| {
                    part.as_object().is_some_and(|object| {
                        !object.is_empty()
                            && object
                                .keys()
                                .all(|key| matches!(key.as_str(), "text" | "reasoningContent"))
                    })
                })
            }),
    }
}

fn append_summary(summary: &mut String, delta: &str) -> Result<(), CompactionError> {
    if summary.len().saturating_add(delta.len()) > MAX_SUMMARY_BYTES {
        return Err(CompactionError::Failed(
            "summary exceeds the durable size limit".to_owned(),
        ));
    }
    summary.push_str(delta);
    Ok(())
}

fn validate_summary(summary: String) -> Result<String, CompactionError> {
    if summary.trim().is_empty() || summary.len() > MAX_SUMMARY_BYTES {
        Err(CompactionError::Failed(
            "summarizer returned an empty or oversized response".to_owned(),
        ))
    } else {
        Ok(summary)
    }
}

fn map_native_error(error: heycode_llm::NativeCompactionError) -> CompactionError {
    if matches!(error, heycode_llm::NativeCompactionError::Cancelled) {
        CompactionError::Cancelled
    } else {
        CompactionError::Failed(error.to_string())
    }
}

fn current_unix_ms() -> Result<u64, CompactionError> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CompactionError::Failed("system clock is before the Unix epoch".to_owned()))?;
    u64::try_from(elapsed.as_millis())
        .map_err(|_| CompactionError::Failed("system clock exceeds supported range".to_owned()))
}

fn bounded(message: &str) -> String {
    message
        .replace(['\n', '\r'], " ")
        .chars()
        .take(256)
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn portable_summary_rejects_hidden_tool_state_and_never_commits_it() {
        let state = heycode_core::ProviderStateItem::new("provider", "model",
            heycode_core::ProviderProtocol::OpenAiChatCompletions,
            heycode_core::ProviderStateKind::ChatAssistantMessage,
            serde_json::json!({"role":"assistant","content":"summary", "tool_calls":[{"id":"hidden","type":"function","function":{"name":"bash","arguments":"{}"}}]}),
        ).unwrap();
        let stream = Box::pin(futures::stream::iter(vec![
            Ok(InferenceEvent::TextDelta("summary".into())),
            Ok(InferenceEvent::ProviderState(state)),
            Ok(InferenceEvent::Finish(heycode_llm::FinishReason::Stop)),
        ]));
        assert!(
            collect_native_summary(stream, &CancellationToken::new(), false)
                .await
                .is_err()
        );
    }

    #[test]
    fn strategy_ids_validate_and_duplicates_fail_loud() {
        assert!(CompactionStrategyId::new("portable-summary").is_ok());
        assert_eq!(
            CompactionStrategyId::new("Portable").unwrap_err(),
            CompactionError::InvalidId
        );
        assert_eq!(
            CompactionStrategyId::new("").unwrap_err(),
            CompactionError::InvalidId
        );
        assert_eq!(
            CompactionStrategyId::new("trailing-").unwrap_err(),
            CompactionError::InvalidId
        );
    }

    #[test]
    fn kinds_have_stable_names_and_order() {
        assert_eq!(CompactionKind::Native.name(), "native");
        assert_eq!(CompactionKind::Portable.name(), "portable");
        assert_eq!(CompactionKind::Prune.name(), "prune");
        assert!(CompactionKind::Native < CompactionKind::Portable);
        assert!(CompactionKind::Portable < CompactionKind::Prune);
    }

    #[test]
    fn bounded_errors_are_single_line_and_utf8_safe() {
        let message = format!("a\n{}", "é".repeat(400));
        let bounded = bounded(&message);
        assert!(!bounded.contains('\n'));
        assert!(bounded.chars().count() <= 256);
    }
}
