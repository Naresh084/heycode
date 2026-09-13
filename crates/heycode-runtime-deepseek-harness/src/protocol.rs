//! Exact DeepSeek Harness SDK v0.0.1 JSON-RPC framing and correlation.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Map, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use heycode_runtime::{AcpFrameDecoder, RuntimeError};

use crate::process::SdkProcess;
use crate::{DSH_SDK_SERVER_NAME, PINNED_DSH_SDK_VERSION};

/// Exact repository-known session event union at SDK protocol v0.0.1.
///
/// Source: DeepSeek Harness commit
/// `528c682e061696f5a160f363f236ecbf53cbd006`, generated
/// `packages/core/session/src/known-event-types.ts`.
pub const PINNED_DSH_SESSION_EVENT_TYPES: &[&str] = &[
    "agent-preset/selected",
    "agent/inbox/spliced",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "assistant/chunk",
    "assistant/message",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/call",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "tool/result",
    "turn/end",
    "turn/start",
    "user/message",
    "web/deepseek-search-llm-request",
];

pub(crate) const HANDLED_DSH_SESSION_EVENT_TYPES: &[&str] = &[
    "agent/inbox/spliced",
    "assistant/chunk",
    "assistant/message",
    "tool/call",
    "tool/result",
    "turn/end",
    "turn/start",
];

pub(crate) const IGNORED_DSH_SESSION_EVENT_TYPES: &[&str] = &[
    "agent-preset/selected",
    "approval/asked",
    "approval/decided",
    "approval/policy",
    "command/done",
    "command/run",
    "compaction/end",
    "compaction/prune",
    "compaction/start",
    "compaction/summary",
    "feedback/record",
    "goal/change",
    "hook/invoked",
    "hook/result",
    "llm/retry",
    "llm/retry-started",
    "permission/preset",
    "plan/mode",
    "request/context",
    "request/header",
    "sandbox/mode",
    "schedule/change",
    "session/end-seed",
    "session/title",
    "session/title-llm-request",
    "step/end",
    "step/start",
    "subagent/descriptor",
    "team/member",
    "team/message/delivered",
    "team/message/queued",
    "team/task",
    "todo/write",
    "tool-workflow/agent-end",
    "tool-workflow/agent-start",
    "tool-workflow/run-end",
    "tool-workflow/run-start",
    "tool/code-dispatch",
    "tool/code-dispatch-start",
    "user/message",
    "web/deepseek-search-llm-request",
];

const MAX_SDK_FRAME_BYTES: usize = 4 * 1024 * 1024;

struct ReaderState {
    decoder: AcpFrameDecoder,
    ready: VecDeque<Value>,
    eof: bool,
}

pub(crate) struct SdkPeer {
    process: Arc<SdkProcess>,
    reader: Mutex<ReaderState>,
    next_id: AtomicU64,
    closed: AtomicBool,
}

impl SdkPeer {
    pub(crate) fn new(process: Arc<SdkProcess>) -> Result<Self, RuntimeError> {
        Ok(Self {
            process,
            reader: Mutex::new(ReaderState {
                decoder: AcpFrameDecoder::new(MAX_SDK_FRAME_BYTES)
                    .map_err(|_| RuntimeError::internal("invalid SDK frame limit"))?,
                ready: VecDeque::new(),
                eof: false,
            }),
            next_id: AtomicU64::new(1),
            closed: AtomicBool::new(false),
        })
    }

    pub(crate) async fn initialize(
        &self,
        cwd: &Path,
        provider: &str,
        model: &str,
        max_tokens: Option<u64>,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let cwd = cwd.to_str().ok_or_else(RuntimeError::invalid_request)?;
        let mut params = Map::new();
        params.insert("cwd".to_owned(), Value::String(cwd.to_owned()));
        params.insert("provider".to_owned(), Value::String(provider.to_owned()));
        params.insert("model".to_owned(), Value::String(model.to_owned()));
        if let Some(max_tokens) = max_tokens {
            params.insert("maxTokens".to_owned(), Value::from(max_tokens));
        }
        let result = self
            .request(
                "initialize",
                Value::Object(params),
                cancellation,
                |_message| Err(RuntimeError::protocol()),
            )
            .await?;
        parse_initialize_result(&result)
    }

    pub(crate) fn next_request_id(&self) -> Result<u64, RuntimeError> {
        self.next_id
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |value| {
                value.checked_add(1)
            })
            .map_err(|_| RuntimeError::internal("SDK request ids exhausted"))
    }

    pub(crate) async fn write_request(
        &self,
        id: u64,
        method: &'static str,
        params: Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let value = if params.is_null() {
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method})
        } else {
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        };
        self.write_value(&value, cancellation).await
    }

    pub(crate) async fn read_value(
        &self,
        cancellation: CancellationToken,
    ) -> Result<Option<Value>, RuntimeError> {
        let mut reader = self.reader.lock().await;
        loop {
            if let Some(value) = reader.ready.pop_front() {
                validate_envelope(&value)?;
                return Ok(Some(value));
            }
            if reader.eof {
                return Ok(None);
            }
            match self.process.read(cancellation.clone()).await? {
                Some(bytes) if !bytes.is_empty() => {
                    let frames = reader
                        .decoder
                        .push(&bytes)
                        .map_err(|_| RuntimeError::protocol())?;
                    reader.ready.extend(frames);
                }
                Some(_) => return Err(RuntimeError::protocol()),
                None => {
                    reader
                        .decoder
                        .finish()
                        .map_err(|_| RuntimeError::protocol())?;
                    reader.eof = true;
                }
            }
        }
    }

    pub(crate) fn response_result(
        &self,
        message: &Value,
        expected_id: u64,
    ) -> Result<Value, RuntimeError> {
        if message.get("method").is_some()
            || message.get("id").and_then(Value::as_u64) != Some(expected_id)
        {
            return Err(RuntimeError::protocol());
        }
        parse_response_result(message)
    }

    pub(crate) async fn shutdown(
        &self,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        let result = self
            .request("shutdown", Value::Null, cancellation, |_message| {
                Err(RuntimeError::protocol())
            })
            .await?;
        if result.as_object().is_none_or(|object| !object.is_empty()) {
            return Err(RuntimeError::protocol());
        }
        Ok(())
    }

    pub(crate) async fn close(&self, cancellation: CancellationToken) -> Result<(), RuntimeError> {
        if self.closed.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        self.process.close(cancellation).await
    }

    async fn request<F>(
        &self,
        method: &'static str,
        params: Value,
        cancellation: CancellationToken,
        mut inbound: F,
    ) -> Result<Value, RuntimeError>
    where
        F: FnMut(&Value) -> Result<(), RuntimeError>,
    {
        let id = self.next_request_id()?;
        self.write_request(id, method, params, cancellation.clone())
            .await?;
        loop {
            let message = self
                .read_value(cancellation.clone())
                .await?
                .ok_or_else(RuntimeError::protocol)?;
            if message.get("method").is_some() {
                inbound(&message)?;
                continue;
            }
            return self.response_result(&message, id);
        }
    }

    async fn write_value(
        &self,
        value: &Value,
        cancellation: CancellationToken,
    ) -> Result<(), RuntimeError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(RuntimeError::closed());
        }
        let mut frame = serde_json::to_vec(value).map_err(|_| RuntimeError::invalid_request())?;
        if frame.len() > MAX_SDK_FRAME_BYTES {
            return Err(RuntimeError::invalid_request());
        }
        frame.push(b'\n');
        self.process.write(&frame, cancellation).await
    }
}

fn parse_initialize_result(value: &Value) -> Result<(), RuntimeError> {
    let object = exact_object(value, &["serverInfo"])?;
    let info = object
        .get("serverInfo")
        .ok_or_else(RuntimeError::protocol)?;
    let info = exact_object(info, &["name", "version"])?;
    if info.get("name").and_then(Value::as_str) != Some(DSH_SDK_SERVER_NAME)
        || info.get("version").and_then(Value::as_str) != Some(PINNED_DSH_SDK_VERSION)
    {
        return Err(RuntimeError::protocol());
    }
    Ok(())
}

fn validate_envelope(value: &Value) -> Result<(), RuntimeError> {
    let object = value.as_object().ok_or_else(RuntimeError::protocol)?;
    if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
        return Err(RuntimeError::protocol());
    }
    let method = object.get("method").and_then(Value::as_str);
    let id = object.get("id");
    let result = object.contains_key("result");
    let error = object.contains_key("error");
    if let Some(method) = method {
        validate_text(method, 128)?;
        if result
            || error
            || object
                .get("params")
                .is_some_and(|params| !params.is_object())
        {
            return Err(RuntimeError::protocol());
        }
        if let Some(id) = id {
            validate_id(id)?;
        }
    } else {
        let id = id.ok_or_else(RuntimeError::protocol)?;
        validate_id(id)?;
        if result == error {
            return Err(RuntimeError::protocol());
        }
    }
    Ok(())
}

fn validate_id(value: &Value) -> Result<(), RuntimeError> {
    match value {
        Value::Number(number) if number.as_u64().is_some() => Ok(()),
        Value::String(value) => validate_text(value, 256),
        _ => Err(RuntimeError::protocol()),
    }
}

fn parse_response_result(message: &Value) -> Result<Value, RuntimeError> {
    if let Some(result) = message.get("result") {
        return Ok(result.clone());
    }
    let error = message
        .get("error")
        .and_then(Value::as_object)
        .ok_or_else(RuntimeError::protocol)?;
    let code = error
        .get("code")
        .and_then(Value::as_i64)
        .ok_or_else(RuntimeError::protocol)?;
    validate_text(
        error
            .get("message")
            .and_then(Value::as_str)
            .ok_or_else(RuntimeError::protocol)?,
        512,
    )?;
    Err(match code {
        -32601 => RuntimeError::unsupported(),
        -32602 => RuntimeError::invalid_request(),
        _ => RuntimeError::unavailable(),
    })
}

fn exact_object<'a>(
    value: &'a Value,
    keys: &[&str],
) -> Result<&'a Map<String, Value>, RuntimeError> {
    let object = value.as_object().ok_or_else(RuntimeError::protocol)?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err(RuntimeError::protocol());
    }
    Ok(object)
}

pub(crate) fn validate_text(value: &str, maximum: usize) -> Result<(), RuntimeError> {
    if value.is_empty()
        || value.trim() != value
        || value.len() > maximum
        || value.chars().any(char::is_control)
    {
        Err(RuntimeError::protocol())
    } else {
        Ok(())
    }
}
