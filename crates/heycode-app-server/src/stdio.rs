//! Bounded local child-process transport for app-server v1.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, AsyncRead, AsyncWrite, AsyncWriteExt as _, BufReader};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::{AppServer, AppServerError, AppServerErrorCode, AppServerNotification};

const MAX_APP_FRAME_BYTES: usize = 4 * 1024 * 1024;
const MAX_TRANSPORT_FRAME_BYTES: usize = MAX_APP_FRAME_BYTES + 1024;
const MAX_PENDING_OPERATIONS: usize = 64;
const MAX_JAVASCRIPT_INTEGER: u64 = 9_007_199_254_740_991;
const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum InboundFrame {
    Request { operation: u64, request: Value },
    Cancel { operation: u64 },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum OutboundFrame {
    Notification {
        operation: u64,
        notification: Box<AppServerNotification>,
    },
    Response {
        operation: u64,
        response: Value,
    },
}

/// Serve one child-owned app-server v1 connection over bounded NDJSON.
///
/// The caller owns the composed [`AppServer`] and its surrounding Context.
/// This transport owns only the child-process connection and its per-request
/// cancellation children. EOF/cancellation settles every admitted operation
/// and the single writer before returning. Raw request/response frames are
/// never logged because an authorization-answer request can contain a secret.
///
/// # Errors
/// Returns a body-free classified error for malformed/bounded framing, I/O,
/// cancellation, duplicate correlation, or operation-lifecycle failure.
pub async fn serve_stdio_transport<R, W>(
    server: Arc<AppServer>,
    reader: R,
    writer: W,
    cancellation: CancellationToken,
) -> Result<(), AppServerError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let (outbound_tx, outbound_rx) = mpsc::channel(128);
    let mut writers = JoinSet::new();
    writers.spawn(write_frames(writer, outbound_rx));
    let mut writer_result = None;
    let mut reader = BufReader::new(reader);
    let mut line = Vec::new();
    let mut operations = JoinSet::new();
    let mut pending = HashMap::<u64, CancellationToken>::new();
    let mut last_operation = 0_u64;
    let mut terminal_error = None;

    loop {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => break,
            result = writers.join_next(), if writer_result.is_none() => {
                let result = result
                    .ok_or_else(|| AppServerError::classified(AppServerErrorCode::Internal))
                    .and_then(join_result);
                writer_result = Some(result);
                terminal_error = writer_result.as_ref().and_then(|result| result.clone().err());
                break;
            }
            result = operations.join_next(), if !operations.is_empty() => {
                let Some(result) = result else { continue; };
                match result {
                    Ok((operation, result)) => {
                        pending.remove(&operation);
                        if let Err(error) = result {
                            terminal_error = Some(error);
                            break;
                        }
                    }
                    Err(_) => {
                        terminal_error = Some(AppServerError::classified(AppServerErrorCode::Internal));
                        break;
                    }
                }
            }
            frame = read_frame(&mut reader, &mut line) => {
                let frame = match frame {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(error) => {
                        terminal_error = Some(error);
                        break;
                    }
                };
                match frame {
                    InboundFrame::Request { operation, request } => {
                        if let Err(error) = validate_operation(operation) {
                            terminal_error = Some(error);
                            break;
                        }
                        if pending.len() >= MAX_PENDING_OPERATIONS
                            || pending.contains_key(&operation)
                            || operation <= last_operation
                            || request_id(&request) != Some(operation)
                        {
                            terminal_error = Some(AppServerError::invalid());
                            break;
                        }
                        last_operation = operation;
                        let request = match serde_json::to_string(&request) {
                            Ok(request) => request,
                            Err(_) => {
                                terminal_error = Some(AppServerError::invalid());
                                break;
                            }
                        };
                        if request.is_empty() || request.len() > MAX_APP_FRAME_BYTES {
                            terminal_error = Some(AppServerError::invalid());
                            break;
                        }
                        let operation_token = cancellation.child_token();
                        pending.insert(operation, operation_token.clone());
                        operations.spawn(run_operation(
                            server.clone(),
                            operation,
                            request,
                            outbound_tx.clone(),
                            operation_token,
                        ));
                    }
                    InboundFrame::Cancel { operation } => {
                        if let Err(error) = validate_operation(operation) {
                            terminal_error = Some(error);
                            break;
                        }
                        if let Some(token) = pending.get(&operation) {
                            token.cancel();
                        }
                    }
                }
            }
        }
    }

    for token in pending.values() {
        token.cancel();
    }
    let settle_operations = async {
        let mut first_error = None;
        while let Some(result) = operations.join_next().await {
            match result {
                Ok((_operation, Ok(()))) => {}
                Ok((_operation, Err(error))) if first_error.is_none() => first_error = Some(error),
                Err(_) if first_error.is_none() => {
                    first_error = Some(AppServerError::classified(AppServerErrorCode::Internal));
                }
                Ok(_) | Err(_) => {}
            }
        }
        first_error
    };
    match tokio::time::timeout(SHUTDOWN_DEADLINE, settle_operations).await {
        Ok(Some(error)) if terminal_error.is_none() => terminal_error = Some(error),
        Ok(_) => {}
        Err(_) => {
            operations.abort_all();
            while operations.join_next().await.is_some() {}
            if terminal_error.is_none() {
                terminal_error = Some(AppServerError::classified(AppServerErrorCode::Internal));
            }
        }
    }
    drop(outbound_tx);
    let writer_result = match writer_result {
        Some(result) => result,
        None => writers
            .join_next()
            .await
            .ok_or_else(|| AppServerError::classified(AppServerErrorCode::Internal))
            .and_then(join_result),
    };
    if terminal_error.is_none()
        && let Err(error) = writer_result
    {
        terminal_error = Some(error);
    }
    terminal_error.map_or(Ok(()), Err)
}

async fn run_operation(
    server: Arc<AppServer>,
    operation: u64,
    request: String,
    outbound: mpsc::Sender<OutboundFrame>,
    cancellation: CancellationToken,
) -> (u64, Result<(), AppServerError>) {
    let (events_tx, mut events_rx) = mpsc::channel(32);
    let response = server.request(&request, events_tx, cancellation);
    tokio::pin!(response);
    let mut events_open = true;
    let response = loop {
        tokio::select! {
            maybe_event = events_rx.recv(), if events_open => {
                match maybe_event {
                    Some(notification) => {
                        if outbound.send(OutboundFrame::Notification {
                            operation,
                            notification: Box::new(notification),
                        }).await.is_err() {
                            return (operation, Err(AppServerError::unavailable()));
                        }
                    }
                    None => events_open = false,
                }
            }
            response = &mut response => break response,
        }
    };
    while let Ok(notification) = events_rx.try_recv() {
        if outbound
            .send(OutboundFrame::Notification {
                operation,
                notification: Box::new(notification),
            })
            .await
            .is_err()
        {
            return (operation, Err(AppServerError::unavailable()));
        }
    }
    let response = match serde_json::from_str::<Value>(&response) {
        Ok(response) => response,
        Err(_) => return (operation, Err(AppServerError::invalid())),
    };
    let result = outbound
        .send(OutboundFrame::Response {
            operation,
            response,
        })
        .await
        .map_err(|_| AppServerError::unavailable());
    (operation, result)
}

async fn write_frames<W>(
    mut writer: W,
    mut outbound: mpsc::Receiver<OutboundFrame>,
) -> Result<(), AppServerError>
where
    W: AsyncWrite + Unpin,
{
    while let Some(frame) = outbound.recv().await {
        let raw = serde_json::to_vec(&frame).map_err(|_| AppServerError::invalid())?;
        if raw.is_empty() || raw.len() > MAX_TRANSPORT_FRAME_BYTES {
            return Err(AppServerError::invalid());
        }
        writer
            .write_all(&raw)
            .await
            .map_err(|_| AppServerError::unavailable())?;
        writer
            .write_all(b"\n")
            .await
            .map_err(|_| AppServerError::unavailable())?;
        writer
            .flush()
            .await
            .map_err(|_| AppServerError::unavailable())?;
    }
    writer
        .shutdown()
        .await
        .map_err(|_| AppServerError::unavailable())
}

async fn read_frame<R>(
    reader: &mut BufReader<R>,
    line: &mut Vec<u8>,
) -> Result<Option<InboundFrame>, AppServerError>
where
    R: AsyncRead + Unpin,
{
    line.clear();
    loop {
        let available = reader
            .fill_buf()
            .await
            .map_err(|_| AppServerError::unavailable())?;
        if available.is_empty() {
            return if line.is_empty() {
                Ok(None)
            } else {
                Err(AppServerError::invalid())
            };
        }
        let newline = available.iter().position(|byte| *byte == b'\n');
        let take = newline.map_or(available.len(), |index| index.saturating_add(1));
        let content = newline.map_or(take, |_| take.saturating_sub(1));
        if line.len().saturating_add(content) > MAX_TRANSPORT_FRAME_BYTES {
            return Err(AppServerError::invalid());
        }
        line.extend_from_slice(&available[..content]);
        reader.consume(take);
        if newline.is_some() {
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                return Err(AppServerError::invalid());
            }
            let raw = std::str::from_utf8(line).map_err(|_| AppServerError::invalid())?;
            let frame = serde_json::from_str(raw).map_err(|_| AppServerError::invalid())?;
            return Ok(Some(frame));
        }
    }
}

fn request_id(request: &Value) -> Option<u64> {
    request.as_object()?.get("id")?.as_u64()
}

fn validate_operation(operation: u64) -> Result<(), AppServerError> {
    if operation == 0 || operation > MAX_JAVASCRIPT_INTEGER {
        return Err(AppServerError::invalid());
    }
    Ok(())
}

fn join_result(
    result: Result<Result<(), AppServerError>, tokio::task::JoinError>,
) -> Result<(), AppServerError> {
    match result {
        Ok(result) => result,
        Err(_) => Err(AppServerError::classified(AppServerErrorCode::Internal)),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn shared_stdio_v1_fixture_locks_both_envelope_directions() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../sdks/fixtures/app-server-stdio-v1.json"
        ))
        .unwrap();
        assert_eq!(fixture["transportVersion"], 1);
        for key in ["request", "cancel"] {
            serde_json::from_value::<InboundFrame>(fixture[key].clone()).unwrap();
        }
        for key in ["notification", "response"] {
            let frame = serde_json::from_value::<OutboundFrame>(fixture[key].clone()).unwrap();
            assert_eq!(serde_json::to_value(frame).unwrap(), fixture[key]);
        }
    }
}
