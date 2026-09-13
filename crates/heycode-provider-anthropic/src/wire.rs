//! Provider-local request extension and response-metadata transport shim.
//!
//! The reusable Messages adapter deliberately knows no Anthropic product
//! policy. This shim adds only fixed provider-owned top-level fields and beta
//! values, then forwards the request through the composed HTTP service.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use futures::StreamExt as _;
use heycode_http::{
    BufferedResponseFuture, HttpMethod, HttpRequest, HttpService, HttpSseRequest, HttpTransport,
    SseEvent, SseEventStream, TransportError,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub(crate) struct ObservedResponse(Arc<Mutex<ObservedResponseState>>);

#[derive(Clone, Default)]
pub(crate) struct ObservedResponseState {
    pub(crate) response_id: Option<String>,
    pub(crate) usage: Option<serde_json::Value>,
    pub(crate) context_management: Option<serde_json::Value>,
}

impl ObservedResponse {
    pub(crate) fn snapshot(&self) -> ObservedResponseState {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn observe(&self, event: &SseEvent) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&event.data) else {
            return;
        };
        let Some(object) = value.as_object() else {
            return;
        };
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match object.get("type").and_then(serde_json::Value::as_str) {
            Some("message_start") => {
                if let Some(message) = object.get("message").and_then(serde_json::Value::as_object)
                {
                    state.response_id = message
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .map(str::to_owned);
                    state.usage = message.get("usage").cloned();
                }
            }
            Some("message_delta") => {
                if let Some(next) = object.get("usage") {
                    merge_usage(&mut state.usage, next);
                }
                if let Some(context_management) = object.get("context_management") {
                    state.context_management = Some(context_management.clone());
                }
            }
            Some(_) | None => {}
        }
    }
}

#[derive(Clone)]
pub(crate) struct AnthropicWirePolicy {
    request_fields: Vec<serde_json::Value>,
    betas: Vec<String>,
    normalize_compaction_stop: bool,
}

impl AnthropicWirePolicy {
    pub(crate) fn new(
        request_fields: Vec<serde_json::Value>,
        betas: Vec<String>,
        normalize_compaction_stop: bool,
    ) -> Self {
        Self {
            request_fields,
            betas,
            normalize_compaction_stop,
        }
    }
}

pub(crate) struct AnthropicOperationTransport {
    inner: HttpService,
    policy: AnthropicWirePolicy,
    observed: ObservedResponse,
}

impl AnthropicOperationTransport {
    pub(crate) fn new(
        inner: HttpService,
        policy: AnthropicWirePolicy,
        observed: ObservedResponse,
    ) -> Self {
        Self {
            inner,
            policy,
            observed,
        }
    }
}

impl HttpTransport for AnthropicOperationTransport {
    fn send(
        &self,
        request: HttpRequest,
        cancellation: CancellationToken,
    ) -> BufferedResponseFuture {
        self.inner.send(request, cancellation)
    }

    fn sse(&self, request: HttpSseRequest, cancellation: CancellationToken) -> SseEventStream {
        let request = match augment_request(request, &self.policy) {
            Ok(request) => request,
            Err(error) => return Box::pin(futures::stream::once(async move { Err(error) })),
        };
        let observed = self.observed.clone();
        let normalize_compaction_stop = self.policy.normalize_compaction_stop;
        Box::pin(self.inner.sse(request, cancellation).map(move |result| {
            result.map(|mut event| {
                observed.observe(&event);
                if normalize_compaction_stop {
                    normalize_compaction_reason(&mut event);
                }
                event
            })
        }))
    }
}

fn augment_request(
    request: HttpSseRequest,
    policy: &AnthropicWirePolicy,
) -> Result<HttpSseRequest, TransportError> {
    if request.method() != HttpMethod::Post {
        return Err(invalid_request("Anthropic request extension requires POST"));
    }
    let mut body: serde_json::Value = serde_json::from_slice(
        request
            .body()
            .ok_or_else(|| invalid_request("Anthropic request body is absent"))?,
    )
    .map_err(|_| invalid_request("Anthropic request body is not JSON"))?;
    for fields in &policy.request_fields {
        merge_request_fields(&mut body, fields)?;
    }
    let encoded = serde_json::to_vec(&body)
        .map_err(|_| invalid_request("Anthropic request body cannot be encoded"))?;
    let mut rebuilt = HttpSseRequest::post(request.url(), encoded)?;
    let mut betas = BTreeSet::new();
    for header in request.headers() {
        if header.name().eq_ignore_ascii_case("anthropic-beta") {
            for beta in header.value().split(',').map(str::trim) {
                if !beta.is_empty() {
                    betas.insert(beta.to_owned());
                }
            }
        } else {
            rebuilt = rebuilt.header(header.name(), header.value())?;
        }
    }
    betas.extend(policy.betas.iter().cloned());
    if !betas.is_empty() {
        rebuilt = rebuilt.header(
            "anthropic-beta",
            &betas.into_iter().collect::<Vec<_>>().join(","),
        )?;
    }
    Ok(rebuilt)
}

fn merge_request_fields(
    body: &mut serde_json::Value,
    fields: &serde_json::Value,
) -> Result<(), TransportError> {
    let body = body
        .as_object_mut()
        .ok_or_else(|| invalid_request("Anthropic request body must be an object"))?;
    let fields = fields
        .as_object()
        .ok_or_else(|| invalid_request("Anthropic request extension must be an object"))?;
    for (key, value) in fields {
        if key == "context_management" {
            merge_context_management(body, value)?;
            continue;
        }
        match body.get(key) {
            Some(existing) if existing != value => {
                return Err(invalid_request("Anthropic request extension conflicts"));
            }
            Some(_) => {}
            None => {
                body.insert(key.clone(), value.clone());
            }
        }
    }
    Ok(())
}

fn merge_context_management(
    body: &mut serde_json::Map<String, serde_json::Value>,
    extension: &serde_json::Value,
) -> Result<(), TransportError> {
    let extension_edits = extension
        .get("edits")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| invalid_request("Anthropic context management has no edits"))?;
    let target = body
        .entry("context_management".to_owned())
        .or_insert_with(|| serde_json::json!({"edits":[]}));
    let target_edits = target
        .get_mut("edits")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| invalid_request("Anthropic context management is invalid"))?;
    target_edits.extend(extension_edits.iter().cloned());
    Ok(())
}

fn normalize_compaction_reason(event: &mut SseEvent) {
    if event.event != "message_delta" {
        return;
    }
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(&event.data) else {
        return;
    };
    if value
        .get("delta")
        .and_then(|delta| delta.get("stop_reason"))
        .and_then(serde_json::Value::as_str)
        != Some("compaction")
    {
        return;
    }
    value["delta"]["stop_reason"] = serde_json::json!("pause_turn");
    event.data = value.to_string();
}

fn merge_usage(current: &mut Option<serde_json::Value>, next: &serde_json::Value) {
    let Some(next) = next.as_object() else {
        return;
    };
    let target = current.get_or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(target) = target.as_object_mut() else {
        return;
    };
    for (key, value) in next {
        target.insert(key.clone(), value.clone());
    }
}

fn invalid_request(message: &'static str) -> TransportError {
    TransportError::InvalidRequest {
        field: "request",
        message: message.to_owned(),
    }
}
