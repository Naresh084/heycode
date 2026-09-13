//! MCP11: elicitation, progress, logging, routing and exact cancellation.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_mcp::{
    McpClientEvent, McpClientEventRouter, McpClientEventSink, McpClientRoute,
    McpElicitationCapabilities, McpElicitationFailure, McpElicitationHandler,
    McpElicitationResponse, McpServerId,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct RecordingSink(Mutex<Vec<McpClientEvent>>);

impl McpClientEventSink for RecordingSink {
    fn publish(&self, event: McpClientEvent) {
        self.0.lock().unwrap().push(event);
    }
}

struct AcceptingHandler;

#[async_trait]
impl McpElicitationHandler for AcceptingHandler {
    async fn elicit(
        &self,
        request: heycode_mcp::McpElicitationRequest,
        _cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure> {
        assert_eq!(request.route().expose(), "session-one");
        assert_eq!(request.server().as_str(), "fixture");
        Ok(McpElicitationResponse::accept(serde_json::json!({
            "answer": "reviewed"
        })))
    }
}

struct WaitForCancellation;

#[async_trait]
impl McpElicitationHandler for WaitForCancellation {
    async fn elicit(
        &self,
        _request: heycode_mcp::McpElicitationRequest,
        cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure> {
        cancellation.cancelled().await;
        Err(McpElicitationFailure::Cancelled)
    }
}

struct PanickingHandler;

#[async_trait]
impl McpElicitationHandler for PanickingHandler {
    async fn elicit(
        &self,
        _request: heycode_mcp::McpElicitationRequest,
        _cancellation: CancellationToken,
    ) -> Result<McpElicitationResponse, McpElicitationFailure> {
        panic!("handler panic must be contained")
    }
}

fn router(handler: Arc<dyn McpElicitationHandler>) -> (McpClientEventRouter, Arc<RecordingSink>) {
    let sink = Arc::new(RecordingSink::default());
    let router = McpClientEventRouter::new(
        McpServerId::new("fixture").unwrap(),
        McpClientRoute::new("session-one").unwrap(),
        McpElicitationCapabilities::form_and_url(),
        handler,
        sink.clone(),
    );
    (router, sink)
}

fn form_request(id: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc":"2.0",
        "id":id,
        "method":"elicitation/create",
        "params":{
            "message":"Choose a reviewed value",
            "requestedSchema":{
                "type":"object",
                "properties":{
                    "answer":{"type":"string","minLength":1,"maxLength":32}
                },
                "required":["answer"]
            }
        }
    })
}

#[tokio::test]
async fn form_elicitation_routes_to_the_exact_session_and_validates_the_reply() {
    let (router, _sink) = router(Arc::new(AcceptingHandler));
    let pending = router
        .admit_request(&form_request(serde_json::json!(7)))
        .unwrap()
        .expect("elicitation is a supported server request");
    assert_eq!(router.pending_elicitations(), 1);

    let reply = pending.resolve().await.expect("not cancelled");
    assert_eq!(reply.as_json()["id"], 7);
    assert_eq!(reply.as_json()["result"]["action"], "accept");
    assert_eq!(reply.as_json()["result"]["content"]["answer"], "reviewed");
    assert_eq!(router.pending_elicitations(), 0);
}

#[tokio::test]
async fn exact_server_cancellation_retires_only_its_pending_elicitation() {
    let (router, _sink) = router(Arc::new(WaitForCancellation));
    let first = router
        .admit_request(&form_request(serde_json::json!("first")))
        .unwrap()
        .unwrap();
    let second = router
        .admit_request(&form_request(serde_json::json!("second")))
        .unwrap()
        .unwrap();

    assert!(!router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/cancelled",
        "params":{"requestId":"foreign","reason":"must stay data"}
    })));
    assert_eq!(router.pending_elicitations(), 2);

    assert!(router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/cancelled",
        "params":{"requestId":"first","reason":"user cancelled"}
    })));
    assert_eq!(router.pending_elicitations(), 1);
    assert!(
        first.resolve().await.is_none(),
        "cancelled requests send no reply"
    );

    router.shutdown();
    assert_eq!(router.pending_elicitations(), 0);
    assert!(second.resolve().await.is_none());
}

#[test]
fn progress_is_monotonic_active_only_and_routed_without_exposing_the_token() {
    let (router, sink) = router(Arc::new(AcceptingHandler));
    let progress = router.begin_progress();
    let token = progress.token().to_json();

    assert!(router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/progress",
        "params":{"progressToken":token,"progress":1.0,"total":2.0,"message":"half"}
    })));
    assert!(router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/progress",
        "params":{"progressToken":progress.token().to_json(),"progress":2.0,"total":2.0}
    })));
    assert!(!router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/progress",
        "params":{"progressToken":progress.token().to_json(),"progress":1.5}
    })));
    assert_eq!(sink.0.lock().unwrap().len(), 2);
    assert_eq!(sink.0.lock().unwrap()[0].route().expose(), "session-one");

    let debug = format!("{progress:?}");
    assert!(!debug.contains(progress.token().expose_for_wire()));
    drop(progress);
    assert!(!router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/progress",
        "params":{"progressToken":token,"progress":3.0}
    })));
}

#[test]
fn bounded_logging_and_url_completion_stay_on_the_human_event_plane() {
    let (router, sink) = router(Arc::new(AcceptingHandler));
    router.enable_logging();
    assert!(router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/message",
        "params":{"level":"warning","logger":"fixture","data":{"detail":"visible only in UI"}}
    })));
    assert!(!router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/message",
        "params":{"level":"invented","data":"ignored"}
    })));
    assert!(matches!(
        sink.0.lock().unwrap().as_slice(),
        [McpClientEvent::Log(_)]
    ));

    let rendered = format!("{:?}", sink.0.lock().unwrap()[0]);
    assert!(!rendered.contains("visible only in UI"));
    assert!(!rendered.contains("session-one"));
}

#[test]
fn shutdown_retires_logging_before_any_late_human_event_can_publish() {
    let (router, sink) = router(Arc::new(AcceptingHandler));
    router.enable_logging();
    router.shutdown();

    assert!(!router.observe_notification(&serde_json::json!({
        "jsonrpc":"2.0",
        "method":"notifications/message",
        "params":{"level":"warning","logger":"late","data":"must not publish"}
    })));
    assert!(sink.0.lock().unwrap().is_empty());
}

#[test]
fn stdio_driver_owns_elicitation_futures_instead_of_detaching_tasks() {
    let source = include_str!("../../src/lib.rs");
    let driver_start = source.find("async fn driver_loop(").unwrap();
    let driver_end = source[driver_start..]
        .find("static REQUEST_SEQ")
        .map(|offset| driver_start + offset)
        .unwrap();
    let driver = &source[driver_start..driver_end];

    assert!(
        !driver.contains("tokio::spawn"),
        "the stdio driver must retain every elicitation future until settlement"
    );
    assert!(
        driver.contains("FuturesUnordered"),
        "owned elicitation work must remain concurrent with stdio reads"
    );
}

#[tokio::test]
async fn malformed_or_duplicate_server_requests_fail_without_replacing_pending_state() {
    let (router, _sink) = router(Arc::new(WaitForCancellation));
    let pending = router
        .admit_request(&form_request(serde_json::json!(1)))
        .unwrap()
        .unwrap();
    let duplicate = router
        .admit_request(&form_request(serde_json::json!(1)))
        .expect_err("duplicate live ids fail loud");
    assert_eq!(duplicate.as_json()["error"]["code"], -32600);
    assert_eq!(router.pending_elicitations(), 1);

    let invalid = router
        .admit_request(&serde_json::json!({
            "jsonrpc":"2.0","id":2,"method":"elicitation/create",
            "params":{"mode":"form","message":"x","requestedSchema":{"type":"object","properties":{"nested":{"type":"object"}}}}
        }))
        .expect_err("nested schemas are outside the elicitation subset");
    assert_eq!(invalid.as_json()["error"]["code"], -32602);
    assert_eq!(router.pending_elicitations(), 1);
    router.shutdown();
    assert!(pending.resolve().await.is_none());
}

#[tokio::test]
async fn pending_elicitation_admission_is_bounded_without_replacing_live_rows() {
    let (router, _sink) = router(Arc::new(WaitForCancellation));
    let mut pending = Vec::new();
    for id in 0..16 {
        pending.push(
            router
                .admit_request(&form_request(serde_json::json!(id)))
                .unwrap()
                .unwrap(),
        );
    }
    let refused = router
        .admit_request(&form_request(serde_json::json!(16)))
        .expect_err("the pending interaction budget must fail closed");
    assert_eq!(refused.as_json()["error"]["code"], -32000);
    assert_eq!(router.pending_elicitations(), 16);
    router.shutdown();
    for request in pending {
        assert!(request.resolve().await.is_none());
    }
}

#[tokio::test]
async fn a_panicking_elicitation_handler_returns_a_static_error_and_retires_the_row() {
    let (router, _sink) = router(Arc::new(PanickingHandler));
    let pending = router
        .admit_request(&form_request(serde_json::json!(9)))
        .unwrap()
        .unwrap();
    let reply = pending.resolve().await.unwrap();
    assert_eq!(reply.as_json()["error"]["code"], -32603);
    assert_eq!(router.pending_elicitations(), 0);
    assert!(!format!("{reply:?}").contains("handler panic"));
}

#[cfg(unix)]
#[tokio::test]
async fn stdio_routes_server_elicitation_and_logging_through_the_transport_owner() {
    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("accepted");
    let script = r#"
import json,sys
init=json.loads(sys.stdin.readline())
assert init['params']['protocolVersion']=='2025-11-25'
assert init['params']['capabilities']['elicitation']=={'form':{},'url':{}}
print(json.dumps({'jsonrpc':'2.0','id':init['id'],'result':{'protocolVersion':'2025-11-25','capabilities':{'logging':{}},'serverInfo':{'name':'interactive','version':'1'}}}),flush=True)
initialized=json.loads(sys.stdin.readline())
assert initialized['method']=='notifications/initialized'
print(json.dumps({'jsonrpc':'2.0','id':'ask-transport','method':'elicitation/create','params':{'mode':'form','message':'review','requestedSchema':{'type':'object','properties':{'answer':{'type':'string'}},'required':['answer']}}}),flush=True)
reply=json.loads(sys.stdin.readline())
assert reply['id']=='ask-transport'
assert reply['result']=={'action':'accept','content':{'answer':'reviewed'}}
open(sys.argv[1],'w').write('ok')
print(json.dumps({'jsonrpc':'2.0','method':'notifications/message','params':{'level':'notice','data':'transport-log'}}),flush=True)
sys.stdin.readline()
"#;
    let (client_events, sink) = router(Arc::new(AcceptingHandler));
    let connection = heycode_mcp::McpConnection::spawn_with_client_events(
        "interactive",
        &heycode_mcp::McpServerConfig {
            command: "python3".to_owned(),
            args: vec![
                "-u".to_owned(),
                "-c".to_owned(),
                script.to_owned(),
                marker.display().to_string(),
            ],
            env: std::collections::HashMap::new(),
            required: false,
        },
        client_events,
    )
    .await
    .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if marker.exists() && !sink.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the server request and log must route");
    assert!(matches!(
        sink.0.lock().unwrap().as_slice(),
        [McpClientEvent::Log(_)]
    ));
    connection.kill();
}

#[cfg(unix)]
#[tokio::test]
async fn cancelling_a_stdio_request_retires_its_exact_transport_row_and_notifies_the_server() {
    use heycode_mcp::McpRequestChannel as _;

    let temp = tempfile::tempdir().unwrap();
    let marker = temp.path().join("cancelled-id");
    let script = r#"
import json,sys
init=json.loads(sys.stdin.readline())
print(json.dumps({'jsonrpc':'2.0','id':init['id'],'result':{'protocolVersion':'2025-11-25','capabilities':{},'serverInfo':{'name':'cancel','version':'1'}}}),flush=True)
sys.stdin.readline()
request=json.loads(sys.stdin.readline())
cancel=json.loads(sys.stdin.readline())
assert cancel['method']=='notifications/cancelled'
assert cancel['params']['requestId']==request['id']
open(sys.argv[1],'w').write(str(request['id']))
sys.stdin.readline()
"#;
    let connection = Arc::new(
        heycode_mcp::McpConnection::spawn(
            "cancel",
            &heycode_mcp::McpServerConfig {
                command: "python3".to_owned(),
                args: vec![
                    "-u".to_owned(),
                    "-c".to_owned(),
                    script.to_owned(),
                    marker.display().to_string(),
                ],
                env: std::collections::HashMap::new(),
                required: false,
            },
        )
        .await
        .unwrap(),
    );
    let cancellation = CancellationToken::new();
    let call = {
        let connection = connection.clone();
        let cancellation = cancellation.clone();
        tokio::spawn(async move {
            connection
                .call("tools/call", serde_json::json!({}), &cancellation)
                .await
        })
    };
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    cancellation.cancel();
    assert_eq!(
        call.await.unwrap(),
        Err(heycode_mcp::McpChannelError::Cancelled)
    );
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !marker.exists() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("server must receive exact request cancellation");
    connection.kill();
}

struct HttpScript {
    replies: Mutex<std::collections::VecDeque<heycode_http::HttpResponse>>,
    bodies: Mutex<Vec<serde_json::Value>>,
}

impl heycode_http::HttpTransport for HttpScript {
    fn send(
        &self,
        request: heycode_http::HttpRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::BufferedResponseFuture {
        self.bodies
            .lock()
            .unwrap()
            .push(serde_json::from_slice(request.body().unwrap_or(b"null")).unwrap_or_default());
        let reply = self.replies.lock().unwrap().pop_front();
        Box::pin(async move { Ok(reply.expect("scripted response")) })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

fn http_response(
    status: u16,
    content_type: Option<&str>,
    body: impl Into<Vec<u8>>,
) -> heycode_http::HttpResponse {
    heycode_http::HttpResponse {
        status,
        content_type: content_type.map(str::to_owned),
        headers: std::collections::BTreeMap::new(),
        body: body.into(),
    }
}

#[tokio::test]
async fn finite_http_sse_routes_elicitation_reply_and_log_on_the_same_connection() {
    let init = serde_json::json!({
        "jsonrpc":"2.0","id":1,
        "result":{"protocolVersion":"2025-11-25","capabilities":{"logging":{}},"serverInfo":{"name":"fixture","version":"1"}}
    });
    let elicitation = serde_json::json!({
        "jsonrpc":"2.0","id":"http-ask","method":"elicitation/create",
        "params":{"mode":"form","message":"review","requestedSchema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"]}}
    });
    let log = serde_json::json!({
        "jsonrpc":"2.0","method":"notifications/message",
        "params":{"level":"info","data":"http-log"}
    });
    let response = serde_json::json!({"jsonrpc":"2.0","id":2,"result":{"ok":true}});
    let sse = format!("data: {elicitation}\n\ndata: {log}\n\ndata: {response}\n\n");
    let transport = Arc::new(HttpScript {
        replies: Mutex::new(
            vec![
                http_response(
                    200,
                    Some("application/json"),
                    serde_json::to_vec(&init).unwrap(),
                ),
                http_response(202, None, Vec::new()),
                http_response(200, Some("text/event-stream"), sse.into_bytes()),
                http_response(202, None, Vec::new()),
            ]
            .into_iter()
            .collect(),
        ),
        bodies: Mutex::new(Vec::new()),
    });
    let (client_events, sink) = router(Arc::new(AcceptingHandler));
    let notification_router = heycode_mcp::McpNotificationRouter::with_client_events(
        heycode_mcp::resources::McpResourceListLimits::default(),
        client_events,
    );
    let definition = heycode_mcp::McpStreamableHttpTransport::new(
        "https://mcp.example.test/endpoint",
        std::collections::BTreeMap::new(),
    )
    .unwrap();
    let client = heycode_mcp::McpStreamableHttpClient::new(
        heycode_http::HttpService::new(transport.clone()),
        &definition,
        notification_router,
        heycode_mcp::McpTimeouts::default(),
    )
    .unwrap();
    client.initialize(&CancellationToken::new()).await.unwrap();
    let result = client
        .request(
            "tools/call",
            serde_json::json!({}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result, serde_json::json!({"ok":true}));
    assert!(matches!(
        sink.0.lock().unwrap().as_slice(),
        [McpClientEvent::Log(_)]
    ));
    let bodies = transport.bodies.lock().unwrap();
    assert_eq!(
        bodies[0]["params"]["capabilities"]["elicitation"]["form"],
        serde_json::json!({})
    );
    assert_eq!(bodies[3]["id"], "http-ask");
    assert_eq!(bodies[3]["result"]["content"]["answer"], "reviewed");
}
