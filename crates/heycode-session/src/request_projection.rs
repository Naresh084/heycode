//! Correlated, protocol-aware request reconstruction from v2 events.

use std::collections::{BTreeSet, HashMap, HashSet};

use crate::{
    RequestContextSnapshot, RequestHeaderSnapshot, Role, SessionEvent, SessionEventKind,
    WireMessage, WireToolCall,
};

/// One ordered model input reconstructed for a durable request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectedInput {
    /// Provider-neutral message/tool result.
    Message(WireMessage),
    /// Same-route lossless provider item replacing its generic assistant copy.
    ProviderState(heycode_core::ProviderStateItem),
}

/// One normalized provider server-tool/citation event produced by a request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProjectedServerToolEvent {
    /// Provider-executed call.
    Call {
        /// Durable event sequence.
        seq: u64,
        /// Provider output/block index.
        output_index: u32,
        /// Safe normalized call metadata.
        call: heycode_core::ServerToolCall,
    },
    /// Provider-executed result.
    Result {
        /// Durable event sequence.
        seq: u64,
        /// Provider output/block index.
        output_index: u32,
        /// Safe normalized result metadata.
        result: heycode_core::ServerToolResult,
    },
    /// Public URL citation attached to assistant output.
    Citation {
        /// Durable event sequence.
        seq: u64,
        /// Provider output/block index.
        output_index: u32,
        /// Safe normalized citation metadata.
        citation: heycode_core::UrlCitation,
    },
    /// Provider-reported aggregate usage without individual call ids.
    Usage {
        /// Durable event sequence.
        seq: u64,
        /// Validated aggregate usage/cost evidence.
        usage: heycode_core::ServerToolUsage,
    },
}

/// Complete durable request proposal reconstructed from one header/context.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectedRequest {
    /// Turn containing the request.
    pub turn: u64,
    /// Step containing the request.
    pub step: u32,
    /// Correlation id.
    pub request_id: heycode_core::RequestId,
    /// Complete durable request header.
    pub header: RequestHeaderSnapshot,
    /// Exact capacity/catalog context.
    pub context: RequestContextSnapshot,
    /// Chronological neutral/provider inputs before this header.
    pub inputs: Vec<ProjectedInput>,
    /// UI-safe normalized server-tool/citation events produced by this request.
    /// Exact provider replay remains in [`ProjectedInput::ProviderState`].
    pub server_tool_events: Vec<ProjectedServerToolEvent>,
    /// Validated detailed cache/context-edit facts produced by this request.
    pub response_metadata: Option<heycode_core::ProviderResponseMetadata>,
}

/// Structural request reconstruction failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectionError {
    /// Configuration fingerprints or lineage do not match durable history.
    #[error("invalid request configuration for `{request_id}`")]
    InvalidConfiguration {
        /// Request with invalid diagnostic metadata.
        request_id: heycode_core::RequestId,
    },
    /// Attachment selection is unadmitted, duplicated or not paired with a user message.
    #[error("durable user attachment selection is invalid")]
    InvalidAttachmentSelection,
    /// Compaction boundary/replacement is malformed or incoherent.
    #[error("compaction at seq {seq} is invalid")]
    InvalidCompaction {
        /// Invalid durable event sequence.
        seq: u64,
    },
    /// Requested future route is structurally invalid.
    #[error("request projection route field `{field}` is invalid")]
    InvalidRoute {
        /// Stable field name only.
        field: &'static str,
    },
    /// One request id was used by two headers.
    #[error("duplicate request/header for `{request_id}`")]
    DuplicateHeader {
        /// Contested id.
        request_id: heycode_core::RequestId,
    },
    /// One request id was used by two contexts.
    #[error("duplicate request/context for `{request_id}`")]
    DuplicateContext {
        /// Contested id.
        request_id: heycode_core::RequestId,
    },
    /// Header has no matching context.
    #[error("request `{request_id}` has no request/context")]
    MissingContext {
        /// Missing id.
        request_id: heycode_core::RequestId,
    },
    /// Context appeared without/prior to its header.
    #[error("request/context for `{request_id}` has no preceding header")]
    OrphanContext {
        /// Orphan id.
        request_id: heycode_core::RequestId,
    },
    /// Provider item has no preceding producing header.
    #[error("provider item at seq {seq} references unknown request `{request_id}`")]
    OrphanProviderState {
        /// Orphan id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Provider item route/protocol differs from its producing header.
    #[error("provider item at seq {seq} does not match request `{request_id}` route")]
    ProviderStateRouteMismatch {
        /// Producing id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Provider item turn/step differs from its producing header.
    #[error("provider item at seq {seq} does not match request `{request_id}` turn/step")]
    ProviderStateStepMismatch {
        /// Producing id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Output indices for one request must be contiguous from zero.
    #[error(
        "provider item index gap for `{request_id}` at seq {seq}: expected {expected}, found {found}"
    )]
    ProviderStateIndexGap {
        /// Producing id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
        /// Required next index.
        expected: u32,
        /// Index found.
        found: u32,
    },
    /// Detailed response metadata has no preceding producing header.
    #[error("response metadata at seq {seq} references unknown request `{request_id}`")]
    OrphanResponseMetadata {
        /// Unknown request id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Detailed response metadata appeared more than once for one request.
    #[error("duplicate response metadata for `{request_id}` at seq {seq}")]
    DuplicateResponseMetadata {
        /// Producing request id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Detailed response metadata turn/step differs from its header.
    #[error("response metadata at seq {seq} does not match request `{request_id}` turn/step")]
    ResponseMetadataStepMismatch {
        /// Producing request id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Detailed response metadata failed its neutral validation boundary.
    #[error("response metadata at seq {seq} is invalid")]
    InvalidResponseMetadata {
        /// Event sequence.
        seq: u64,
    },
    /// Normalized server event has no preceding producing header.
    #[error("server-tool event at seq {seq} references unknown request `{request_id}`")]
    OrphanServerToolEvent {
        /// Unknown request id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Normalized server event payload failed its safe boundary.
    #[error("server-tool event at seq {seq} is invalid")]
    InvalidServerToolEvent {
        /// Event sequence.
        seq: u64,
    },
    /// Normalized server event turn/step differs from its producing header.
    #[error("server-tool event at seq {seq} does not match request `{request_id}` turn/step")]
    ServerToolStepMismatch {
        /// Producing request id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// Normalized server output indices cannot move backwards within a request.
    #[error("server-tool event output order regressed for `{request_id}` at seq {seq}")]
    ServerToolOutputOrder {
        /// Producing request id.
        request_id: heycode_core::RequestId,
        /// Event sequence.
        seq: u64,
    },
    /// A provider server-call id was inserted twice.
    #[error("duplicate provider server-tool call `{call_id}` at seq {seq}")]
    DuplicateServerToolCall {
        /// Contested call id.
        call_id: heycode_core::CallId,
        /// Event sequence.
        seq: u64,
    },
    /// A provider server-result has no preceding call in durable history.
    #[error("provider server-tool result at seq {seq} references unknown call `{call_id}`")]
    OrphanServerToolResult {
        /// Unknown call id.
        call_id: heycode_core::CallId,
        /// Event sequence.
        seq: u64,
    },
    /// A provider server-call settled more than once.
    #[error("duplicate provider server-tool result `{call_id}` at seq {seq}")]
    DuplicateServerToolResult {
        /// Contested call id.
        call_id: heycode_core::CallId,
        /// Event sequence.
        seq: u64,
    },
    /// One request/logical aggregate was published twice.
    #[error("duplicate provider server-tool usage for `{logical}` at seq {seq}")]
    DuplicateServerToolUsage {
        /// Safe logical tool id.
        logical: String,
        /// Event sequence.
        seq: u64,
    },
    /// A provider server-result route differs from its preceding call route.
    #[error("provider server-tool result at seq {seq} changed route for `{call_id}`")]
    ServerToolRouteMismatch {
        /// Correlated call id.
        call_id: heycode_core::CallId,
        /// Event sequence.
        seq: u64,
    },
}

#[derive(Clone)]
struct HeaderRecord {
    event_index: usize,
    turn: u64,
    step: u32,
    header: RequestHeaderSnapshot,
}

#[derive(Clone)]
struct ServerCallRoute {
    provider: String,
    model: String,
    protocol: heycode_core::ProviderProtocol,
}

/// Reconstruct every durable request in log order.
///
/// Same-route provider state replaces the generic assistant message for its
/// turn/step. Incompatible state is excluded and the neutral assistant copy is
/// retained. Context/header/state correlation is strict.
///
/// # Errors
/// Duplicate/missing/orphan correlation, route/step mismatch or provider
/// output-index gaps.
pub fn project_requests(events: &[SessionEvent]) -> Result<Vec<ProjectedRequest>, ProjectionError> {
    validate_compaction_sequence(events)?;
    validate_attachment_sequence(events)?;
    let mut headers: HashMap<heycode_core::RequestId, HeaderRecord> = HashMap::new();
    let mut header_order: Vec<(heycode_core::RequestId, HeaderRecord)> = Vec::new();
    let mut contexts: HashMap<heycode_core::RequestId, RequestContextSnapshot> = HashMap::new();
    let mut next_output_index: HashMap<heycode_core::RequestId, u32> = HashMap::new();
    let mut response_metadata: HashMap<
        heycode_core::RequestId,
        heycode_core::ProviderResponseMetadata,
    > = HashMap::new();
    let mut server_tool_events: HashMap<heycode_core::RequestId, Vec<ProjectedServerToolEvent>> =
        HashMap::new();
    let mut last_server_output_index: HashMap<heycode_core::RequestId, u32> = HashMap::new();
    let mut server_calls: HashMap<heycode_core::CallId, ServerCallRoute> = HashMap::new();
    let mut server_results: HashSet<heycode_core::CallId> = HashSet::new();
    let mut server_usage: HashSet<(heycode_core::RequestId, String)> = HashSet::new();

    for (event_index, event) in events.iter().enumerate() {
        match &event.kind {
            SessionEventKind::RequestHeader {
                turn,
                step,
                request_id,
                header,
            } => {
                header
                    .validate_configuration_after(
                        header_order.last().map(|(_, record)| &record.header),
                    )
                    .map_err(|_| ProjectionError::InvalidConfiguration {
                        request_id: request_id.clone(),
                    })?;
                let record = HeaderRecord {
                    event_index,
                    turn: *turn,
                    step: *step,
                    header: header.as_ref().clone(),
                };
                if headers.insert(request_id.clone(), record.clone()).is_some() {
                    return Err(ProjectionError::DuplicateHeader {
                        request_id: request_id.clone(),
                    });
                }
                header_order.push((request_id.clone(), record));
            }
            SessionEventKind::RequestContext {
                request_id,
                context,
            } => {
                if !headers.contains_key(request_id) {
                    return Err(ProjectionError::OrphanContext {
                        request_id: request_id.clone(),
                    });
                }
                if contexts
                    .insert(request_id.clone(), context.clone())
                    .is_some()
                {
                    return Err(ProjectionError::DuplicateContext {
                        request_id: request_id.clone(),
                    });
                }
            }
            SessionEventKind::AssistantProviderItem {
                turn,
                step,
                request_id,
                output_index,
                item,
            } => {
                let Some(header) = headers.get(request_id) else {
                    return Err(ProjectionError::OrphanProviderState {
                        request_id: request_id.clone(),
                        seq: event.seq,
                    });
                };
                if item.provider() != header.header.provider
                    || item.model() != header.header.model
                    || item.protocol() != header.header.protocol
                {
                    return Err(ProjectionError::ProviderStateRouteMismatch {
                        request_id: request_id.clone(),
                        seq: event.seq,
                    });
                }
                if *turn != header.turn || *step != header.step {
                    return Err(ProjectionError::ProviderStateStepMismatch {
                        request_id: request_id.clone(),
                        seq: event.seq,
                    });
                }
                let expected = next_output_index.entry(request_id.clone()).or_insert(0);
                if *output_index != *expected {
                    return Err(ProjectionError::ProviderStateIndexGap {
                        request_id: request_id.clone(),
                        seq: event.seq,
                        expected: *expected,
                        found: *output_index,
                    });
                }
                *expected = expected.saturating_add(1);
            }
            SessionEventKind::AssistantResponseMetadata {
                turn,
                step,
                request_id,
                metadata,
            } => {
                metadata
                    .validate()
                    .map_err(|_| ProjectionError::InvalidResponseMetadata { seq: event.seq })?;
                let Some(header) = headers.get(request_id) else {
                    return Err(ProjectionError::OrphanResponseMetadata {
                        request_id: request_id.clone(),
                        seq: event.seq,
                    });
                };
                if *turn != header.turn || *step != header.step {
                    return Err(ProjectionError::ResponseMetadataStepMismatch {
                        request_id: request_id.clone(),
                        seq: event.seq,
                    });
                }
                if response_metadata
                    .insert(request_id.clone(), metadata.as_ref().clone())
                    .is_some()
                {
                    return Err(ProjectionError::DuplicateResponseMetadata {
                        request_id: request_id.clone(),
                        seq: event.seq,
                    });
                }
            }
            SessionEventKind::ServerToolCall {
                turn,
                step,
                request_id,
                output_index,
                call,
            } => {
                call.validate()
                    .map_err(|_| ProjectionError::InvalidServerToolEvent { seq: event.seq })?;
                let header = server_event_header(&headers, request_id, *turn, *step, event.seq)?;
                validate_server_output_order(
                    &mut last_server_output_index,
                    request_id,
                    *output_index,
                    event.seq,
                )?;
                let route = ServerCallRoute {
                    provider: header.header.provider.clone(),
                    model: header.header.model.clone(),
                    protocol: header.header.protocol,
                };
                if server_calls.insert(call.id().clone(), route).is_some() {
                    return Err(ProjectionError::DuplicateServerToolCall {
                        call_id: call.id().clone(),
                        seq: event.seq,
                    });
                }
                server_tool_events
                    .entry(request_id.clone())
                    .or_default()
                    .push(ProjectedServerToolEvent::Call {
                        seq: event.seq,
                        output_index: *output_index,
                        call: call.as_ref().clone(),
                    });
            }
            SessionEventKind::ServerToolResult {
                turn,
                step,
                request_id,
                output_index,
                result,
            } => {
                result
                    .validate()
                    .map_err(|_| ProjectionError::InvalidServerToolEvent { seq: event.seq })?;
                let header = server_event_header(&headers, request_id, *turn, *step, event.seq)?;
                validate_server_output_order(
                    &mut last_server_output_index,
                    request_id,
                    *output_index,
                    event.seq,
                )?;
                let call_id = result.call_id().clone();
                let Some(call_route) = server_calls.get(&call_id) else {
                    return Err(ProjectionError::OrphanServerToolResult {
                        call_id,
                        seq: event.seq,
                    });
                };
                if call_route.provider != header.header.provider
                    || call_route.model != header.header.model
                    || call_route.protocol != header.header.protocol
                {
                    return Err(ProjectionError::ServerToolRouteMismatch {
                        call_id,
                        seq: event.seq,
                    });
                }
                if !server_results.insert(call_id.clone()) {
                    return Err(ProjectionError::DuplicateServerToolResult {
                        call_id,
                        seq: event.seq,
                    });
                }
                server_tool_events
                    .entry(request_id.clone())
                    .or_default()
                    .push(ProjectedServerToolEvent::Result {
                        seq: event.seq,
                        output_index: *output_index,
                        result: result.as_ref().clone(),
                    });
            }
            SessionEventKind::ServerToolUsage {
                turn,
                step,
                request_id,
                usage,
            } => {
                usage
                    .validate()
                    .map_err(|_| ProjectionError::InvalidServerToolEvent { seq: event.seq })?;
                let _header = server_event_header(&headers, request_id, *turn, *step, event.seq)?;
                let key = (request_id.clone(), usage.logical().to_owned());
                if !server_usage.insert(key) {
                    return Err(ProjectionError::DuplicateServerToolUsage {
                        logical: usage.logical().to_owned(),
                        seq: event.seq,
                    });
                }
                server_tool_events
                    .entry(request_id.clone())
                    .or_default()
                    .push(ProjectedServerToolEvent::Usage {
                        seq: event.seq,
                        usage: usage.as_ref().clone(),
                    });
            }
            SessionEventKind::AssistantCitation {
                turn,
                step,
                request_id,
                output_index,
                citation,
            } => {
                citation
                    .validate()
                    .map_err(|_| ProjectionError::InvalidServerToolEvent { seq: event.seq })?;
                let _header = server_event_header(&headers, request_id, *turn, *step, event.seq)?;
                validate_server_output_order(
                    &mut last_server_output_index,
                    request_id,
                    *output_index,
                    event.seq,
                )?;
                server_tool_events
                    .entry(request_id.clone())
                    .or_default()
                    .push(ProjectedServerToolEvent::Citation {
                        seq: event.seq,
                        output_index: *output_index,
                        citation: citation.as_ref().clone(),
                    });
            }
            _ => {}
        }
    }

    let mut projected = Vec::with_capacity(header_order.len());
    for (request_id, header) in header_order {
        let context =
            contexts
                .get(&request_id)
                .cloned()
                .ok_or_else(|| ProjectionError::MissingContext {
                    request_id: request_id.clone(),
                })?;
        let inputs = project_inputs(
            &events[..header.event_index],
            &header.header.provider,
            &header.header.model,
            header.header.protocol,
        );
        projected.push(ProjectedRequest {
            turn: header.turn,
            step: header.step,
            server_tool_events: server_tool_events.remove(&request_id).unwrap_or_default(),
            response_metadata: response_metadata.remove(&request_id),
            request_id,
            header: header.header.clone(),
            context,
            inputs,
        });
    }
    Ok(projected)
}

fn server_event_header<'a>(
    headers: &'a HashMap<heycode_core::RequestId, HeaderRecord>,
    request_id: &heycode_core::RequestId,
    turn: u64,
    step: u32,
    seq: u64,
) -> Result<&'a HeaderRecord, ProjectionError> {
    let Some(header) = headers.get(request_id) else {
        return Err(ProjectionError::OrphanServerToolEvent {
            request_id: request_id.clone(),
            seq,
        });
    };
    if header.turn != turn || header.step != step {
        return Err(ProjectionError::ServerToolStepMismatch {
            request_id: request_id.clone(),
            seq,
        });
    }
    Ok(header)
}

fn validate_server_output_order(
    last: &mut HashMap<heycode_core::RequestId, u32>,
    request_id: &heycode_core::RequestId,
    output_index: u32,
    seq: u64,
) -> Result<(), ProjectionError> {
    if last
        .get(request_id)
        .is_some_and(|previous| output_index < *previous)
    {
        return Err(ProjectionError::ServerToolOutputOrder {
            request_id: request_id.clone(),
            seq,
        });
    }
    last.insert(request_id.clone(), output_index);
    Ok(())
}

/// Reconstruct the complete current input prefix for one proposed exact route.
///
/// The same session-owned fold as [`project_requests`] replaces a neutral
/// assistant message only when complete provider state matches all three route
/// facts. Creation/inbox/lifecycle events remain excluded. Existing durable
/// request correlation is validated before any inputs are returned.
///
/// # Errors
/// Invalid proposed route identity, duplicate/missing/orphan correlation,
/// route/step mismatch, or provider output-index gaps.
pub fn project_inputs_for_route(
    events: &[SessionEvent],
    provider: &str,
    model: &str,
    protocol: heycode_core::ProviderProtocol,
) -> Result<Vec<ProjectedInput>, ProjectionError> {
    if provider.is_empty() || provider.trim() != provider {
        return Err(ProjectionError::InvalidRoute { field: "provider" });
    }
    if model.is_empty() || model.trim() != model {
        return Err(ProjectionError::InvalidRoute { field: "model" });
    }
    if protocol == heycode_core::ProviderProtocol::Unknown {
        return Err(ProjectionError::InvalidRoute { field: "protocol" });
    }
    let _validated_requests = project_requests(events)?;
    Ok(project_inputs(events, provider, model, protocol))
}

fn project_inputs(
    events: &[SessionEvent],
    provider: &str,
    model: &str,
    protocol: heycode_core::ProviderProtocol,
) -> Vec<ProjectedInput> {
    let winner = winning_compaction_for_route(events, provider, model, protocol);
    let shadow_upto = winner.as_ref().map(|(upto, _)| *upto);
    let replacement_steps: BTreeSet<(u64, u32)> = events
        .iter()
        .filter_map(|event| match &event.kind {
            SessionEventKind::AssistantProviderItem {
                turn, step, item, ..
            } if !shadow_upto.is_some_and(|upto| event.seq <= upto)
                && item.provider() == provider
                && item.model() == model
                && item.protocol() == protocol
                && state_replaces_assistant(item) =>
            {
                Some((*turn, *step))
            }
            _ => None,
        })
        .collect();
    let mut inputs = Vec::new();
    if let Some((_, marker)) = &winner {
        match &marker.kind {
            SessionEventKind::CompactionApplied { summary, .. } => {
                inputs.push(ProjectedInput::Message(WireMessage {
                    role: Role::User,
                    content: format!("<compacted-summary>\n{summary}\n</compacted-summary>"),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: None,
                }));
            }
            SessionEventKind::NativeCompactionApplied { items, .. } => {
                inputs.extend(items.iter().cloned().map(ProjectedInput::ProviderState));
            }
            _ => {}
        }
    }
    let mut pending_attachments = Vec::new();
    let mut pending_document_routes = Vec::new();
    for event in events {
        if shadow_upto.is_some_and(|upto| event.seq <= upto) {
            continue;
        }
        match &event.kind {
            SessionEventKind::UserAttachments {
                attachments,
                document_routes,
            } => {
                pending_attachments = attachments.clone();
                pending_document_routes = document_routes.clone();
            }
            SessionEventKind::UserMessage { text } => {
                inputs.push(ProjectedInput::Message(WireMessage {
                    role: Role::User,
                    content: text.clone(),
                    attachments: std::mem::take(&mut pending_attachments),
                    document_routes: std::mem::take(&mut pending_document_routes),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: None,
                }));
            }
            SessionEventKind::HookContribution { contribution } => {
                inputs.push(ProjectedInput::Message(WireMessage {
                    role: Role::User,
                    content: contribution.render_for_model(),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: None,
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: contribution.boundary(),
                }));
            }
            SessionEventKind::AssistantProviderItem { item, .. }
                if item.provider() == provider
                    && item.model() == model
                    && item.protocol() == protocol =>
            {
                inputs.push(ProjectedInput::ProviderState(item.as_ref().clone()));
            }
            SessionEventKind::AssistantMessage {
                turn,
                step,
                content,
                tool_calls,
                ..
            } if !replacement_steps.contains(&(*turn, *step)) => {
                inputs.push(ProjectedInput::Message(WireMessage {
                    role: Role::Assistant,
                    content: content.clone(),
                    attachments: Vec::new(),
                    document_routes: Vec::new(),
                    tool_calls: tool_calls.as_ref().map(|calls| {
                        calls
                            .iter()
                            .map(|call| WireToolCall {
                                id: call.id.clone(),
                                name: call.name.clone(),
                                arguments: call.arguments.clone(),
                            })
                            .collect()
                    }),
                    tool_call_id: None,
                    tool_result_is_error: None,
                    untrusted_content: None,
                }));
            }
            SessionEventKind::ToolResult {
                call_id,
                content,
                is_error,
                untrusted_content,
            } => inputs.push(ProjectedInput::Message(WireMessage {
                role: Role::Tool,
                content: content.clone(),
                attachments: Vec::new(),
                document_routes: Vec::new(),
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
                tool_result_is_error: Some(*is_error),
                untrusted_content: *untrusted_content,
            })),
            SessionEventKind::RichToolResult {
                call_id,
                result,
                is_error,
                untrusted_content,
            } => inputs.push(ProjectedInput::Message(WireMessage {
                role: Role::Tool,
                content: result.render_for_model(),
                attachments: Vec::new(),
                document_routes: Vec::new(),
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
                tool_result_is_error: Some(*is_error),
                untrusted_content: *untrusted_content,
            })),
            _ => {}
        }
    }
    inputs
}

fn validate_attachment_sequence(events: &[SessionEvent]) -> Result<(), ProjectionError> {
    let mut admitted = Vec::new();
    let mut pending = false;
    for event in events {
        if pending && !matches!(&event.kind, SessionEventKind::UserMessage { .. }) {
            return Err(ProjectionError::InvalidAttachmentSelection);
        }
        match &event.kind {
            SessionEventKind::AttachmentAdded { attachment } => {
                admitted.push(attachment.as_ref().clone());
            }
            SessionEventKind::UserAttachments {
                attachments,
                document_routes,
            } => {
                if event.kind.validate_for_version(event.v).is_err()
                    || attachments
                        .iter()
                        .any(|selected| !admitted.iter().any(|candidate| candidate == selected))
                    || document_routes.iter().any(|route| {
                        !admitted.iter().any(|candidate| candidate == route.source())
                            || !admitted
                                .iter()
                                .any(|candidate| candidate == route.selected())
                    })
                {
                    return Err(ProjectionError::InvalidAttachmentSelection);
                }
                pending = true;
            }
            SessionEventKind::UserMessage { .. } => pending = false,
            _ => {}
        }
    }
    if pending {
        return Err(ProjectionError::InvalidAttachmentSelection);
    }
    Ok(())
}

fn validate_compaction_sequence(events: &[SessionEvent]) -> Result<(), ProjectionError> {
    for event in events {
        let replaced_upto_seq = match &event.kind {
            SessionEventKind::CompactionApplied {
                replaced_upto_seq, ..
            }
            | SessionEventKind::NativeCompactionApplied {
                replaced_upto_seq, ..
            } => Some(*replaced_upto_seq),
            _ => None,
        };
        if replaced_upto_seq.is_some_and(|replaced| replaced >= event.seq)
            || replaced_upto_seq.is_some() && event.kind.validate_for_version(event.v).is_err()
        {
            return Err(ProjectionError::InvalidCompaction { seq: event.seq });
        }
    }
    Ok(())
}

fn state_replaces_assistant(item: &heycode_core::ProviderStateItem) -> bool {
    match item.kind() {
        // Each of these is one complete model turn on its own protocol: the
        // Chat assistant message, the Anthropic block array, and the Gemini
        // model `Content` whose parts carry the thought signature.
        heycode_core::ProviderStateKind::ChatAssistantMessage
        | heycode_core::ProviderStateKind::AnthropicMessage
        | heycode_core::ProviderStateKind::GeminiModelContent
        | heycode_core::ProviderStateKind::BedrockConverseMessage => true,
        heycode_core::ProviderStateKind::ResponseOutputItem => item
            .data()
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|kind| matches!(kind, "message" | "function_call")),
    }
}

fn winning_compaction_for_route<'a>(
    events: &'a [SessionEvent],
    provider: &str,
    model: &str,
    protocol: heycode_core::ProviderProtocol,
) -> Option<(u64, &'a SessionEvent)> {
    events.iter().fold(None, |best, event| {
        let replaced_upto_seq = match &event.kind {
            SessionEventKind::CompactionApplied {
                replaced_upto_seq, ..
            } => Some(*replaced_upto_seq),
            SessionEventKind::NativeCompactionApplied {
                replaced_upto_seq,
                items,
                ..
            } if items.first().is_some_and(|item| {
                item.provider() == provider && item.model() == model && item.protocol() == protocol
            }) =>
            {
                Some(*replaced_upto_seq)
            }
            _ => None,
        };
        let Some(replaced_upto_seq) = replaced_upto_seq else {
            return best;
        };
        match best {
            Some((best_upto, _)) if best_upto > replaced_upto_seq => best,
            _ => Some((replaced_upto_seq, event)),
        }
    })
}
