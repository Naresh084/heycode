//! Direct native coding turns through MiniMax's documented Chat endpoint.

use std::sync::Arc;

use futures::StreamExt as _;
use heycode_credentials::{CredentialResolutionError, CredentialSecret, CredentialsService};
use heycode_http::HttpService;
use heycode_llm::{
    AuthenticationBinding, ChatRequest, ChunkStream, CredentialHandle, CredentialResolver,
    InferenceAdapter, InferenceEvent, InferenceInput, InferenceStream, LlmError, ModelDescriptor,
    OpenAiChatCompletionsAdapter, OpenAiChatCompletionsConfig, Provider, ProviderDescriptor,
    ProviderInfo, RequestDraft, ResolveError, ResolvedCall, RouteCredential,
};
use tokio_util::sync::CancellationToken;

use crate::{
    MiniMaxApiFamily, MiniMaxPlan, MiniMaxProfile, MiniMaxReasoningGuarantee, MiniMaxStateDialect,
    MiniMaxStateRoute, normalize_model,
};

struct PlanCredential<P: MiniMaxPlan> {
    profile: MiniMaxProfile<P>,
    credentials: CredentialsService,
    route: CredentialHandle,
}

impl<P: MiniMaxPlan> CredentialResolver for PlanCredential<P> {
    fn route(&self) -> &CredentialHandle {
        &self.route
    }
    fn resolve(
        &self,
        route: &CredentialHandle,
    ) -> Result<CredentialSecret, CredentialResolutionError> {
        if route != &self.route {
            return Err(CredentialResolutionError::RouteMismatch {
                reference: route.as_str().into(),
            });
        }
        let value = self.profile.resolve(&self.credentials).map_err(|_| {
            CredentialResolutionError::Unavailable {
                reference: route.as_str().into(),
                message: "MiniMax plan credential is missing or invalid".into(),
            }
        })?;
        Ok(CredentialSecret::new(value.expose()))
    }
}

/// Plan-bound, rotating-credential MiniMax route. Uses native think tags so the
/// complete assistant content and tool calls retain one ordered replay unit.
/// Catalog Unknown tools are explicitly attempted; evidence remains Unknown.
pub struct MiniMaxInference<P: MiniMaxPlan> {
    profile: MiniMaxProfile<P>,
    model: String,
    adapter: OpenAiChatCompletionsAdapter,
}

impl<P: MiniMaxPlan> MiniMaxInference<P> {
    /// Construct the documented regional Chat route without acquiring a key.
    ///
    /// # Errors
    /// Undocumented regional routes or malformed model/configuration fail locally.
    pub fn new(
        http: HttpService,
        credentials: CredentialsService,
        profile: MiniMaxProfile<P>,
        model: String,
    ) -> Result<Self, LlmError> {
        if profile.documented_base_url(MiniMaxApiFamily::OpenAiCompatible)
            != heycode_llm::CapabilitySupport::Supported
        {
            return Err(LlmError::Transport(
                "MiniMax regional Chat endpoint is not documented".into(),
            ));
        }
        if model.is_empty()
            || model.trim() != model
            || model.len() > 256
            || model.chars().any(char::is_control)
        {
            return Err(LlmError::Transport("MiniMax model id is invalid".into()));
        }
        let query = profile
            .credential_query()
            .map_err(|_| LlmError::Transport("MiniMax credential query is invalid".into()))?;
        let route = CredentialHandle::from_reference(&query.reference);
        let credential = RouteCredential::per_operation(
            route.clone(),
            Arc::new(PlanCredential {
                profile,
                credentials,
                route,
            }),
        );
        let adapter = OpenAiChatCompletionsAdapter::new(
            OpenAiChatCompletionsConfig::with_credential(
                chat_descriptor(profile),
                profile.base_url(MiniMaxApiFamily::OpenAiCompatible),
                credential,
            )
            .with_unknown_tool_attempts()
            .with_reasoning_split(false)
            .with_image_detail("default"),
            http,
        )?;
        Ok(Self {
            profile,
            model,
            adapter,
        })
    }

    fn state_route(&self, model: &str) -> Result<MiniMaxStateRoute<P>, ResolveError> {
        MiniMaxStateRoute::new(
            self.profile,
            MiniMaxStateDialect::ChatThinkTags,
            model,
            if crate::documented_model_ids().any(|id| id == model) {
                MiniMaxReasoningGuarantee::Always
            } else {
                MiniMaxReasoningGuarantee::Optional
            },
        )
        .map_err(|error| ResolveError::InvalidRequest {
            field: "provider_state",
            message: error.to_string(),
        })
    }
}

impl<P: MiniMaxPlan> Provider for MiniMaxInference<P> {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: P::ID.registry_name().into(),
            default_model: self.model.clone(),
        }
    }
    fn descriptor(&self) -> ProviderDescriptor {
        self.profile.provider_descriptor()
    }
    fn credential_reference(&self) -> Option<&str> {
        Some(P::ID.credential_reference())
    }
    fn describe_model(&self, model: &str) -> ModelDescriptor {
        normalize_model(model.into(), None)
    }
    fn inference_adapter(&self) -> Option<&dyn InferenceAdapter> {
        Some(self)
    }
    fn stream(&self, _: ChatRequest) -> ChunkStream {
        Box::pin(futures::stream::once(async {
            Err(LlmError::InvalidResponse(
                "MiniMax requires the strict inference path for thinking continuation".into(),
            ))
        }))
    }
}

impl<P: MiniMaxPlan> InferenceAdapter for MiniMaxInference<P> {
    fn descriptor(&self) -> ProviderDescriptor {
        chat_descriptor(self.profile)
    }
    fn authentication_binding(&self) -> AuthenticationBinding {
        self.adapter.authentication_binding()
    }
    fn resolve(
        &self,
        draft: RequestDraft,
        model: &ModelDescriptor,
    ) -> Result<ResolvedCall, ResolveError> {
        let state_route = self.state_route(&model.id)?;
        if draft
            .temperature
            .is_some_and(|t| !t.is_finite() || !(0.0..=2.0).contains(&t))
        {
            return Err(ResolveError::InvalidRequest {
                field: "temperature",
                message: "MiniMax temperature must be between 0 and 2".into(),
            });
        }
        for input in &draft.inputs {
            match input {
                InferenceInput::ProviderState(state) => {
                    state_route
                        .replay(state)
                        .map_err(|error| ResolveError::InvalidRequest {
                            field: "provider_state",
                            message: error.to_string(),
                        })?;
                }
                InferenceInput::Message(message)
                    if message
                        .tool_calls
                        .as_ref()
                        .is_some_and(|calls| !calls.is_empty()) =>
                {
                    return Err(ResolveError::InvalidRequest {
                        field: "provider_state",
                        message: "MiniMax tool continuation requires its complete assistant state"
                            .into(),
                    });
                }
                _ => {}
            }
        }
        self.adapter.resolve(draft, model)
    }
    fn stream(&self, call: ResolvedCall) -> InferenceStream {
        self.stream_cancellable(call, CancellationToken::new())
    }
    fn stream_cancellable(
        &self,
        call: ResolvedCall,
        cancellation: CancellationToken,
    ) -> InferenceStream {
        let route = match self.state_route(call.model()) {
            Ok(route) => route,
            Err(error) => {
                return Box::pin(futures::stream::once(async move {
                    Err(LlmError::InvalidResponse(error.to_string()))
                }));
            }
        };
        let stream = self.adapter.stream_cancellable(call, cancellation);
        let mut text = ThinkingText::default();
        Box::pin(
            stream
                .scan(false, move |failed, event| {
                    let mut output = Vec::new();
                    if *failed {
                        return std::future::ready(None);
                    }
                    match event {
                        Ok(InferenceEvent::TextDelta(delta)) => output.extend(text.push(&delta)),
                        Ok(InferenceEvent::ItemFinished {
                            output_index,
                            item_id,
                            kind,
                        }) => {
                            if text.thinking {
                                *failed = true;
                                output.push(Err(LlmError::InvalidResponse(
                                    "MiniMax thinking content is truncated".into(),
                                )));
                            } else {
                                if !text.pending.is_empty() {
                                    output.push(Ok(InferenceEvent::TextDelta(std::mem::take(
                                        &mut text.pending,
                                    ))));
                                }
                                output.push(Ok(InferenceEvent::ItemFinished {
                                    output_index,
                                    item_id,
                                    kind,
                                }));
                            }
                        }
                        Ok(InferenceEvent::ProviderState(state)) => match route.replay(&state) {
                            Ok(_) => output.push(Ok(InferenceEvent::ProviderState(state))),
                            Err(error) => {
                                *failed = true;
                                output.push(Err(LlmError::InvalidResponse(error.to_string())));
                            }
                        },
                        Err(error) => {
                            *failed = true;
                            output.push(Err(error));
                        }
                        other => output.push(other),
                    }
                    std::future::ready(Some(futures::stream::iter(output)))
                })
                .flatten(),
        )
    }
}

fn chat_descriptor<P: MiniMaxPlan>(profile: MiniMaxProfile<P>) -> ProviderDescriptor {
    let mut descriptor = profile.provider_descriptor();
    descriptor.protocols = vec![heycode_core::ProviderProtocol::OpenAiChatCompletions];
    descriptor
}

/// Only the visible event projection is split. The shared parser's complete
/// assistant state is preserved verbatim for the next provider request.
#[derive(Default)]
struct ThinkingText {
    pending: String,
    thinking: bool,
}
impl ThinkingText {
    fn push(&mut self, delta: &str) -> Vec<Result<InferenceEvent, LlmError>> {
        self.pending.push_str(delta);
        let mut output = Vec::new();
        loop {
            let marker = if self.thinking { "</think>" } else { "<think>" };
            let position = self.pending.find(marker);
            let emit = position.unwrap_or_else(|| {
                let retained = (1..marker.len())
                    .rev()
                    .find(|n| self.pending.ends_with(&marker[..*n]))
                    .unwrap_or(0);
                self.pending.len() - retained
            });
            if emit > 0 {
                let value: String = self.pending.drain(..emit).collect();
                output.push(Ok(if self.thinking {
                    InferenceEvent::ReasoningDelta(value)
                } else {
                    InferenceEvent::TextDelta(value)
                }));
            }
            if position.is_none() {
                break;
            }
            self.pending.drain(..marker.len());
            self.thinking = !self.thinking;
        }
        output
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn think_markers_can_be_split_at_every_utf8_boundary_without_leaking_tags() {
        let input = "<think>Check café carefully.</think>Done.";
        for split in (0..=input.len()).filter(|n| input.is_char_boundary(*n)) {
            let mut parser = ThinkingText::default();
            let events = parser
                .push(&input[..split])
                .into_iter()
                .chain(parser.push(&input[split..]))
                .collect::<Vec<_>>();
            let text = events
                .iter()
                .filter_map(|e| {
                    if let Ok(InferenceEvent::TextDelta(s)) = e {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>();
            let thinking = events
                .iter()
                .filter_map(|e| {
                    if let Ok(InferenceEvent::ReasoningDelta(s)) = e {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .collect::<String>();
            assert_eq!(text, "Done.");
            assert_eq!(thinking, "Check café carefully.");
            assert!(!parser.thinking);
            assert!(parser.pending.is_empty());
        }
    }
}
