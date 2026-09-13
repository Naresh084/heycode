//! Runnable typed start/resume/stream/cancel example over a local test transport.

use std::sync::Arc;

use async_trait::async_trait;
use heycode_sdk::{
    APP_SERVER_PROTOCOL_VERSION, AppClient, AppServerError, AppServerErrorCode, AppTransport,
    AppTurnReason,
};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct ExampleTransport {
    cancelled: Notify,
}

#[async_trait]
impl AppTransport for ExampleTransport {
    async fn exchange(
        &self,
        request: String,
        notifications: mpsc::Sender<String>,
        _cancellation: CancellationToken,
    ) -> Result<String, AppServerError> {
        let request: serde_json::Value =
            serde_json::from_str(&request).map_err(|_| AppServerError::invalid())?;
        let id = request["id"].clone();
        let result = match request["method"].as_str() {
            Some("initialize") => serde_json::json!({
                "protocolVersion":APP_SERVER_PROTOCOL_VERSION,
                "server":{"name":"heycode","version":"0.1.0"},
                "capabilities":{"turns":true,"attachments":true,"cancel":true,
                    "authorization":false,"models":false,"mcp":false,
                    "plugins":false,"settings":false}
            }),
            Some("session/open") => serde_json::json!({
                "sessionId":"example-session","runtimeId":"native","cwd":"/workspace"
            }),
            Some("turn/start") => {
                notifications
                    .send(
                        serde_json::json!({
                            "jsonrpc":"2.0","method":"session/event",
                            "params":{"sessionId":"example-session","sequence":5,
                                "event":{"type":"turn_started","turn_id":"1"}}
                        })
                        .to_string(),
                    )
                    .await
                    .map_err(|_| AppServerError::unavailable())?;
                self.cancelled.notified().await;
                notifications
                    .send(
                        serde_json::json!({
                            "jsonrpc":"2.0","method":"session/event",
                            "params":{"sessionId":"example-session","sequence":6,
                                "event":{"type":"turn_finished","turn_id":"1",
                                    "reason":"cancelled"}}
                        })
                        .to_string(),
                    )
                    .await
                    .map_err(|_| AppServerError::unavailable())?;
                serde_json::json!({"turnId":"1","reason":"cancelled"})
            }
            Some("turn/cancel") => {
                self.cancelled.notify_one();
                serde_json::Value::Null
            }
            _ => {
                return Err(AppServerError::classified(
                    AppServerErrorCode::MethodNotFound,
                ));
            }
        };
        Ok(serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
    }
}

#[tokio::main]
async fn main() -> Result<(), AppServerError> {
    let client = AppClient::new(Arc::new(ExampleTransport::default()));
    let session = client.start().await?;
    client.resume(&session.session_id).await?;

    let (events_tx, mut events_rx) = mpsc::channel(8);
    let turn = {
        let client = client.clone();
        tokio::spawn(async move {
            client
                .turn("wait", Vec::new(), events_tx, CancellationToken::new())
                .await
        })
    };
    events_rx
        .recv()
        .await
        .ok_or_else(AppServerError::unavailable)?;
    client.cancel().await?;
    let result = turn
        .await
        .map_err(|_| AppServerError::classified(AppServerErrorCode::Internal))??;
    if result.reason != AppTurnReason::Cancelled {
        return Err(AppServerError::invalid());
    }
    events_rx
        .recv()
        .await
        .ok_or_else(AppServerError::unavailable)?;
    Ok(())
}
