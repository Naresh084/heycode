//! P13 — the WebSocket seam, its HTTP fallback and its reconnect metrics.
//!
//! No provider heycode is configured to talk to offers a WebSocket transport for
//! the request shapes this crate carries, so the branch that runs in
//! production is the fallback. That branch is driven here against real
//! loopback HTTP servers.
//!
//! The WebSocket branch is driven by [`LoopbackConnector`], which is **not**
//! RFC 6455: with no WebSocket library in the tree there is no handshake to
//! perform. What it does supply is the part that actually decides P13's
//! behaviour — a real socket whose connect is genuinely refused by the
//! operating system, whose stream genuinely dies mid-session, and whose sends
//! genuinely arrive at a listener. Every count asserted below is produced by
//! one of those events; none comes from a mock returning a canned answer.
//!
//! Every case that can wait on time is wrapped whole in `tokio::time::timeout`
//! so that a regression fails the gate instead of hanging it.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt as _;
use heycode_http::{
    FallbackReason, HttpService, HttpSseRequest, ReconnectPolicy, ReqwestHttpTransport, SseEvent,
    StreamPlan, StreamTransport, StreamTransportMetricsSnapshot, StreamWire, TransportError,
    WebSocketConnectFuture, WebSocketConnectRequest, WebSocketConnection, WebSocketConnector,
    WebSocketMessage, WebSocketRequest, WebSocketSendFuture, WebSocketSink,
};
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::net::tcp::OwnedWriteHalf;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(5);

/// Line the frame server plays to make an established connection fail.
const DROP_LINE: &str = "!drop";
/// Line the frame server plays to deliver a binary frame.
const BINARY_LINE: &str = "!binary";

// ── loopback WebSocket stand-in ─────────────────────────────────────────────

struct LoopbackSink {
    writer: tokio::sync::Mutex<OwnedWriteHalf>,
}

impl WebSocketSink for LoopbackSink {
    fn send_text(&self, text: String, cancellation: CancellationToken) -> WebSocketSendFuture<'_> {
        Box::pin(async move {
            let mut writer = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(TransportError::Cancelled),
                writer = self.writer.lock() => writer,
            };
            let line = format!("{text}\n");
            tokio::select! {
                biased;
                () = cancellation.cancelled() => Err(TransportError::Cancelled),
                written = writer.write_all(line.as_bytes()) => written.map_err(|_| {
                    TransportError::Network { message: "frame could not be sent".to_owned() }
                }),
            }
        })
    }
}

#[derive(Default)]
struct LoopbackConnector {
    /// One address per open attempt. An address with no listener behind it
    /// produces a real connection refusal from the operating system.
    script: Mutex<VecDeque<SocketAddr>>,
    /// The handshake endpoint observed on each attempt, in order.
    attempts: Mutex<Vec<String>>,
}

impl LoopbackConnector {
    fn scripted(addresses: &[SocketAddr]) -> Self {
        Self {
            script: Mutex::new(addresses.iter().copied().collect()),
            attempts: Mutex::new(Vec::new()),
        }
    }

    fn attempts(&self) -> Vec<String> {
        self.attempts.lock().unwrap().clone()
    }
}

impl WebSocketConnector for LoopbackConnector {
    fn connect(
        &self,
        request: WebSocketConnectRequest,
        cancellation: CancellationToken,
    ) -> WebSocketConnectFuture {
        let address = self.script.lock().unwrap().pop_front();
        self.attempts.lock().unwrap().push(request.url().to_owned());
        Box::pin(async move {
            let Some(address) = address else {
                return Err(TransportError::Network {
                    message: "no scripted attempt remains".to_owned(),
                });
            };
            let socket = tokio::select! {
                biased;
                () = cancellation.cancelled() => return Err(TransportError::Cancelled),
                socket = tokio::net::TcpStream::connect(address) => socket,
            }
            .map_err(|_| TransportError::Network {
                message: "connection refused".to_owned(),
            })?;
            let (reader, writer) = socket.into_split();
            let sink = Arc::new(LoopbackSink {
                writer: tokio::sync::Mutex::new(writer),
            });
            let messages = Box::pin(futures::stream::unfold(
                tokio::io::BufReader::new(reader),
                |mut reader| async move {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => None,
                        Ok(_) => {
                            let line = line.trim_end_matches(['\r', '\n']).to_owned();
                            let item = match line.as_str() {
                                DROP_LINE => Err(TransportError::Network {
                                    message: "connection lost".to_owned(),
                                }),
                                BINARY_LINE => Ok(WebSocketMessage::Binary(vec![0xff, 0xfe])),
                                _ => Ok(WebSocketMessage::Text(line)),
                            };
                            Some((item, reader))
                        }
                        Err(_) => Some((
                            Err(TransportError::Network {
                                message: "read failed".to_owned(),
                            }),
                            reader,
                        )),
                    }
                },
            ));
            Ok(WebSocketConnection {
                sink: sink as Arc<dyn WebSocketSink>,
                messages,
            })
        })
    }
}

#[derive(Default)]
struct RejectingOpeningSink {
    sends: std::sync::atomic::AtomicUsize,
}

impl WebSocketSink for RejectingOpeningSink {
    fn send_text(
        &self,
        _text: String,
        _cancellation: CancellationToken,
    ) -> WebSocketSendFuture<'_> {
        self.sends.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Err(TransportError::Network {
                message: "opening frame was not accepted".to_owned(),
            })
        })
    }
}

struct EstablishedConnector {
    sink: Arc<RejectingOpeningSink>,
}

impl WebSocketConnector for EstablishedConnector {
    fn connect(
        &self,
        _request: WebSocketConnectRequest,
        _cancellation: CancellationToken,
    ) -> WebSocketConnectFuture {
        let sink = Arc::clone(&self.sink);
        Box::pin(async move {
            Ok(WebSocketConnection {
                sink,
                messages: Box::pin(futures::stream::pending()),
            })
        })
    }
}

struct CancelledConnector {
    attempts: std::sync::atomic::AtomicUsize,
}

impl WebSocketConnector for CancelledConnector {
    fn connect(
        &self,
        _request: WebSocketConnectRequest,
        _cancellation: CancellationToken,
    ) -> WebSocketConnectFuture {
        self.attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Err(TransportError::Cancelled) })
    }
}

// ── local listeners ─────────────────────────────────────────────────────────

/// An address bound and immediately released, so a dial to it is refused
/// rather than left hanging.
async fn refused_address() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

/// A listener that plays `server_lines` at the connector, then reads exactly
/// `expected_client_lines` back. Passing zero closes the socket immediately,
/// which is how a clean end-of-stream is produced.
async fn frame_server(
    server_lines: &[&str],
    expected_client_lines: usize,
) -> (SocketAddr, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server_lines = server_lines
        .iter()
        .map(|line| (*line).to_owned())
        .collect::<Vec<_>>();
    let worker = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = socket.into_split();
        for line in server_lines {
            if writer
                .write_all(format!("{line}\n").as_bytes())
                .await
                .is_err()
            {
                break;
            }
        }
        let mut received = Vec::new();
        let mut reader = tokio::io::BufReader::new(reader);
        while received.len() < expected_client_lines {
            let mut line = String::new();
            match reader.read_line(&mut line).await {
                Ok(0) | Err(_) => break,
                Ok(_) => received.push(line.trim_end().to_owned()),
            }
        }
        received
    });
    (address, worker)
}

/// A minimal HTTP listener that answers one request with an SSE response.
async fn sse_server(events: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{events}"
    );
    let worker = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = socket.into_split();
        let mut reader = tokio::io::BufReader::new(reader);
        let mut line = String::new();
        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
            if line == "\r\n" || line == "\n" {
                break;
            }
            line.clear();
        }
        let _written = writer.write_all(response.as_bytes()).await;
        let _closed = writer.shutdown().await;
    });
    (format!("http://{address}/events"), worker)
}

// ── helpers ─────────────────────────────────────────────────────────────────

fn http_service() -> HttpService {
    HttpService::new(Arc::new(ReqwestHttpTransport::new().unwrap()))
}

fn websocket_url(address: SocketAddr) -> WebSocketRequest {
    WebSocketRequest::new(format!("ws://{address}/socket")).unwrap()
}

fn data_of(items: &[Result<SseEvent, TransportError>]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| item.as_ref().ok())
        .map(|event| event.data.clone())
        .collect()
}

// ── HTTP is the only wire any configured provider offers ────────────────────

#[tokio::test]
async fn an_http_only_plan_runs_on_http_and_is_never_recorded_as_a_fallback() {
    tokio::time::timeout(TIMEOUT, async {
        let (url, worker) = sse_server("data: one\n\ndata: two\n\n").await;
        let transport = StreamTransport::http_only(http_service());
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::http_only(HttpSseRequest::get(url).unwrap()),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["one", "two"]);
        let facts = session.outcome.get().expect("a polled session has decided");
        assert_eq!(facts.wire(), Some(StreamWire::HttpSse));
        assert_eq!(facts.fallback(), None);
        assert!(!facts.fell_back(), "HTTP by plan is not a fallback");
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                http_sessions: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("an HTTP-only session must complete");
}

#[tokio::test]
async fn offering_a_websocket_with_no_connector_installed_falls_back_and_records_the_reason() {
    tokio::time::timeout(TIMEOUT, async {
        // This is heycode's production configuration today: a plan may name a
        // WebSocket, and no build has a connector to open one.
        let (url, worker) = sse_server("data: fallback\n\n").await;
        let transport = StreamTransport::http_only(http_service());
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::preferring_websocket(
                WebSocketRequest::new("wss://provider.test/socket").unwrap(),
                HttpSseRequest::get(url).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["fallback"]);
        let facts = session.outcome.get().expect("a polled session has decided");
        assert_eq!(facts.wire(), Some(StreamWire::HttpSse));
        assert_eq!(facts.fallback(), Some(FallbackReason::NoConnector));
        assert!(facts.fell_back());
        assert_eq!(facts.websocket_attempts(), 0);
        assert!(
            session.sink.get().is_none(),
            "an HTTP session has no client-to-server channel"
        );
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                fallbacks: 1,
                http_sessions: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a fallback session must complete");
}

// ── reconnect budget ────────────────────────────────────────────────────────

#[tokio::test]
async fn a_failed_open_with_no_reconnect_budget_falls_back_after_exactly_one_attempt() {
    tokio::time::timeout(TIMEOUT, async {
        let (url, worker) = sse_server("data: fallback\n\n").await;
        let closed = refused_address().await;
        let connector = Arc::new(LoopbackConnector::scripted(&[closed]));
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        )
        .with_reconnect_policy(ReconnectPolicy::none());
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(closed),
                HttpSseRequest::get(url).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["fallback"]);
        assert_eq!(connector.attempts().len(), 1, "no reconnect was permitted");
        let facts = session.outcome.get().expect("a polled session has decided");
        assert_eq!(facts.fallback(), Some(FallbackReason::OpenFailed));
        assert_eq!(facts.websocket_attempts(), 1);
        assert_eq!(facts.reconnects(), 0);
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                websocket_attempts: 1,
                fallbacks: 1,
                http_sessions: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a single failed open must settle");
}

#[tokio::test]
async fn every_reconnect_in_the_budget_is_attempted_and_counted_before_the_http_fallback() {
    tokio::time::timeout(TIMEOUT, async {
        let (url, worker) = sse_server("data: fallback\n\n").await;
        let closed = refused_address().await;
        // Four addresses for a budget that permits four opens; a fifth dial
        // would exhaust the script and is therefore observable.
        let connector = Arc::new(LoopbackConnector::scripted(&[closed; 4]));
        let policy =
            ReconnectPolicy::new(3, Duration::from_millis(1), Duration::from_millis(5)).unwrap();
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        )
        .with_reconnect_policy(policy);
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(closed),
                HttpSseRequest::get(url).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["fallback"]);
        assert_eq!(
            connector.attempts().len(),
            4,
            "one open plus a budget of three reconnects, and no more"
        );
        let facts = session.outcome.get().expect("a polled session has decided");
        assert_eq!(
            facts.fallback(),
            Some(FallbackReason::ReconnectBudgetExhausted)
        );
        assert_eq!(facts.websocket_attempts(), 4);
        assert_eq!(facts.reconnects(), 3);
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                websocket_attempts: 4,
                reconnects: 3,
                reconnect_budget_exhausted: 1,
                fallbacks: 1,
                http_sessions: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a spent reconnect budget must settle onto HTTP");
}

#[tokio::test]
async fn a_reconnect_that_succeeds_runs_on_the_websocket_and_never_reaches_http() {
    tokio::time::timeout(TIMEOUT, async {
        let closed = refused_address().await;
        let (live, worker) = frame_server(&["alpha", "beta"], 0).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[closed, closed, live]));
        let policy =
            ReconnectPolicy::new(3, Duration::from_millis(1), Duration::from_millis(5)).unwrap();
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        )
        .with_reconnect_policy(policy);
        let metrics = transport.metrics();

        // The HTTP request names a closed port, so any silent fallback would
        // surface as a network error instead of the WebSocket's events.
        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(live),
                HttpSseRequest::get(format!("http://{closed}/events")).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["alpha", "beta"]);
        assert!(items.iter().all(Result::is_ok), "{items:?}");
        let facts = session.outcome.get().expect("a polled session has decided");
        assert_eq!(facts.wire(), Some(StreamWire::WebSocket));
        assert_eq!(facts.fallback(), None);
        assert_eq!(facts.reconnects(), 2);
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                websocket_attempts: 3,
                websocket_opens: 1,
                reconnects: 2,
                websocket_sessions: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a third-attempt open must run on the WebSocket");
}

#[tokio::test]
async fn a_connection_that_fails_after_it_opened_is_terminal_rather_than_a_silent_fallback() {
    tokio::time::timeout(TIMEOUT, async {
        // Once a connection exists the transport cannot know whether the
        // opening frames were processed, so replaying them anywhere could
        // duplicate a provider request. The session ends instead.
        let closed = refused_address().await;
        let (live, worker) = frame_server(&["alpha", DROP_LINE], 0).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[live, live, live, live]));
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        );
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(live),
                HttpSseRequest::get(format!("http://{closed}/events")).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["alpha"]);
        assert!(
            matches!(items.last(), Some(Err(TransportError::Network { .. }))),
            "{items:?}"
        );
        assert_eq!(
            connector.attempts().len(),
            1,
            "an established connection is never reopened"
        );
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                websocket_attempts: 1,
                websocket_opens: 1,
                websocket_sessions: 1,
                websocket_stream_failures: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a lost connection must end the session");
}

// ── fallback must not withdraw a capability ─────────────────────────────────

#[tokio::test]
async fn a_bidirectional_plan_with_no_connector_is_failed_rather_than_downgraded_onto_http() {
    tokio::time::timeout(TIMEOUT, async {
        let transport = StreamTransport::http_only(http_service());
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::requiring_websocket(
                WebSocketRequest::new("wss://provider.test/socket").unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;

        assert!(
            matches!(
                &items[..],
                [Err(TransportError::InvalidRequest { field: "transport", message })]
                    if message.contains("client-to-server")
                        && message.contains("no WebSocket connector is installed")
            ),
            "{items:?}"
        );
        let facts = session
            .outcome
            .get()
            .expect("a refusal is still a decision");
        assert_eq!(facts.wire(), None);
        assert_eq!(facts.fallback(), Some(FallbackReason::NoConnector));
        assert!(facts.refused_fallback());
        assert!(!facts.fell_back());
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                fallbacks_refused: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a refused fallback must settle");
}

#[tokio::test]
async fn a_bidirectional_plan_is_refused_after_the_reconnect_budget_too_not_only_up_front() {
    tokio::time::timeout(TIMEOUT, async {
        let closed = refused_address().await;
        let connector = Arc::new(LoopbackConnector::scripted(&[closed; 2]));
        let policy =
            ReconnectPolicy::new(1, Duration::from_millis(1), Duration::from_millis(2)).unwrap();
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        )
        .with_reconnect_policy(policy);
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::requiring_websocket(websocket_url(closed)),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;

        assert!(
            matches!(
                &items[..],
                [Err(TransportError::InvalidRequest {
                    field: "transport",
                    ..
                })]
            ),
            "{items:?}"
        );
        assert_eq!(connector.attempts().len(), 2);
        let facts = session
            .outcome
            .get()
            .expect("a refusal is still a decision");
        assert!(facts.refused_fallback());
        assert_eq!(
            facts.fallback(),
            Some(FallbackReason::ReconnectBudgetExhausted)
        );
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                websocket_attempts: 2,
                reconnects: 1,
                reconnect_budget_exhausted: 1,
                fallbacks_refused: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a refused fallback after reconnects must settle");
}

#[tokio::test]
async fn the_client_to_server_channel_exists_only_on_a_websocket_session() {
    tokio::time::timeout(TIMEOUT, async {
        let (live, worker) = frame_server(&["alpha"], 1).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[live]));
        let transport = StreamTransport::with_connector(
            http_service(),
            connector as Arc<dyn WebSocketConnector>,
        );

        let session = transport.open(
            StreamPlan::requiring_websocket(websocket_url(live)),
            CancellationToken::new(),
        );
        let mut events = session.events;
        let first = events.next().await;
        assert!(matches!(first, Some(Ok(SseEvent { ref data, .. })) if data == "alpha"));

        let sink = session
            .sink
            .get()
            .expect("a WebSocket session exposes its sink");
        sink.send_text("client-frame".to_owned(), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(worker.await.unwrap(), ["client-frame"]);
    })
    .await
    .expect("a client frame must reach the listener");
}

// ── cancellation ────────────────────────────────────────────────────────────

#[tokio::test]
async fn cancellation_before_the_wire_is_chosen_yields_cancelled_and_dials_nothing() {
    tokio::time::timeout(TIMEOUT, async {
        let closed = refused_address().await;
        let connector = Arc::new(LoopbackConnector::scripted(&[closed]));
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        );
        let metrics = transport.metrics();
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(closed),
                HttpSseRequest::get(format!("http://{closed}/events")).unwrap(),
            ),
            cancellation,
        );
        let items = session.events.collect::<Vec<_>>().await;

        assert_eq!(items, [Err(TransportError::Cancelled)]);
        assert!(connector.attempts().is_empty());
        assert!(
            session.outcome.get().is_none(),
            "no wire was chosen, which is unknown rather than HTTP"
        );
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a pre-cancelled session must settle immediately");
}

#[tokio::test]
async fn cancellation_during_reconnect_backoff_settles_without_waiting_out_the_delay() {
    // The backoff is far longer than the case deadline, so only a cancellation
    // that actually interrupts the sleep can let this pass.
    tokio::time::timeout(TIMEOUT, async {
        let closed = refused_address().await;
        let connector = Arc::new(LoopbackConnector::scripted(&[closed; 4]));
        let policy =
            ReconnectPolicy::new(3, Duration::from_secs(30), Duration::from_secs(60)).unwrap();
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        )
        .with_reconnect_policy(policy);
        let metrics = transport.metrics();
        let cancellation = CancellationToken::new();

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(closed),
                HttpSseRequest::get(format!("http://{closed}/events")).unwrap(),
            ),
            cancellation.clone(),
        );
        let events = session.events;
        let collector = tokio::spawn(async move { events.collect::<Vec<_>>().await });

        while connector.attempts().is_empty() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        cancellation.cancel();

        assert_eq!(collector.await.unwrap(), [Err(TransportError::Cancelled)]);
        assert_eq!(metrics.snapshot().websocket_attempts, 1);
        assert_eq!(
            metrics.snapshot().fallbacks,
            0,
            "cancelling is not a fallback"
        );
    })
    .await
    .expect("cancellation must interrupt the backoff rather than wait it out");
}

#[tokio::test]
async fn cancellation_mid_websocket_stream_ends_the_session_after_the_delivered_events() {
    tokio::time::timeout(TIMEOUT, async {
        // The listener waits for a client line that never comes, so the
        // connection stays open until cancellation ends it.
        let (live, worker) = frame_server(&["alpha"], 1).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[live]));
        let transport = StreamTransport::with_connector(
            http_service(),
            connector as Arc<dyn WebSocketConnector>,
        );
        let cancellation = CancellationToken::new();

        let session = transport.open(
            StreamPlan::requiring_websocket(websocket_url(live)),
            cancellation.clone(),
        );
        let mut events = session.events;
        assert!(matches!(
            events.next().await,
            Some(Ok(SseEvent { ref data, .. })) if data == "alpha"
        ));
        cancellation.cancel();
        assert_eq!(events.next().await, Some(Err(TransportError::Cancelled)));
        worker.abort();
    })
    .await
    .expect("cancellation must end an open WebSocket session");
}

// ── framing ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_binary_frame_is_a_framing_failure_rather_than_a_fabricated_event() {
    tokio::time::timeout(TIMEOUT, async {
        let (live, worker) = frame_server(&["alpha", BINARY_LINE], 0).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[live]));
        let transport = StreamTransport::with_connector(
            http_service(),
            connector as Arc<dyn WebSocketConnector>,
        );
        let metrics = transport.metrics();

        let session = transport.open(
            StreamPlan::requiring_websocket(websocket_url(live)),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        assert_eq!(data_of(&items), ["alpha"]);
        assert!(
            matches!(
                items.last(),
                Some(Err(TransportError::InvalidSse { message })) if message.contains("binary")
            ),
            "{items:?}"
        );
        assert_eq!(metrics.snapshot().websocket_stream_failures, 1);
    })
    .await
    .expect("a binary frame must terminate the session");
}

#[tokio::test]
async fn text_frames_become_message_events_without_any_provider_interpretation() {
    tokio::time::timeout(TIMEOUT, async {
        // Framing is transport; the meaning of `data` belongs to the protocol
        // adapter. `[DONE]` and SSE-looking text must survive verbatim.
        let (live, worker) =
            frame_server(&["[DONE]", "{\"delta\":\"x\"}", "data: not-sse"], 0).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[live]));
        let transport = StreamTransport::with_connector(
            http_service(),
            connector as Arc<dyn WebSocketConnector>,
        );

        let session = transport.open(
            StreamPlan::requiring_websocket(websocket_url(live)),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;
        worker.await.unwrap();

        let events = items.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
        assert_eq!(
            events,
            [
                SseEvent {
                    event: "message".to_owned(),
                    data: "[DONE]".to_owned(),
                    id: None,
                    retry_ms: None,
                },
                SseEvent {
                    event: "message".to_owned(),
                    data: "{\"delta\":\"x\"}".to_owned(),
                    id: None,
                    retry_ms: None,
                },
                SseEvent {
                    event: "message".to_owned(),
                    data: "data: not-sse".to_owned(),
                    id: None,
                    retry_ms: None,
                },
            ]
        );
    })
    .await
    .expect("text frames must pass through uninterpreted");
}

#[tokio::test]
async fn opening_frames_are_sent_once_only_after_a_handshake_succeeds() {
    tokio::time::timeout(TIMEOUT, async {
        let closed = refused_address().await;
        let (live, worker) = frame_server(&["alpha"], 1).await;
        let connector = Arc::new(LoopbackConnector::scripted(&[closed, live]));
        let policy =
            ReconnectPolicy::new(1, Duration::from_millis(1), Duration::from_millis(2)).unwrap();
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        )
        .with_reconnect_policy(policy);

        let session = transport.open(
            StreamPlan::requiring_websocket(websocket_url(live).open_frame("setup").unwrap()),
            CancellationToken::new(),
        );
        let mut events = session.events;
        assert!(matches!(
            events.next().await,
            Some(Ok(SseEvent { ref data, .. })) if data == "alpha"
        ));

        assert_eq!(
            connector.attempts().len(),
            2,
            "one failed handshake and one successful handshake ran"
        );
        assert_eq!(
            worker.await.unwrap(),
            ["setup"],
            "no opening frame was replayed through the failed handshake"
        );
    })
    .await
    .expect("opening frames must reach the opened connection");
}

#[tokio::test]
async fn an_opening_frame_failure_after_handshake_is_terminal_and_never_falls_back() {
    tokio::time::timeout(TIMEOUT, async {
        let sink = Arc::new(RejectingOpeningSink::default());
        let connector = Arc::new(EstablishedConnector {
            sink: Arc::clone(&sink),
        });
        let transport = StreamTransport::with_connector(
            http_service(),
            connector as Arc<dyn WebSocketConnector>,
        );
        let metrics = transport.metrics();
        let closed = refused_address().await;

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(closed).open_frame("setup").unwrap(),
                HttpSseRequest::get(format!("http://{closed}/events")).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;

        assert!(matches!(
            items.as_slice(),
            [Err(TransportError::Network { message })] if message.contains("opening frame")
        ));
        assert_eq!(sink.sends.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(
            session.outcome.get().unwrap().wire(),
            Some(StreamWire::WebSocket)
        );
        assert!(session.sink.get().is_none());
        assert_eq!(
            metrics.snapshot(),
            StreamTransportMetricsSnapshot {
                sessions: 1,
                websocket_attempts: 1,
                websocket_opens: 1,
                websocket_stream_failures: 1,
                ..StreamTransportMetricsSnapshot::default()
            }
        );
    })
    .await
    .expect("a failed opening frame must settle without retry or fallback");
}

#[tokio::test]
async fn a_connector_cancellation_is_terminal_even_when_the_caller_token_is_still_live() {
    tokio::time::timeout(TIMEOUT, async {
        let connector = Arc::new(CancelledConnector {
            attempts: std::sync::atomic::AtomicUsize::new(0),
        });
        let transport = StreamTransport::with_connector(
            http_service(),
            Arc::clone(&connector) as Arc<dyn WebSocketConnector>,
        );
        let metrics = transport.metrics();
        let closed = refused_address().await;

        let session = transport.open(
            StreamPlan::preferring_websocket(
                websocket_url(closed),
                HttpSseRequest::get(format!("http://{closed}/events")).unwrap(),
            ),
            CancellationToken::new(),
        );
        let items = session.events.collect::<Vec<_>>().await;

        assert_eq!(items, [Err(TransportError::Cancelled)]);
        assert_eq!(
            connector.attempts.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "cancellation is not a retryable handshake failure"
        );
        assert_eq!(metrics.snapshot().fallbacks, 0);
        assert_eq!(metrics.snapshot().reconnects, 0);
        assert!(session.outcome.get().is_none());
    })
    .await
    .expect("connector cancellation must settle immediately");
}

// ── request and policy validation ───────────────────────────────────────────

#[test]
fn a_websocket_url_must_be_host_qualified_ws_or_wss_without_embedded_credentials() {
    for rejected in [
        "socket",
        "/socket",
        "http://provider.test/socket",
        "https://provider.test/socket",
        "file:///socket",
        "ws://user@provider.test/socket",
        "wss://user:secret@provider.test/socket",
    ] {
        // `.err().expect(..)` rather than `.unwrap_err()`: the latter needs
        // `Debug` on the Ok type, and `WebSocketRequest` deliberately has no
        // `Debug` because its headers and opening frames may carry
        // authorization values. The test bends, not the type.
        let error = WebSocketRequest::new(rejected)
            .err()
            .expect("credential-bearing and non-ws URLs must be refused");
        assert!(
            matches!(&error, TransportError::InvalidRequest { field: "url", .. }),
            "{rejected} produced {error:?}"
        );
        assert!(
            !format!("{error:?} {error}").contains("secret"),
            "a rejection must not echo the credential it rejected"
        );
    }
    // Accepted URLs keep everything that identifies the endpoint. The scheme's
    // default port is not part of that: `wss://host:443` and `wss://host` name
    // the same socket, and `Url` normalises the redundant port away. Asserting
    // byte-identity here would pin URL normalisation rather than the guard, so
    // each case states the form it is expected to settle into.
    for (accepted, normalised) in [
        ("ws://provider.test/socket", "ws://provider.test/socket"),
        (
            "wss://provider.test:443/socket?model=x",
            "wss://provider.test/socket?model=x",
        ),
        // A non-default port IS identifying, so it must survive.
        (
            "wss://provider.test:8443/socket",
            "wss://provider.test:8443/socket",
        ),
    ] {
        assert_eq!(
            WebSocketRequest::new(accepted).unwrap().url(),
            normalised,
            "{accepted}"
        );
    }
}

#[test]
fn opening_frames_are_bounded_before_a_connector_can_observe_them() {
    let oversized = "x".repeat(1024 * 1024 + 1);
    let error = WebSocketRequest::new("wss://provider.test/socket")
        .unwrap()
        .open_frame(oversized)
        .err()
        .expect("an oversized opening frame must be refused");
    assert!(matches!(
        error,
        TransportError::InvalidRequest {
            field: "open_frames",
            ..
        }
    ));

    let mut request = WebSocketRequest::new("wss://provider.test/socket").unwrap();
    for index in 0..16 {
        request = request.open_frame(format!("frame-{index}")).unwrap();
    }
    let error = request
        .open_frame("one-too-many")
        .err()
        .expect("the opening-frame count must be bounded");
    assert!(matches!(
        error,
        TransportError::InvalidRequest {
            field: "open_frames",
            ..
        }
    ));
}

#[test]
fn a_reconnect_policy_refuses_an_unbounded_or_inverted_budget() {
    assert!(matches!(
        ReconnectPolicy::new(9, Duration::from_millis(1), Duration::from_secs(1)),
        Err(TransportError::InvalidRequest {
            field: "max_reconnects",
            ..
        })
    ));
    assert!(matches!(
        ReconnectPolicy::new(1, Duration::from_secs(2), Duration::from_secs(1)),
        Err(TransportError::InvalidRequest {
            field: "reconnect_backoff",
            ..
        })
    ));
    assert!(matches!(
        ReconnectPolicy::new(1, Duration::from_secs(1), Duration::from_secs(61)),
        Err(TransportError::InvalidRequest {
            field: "reconnect_backoff",
            ..
        })
    ));
    assert_eq!(
        ReconnectPolicy::new(8, Duration::from_millis(1), Duration::from_secs(60))
            .unwrap()
            .max_reconnects(),
        8
    );
    assert_eq!(ReconnectPolicy::default().max_reconnects(), 3);
    assert_eq!(ReconnectPolicy::none().max_reconnects(), 0);
}

#[tokio::test]
async fn an_unpolled_session_has_decided_nothing_and_counted_nothing() {
    let transport = StreamTransport::http_only(http_service());
    let metrics = transport.metrics();
    let session = transport.open(
        StreamPlan::http_only(HttpSseRequest::get("https://provider.test/events").unwrap()),
        CancellationToken::new(),
    );

    assert!(session.outcome.get().is_none());
    assert!(session.sink.get().is_none());
    assert_eq!(
        metrics.snapshot(),
        StreamTransportMetricsSnapshot::default(),
        "a session that never ran must not appear in the counts"
    );
}
