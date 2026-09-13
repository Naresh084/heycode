//! Provider-neutral dynamic response ownership for concurrent protocols.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use futures::StreamExt as _;
use heycode_http::{HttpRequest, HttpTransport, ReqwestHttpTransport, TransportError};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(5);

async fn chunked_server(
    first: &'static [u8],
    second: &'static [u8],
) -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (opened_tx, opened_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 4096];
        let _read = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nX-Private: header-canary\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        write_chunk(&mut socket, first).await;
        let _opened = opened_tx.send(());
        let _released = release_rx.await;
        write_chunk(&mut socket, second).await;
        socket.write_all(b"0\r\n\r\n").await.unwrap();
        socket.shutdown().await.unwrap();
    });
    (
        format!("http://{address}/mcp"),
        opened_rx,
        release_tx,
        worker,
    )
}

async fn write_chunk(socket: &mut tokio::net::TcpStream, body: &[u8]) {
    socket
        .write_all(format!("{:X}\r\n", body.len()).as_bytes())
        .await
        .unwrap();
    socket.write_all(body).await.unwrap();
    socket.write_all(b"\r\n").await.unwrap();
    socket.flush().await.unwrap();
}

#[tokio::test]
async fn headers_arrive_before_body_completion_and_chunks_are_pull_driven() {
    let (url, opened, release, worker) =
        chunked_server(b"data: first\n\n", b"data: second\n\n").await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let mut response = transport
        .stream_response(
            HttpRequest::post(url, b"{}".to_vec())
                .unwrap()
                .with_max_response_bytes(1024),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, opened)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(response.status(), 200);
    assert_eq!(response.content_type(), Some("text/event-stream"));
    assert_eq!(response.header("x-private"), Some("header-canary"));
    let debug = format!("{response:?}");
    assert!(!debug.contains("header-canary"));
    assert!(debug.contains("x-private"));
    assert_eq!(
        response.body_mut().next().await.unwrap().unwrap(),
        b"data: first\n\n"
    );

    let _ = release.send(());
    assert_eq!(
        response.body_mut().next().await.unwrap().unwrap(),
        b"data: second\n\n"
    );
    assert!(response.body_mut().next().await.is_none());
    worker.await.unwrap();
}

#[tokio::test]
async fn caller_cancellation_terminates_the_owned_body_without_a_success_tail() {
    let (url, opened, _release, worker) = chunked_server(b"first", b"late").await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let cancellation = CancellationToken::new();
    let mut response = transport
        .stream_response(
            HttpRequest::get(url).unwrap().with_max_response_bytes(1024),
            cancellation.clone(),
        )
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, opened)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.body_mut().next().await.unwrap().unwrap(), b"first");
    cancellation.cancel();
    assert_eq!(
        response.body_mut().next().await,
        Some(Err(TransportError::Cancelled))
    );
    assert!(response.body_mut().next().await.is_none());
    worker.abort();
    let _ = worker.await;
}

#[tokio::test]
async fn cumulative_streamed_bytes_enforce_the_request_cap_without_leaking_body() {
    let (url, opened, release, worker) = chunked_server(b"123456", b"7890").await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let mut response = transport
        .stream_response(
            HttpRequest::get(url).unwrap().with_max_response_bytes(8),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    tokio::time::timeout(TIMEOUT, opened)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        response.body_mut().next().await.unwrap().unwrap(),
        b"123456"
    );
    let _ = release.send(());
    let error = response.body_mut().next().await.unwrap().unwrap_err();
    assert_eq!(error, TransportError::ResponseTooLarge { max_bytes: 8 });
    assert!(!format!("{error:?} {error}").contains("7890"));
    assert!(response.body_mut().next().await.is_none());
    worker.await.unwrap();
}

struct BufferedOnly;

impl HttpTransport for BufferedOnly {
    fn send(
        &self,
        _request: HttpRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::BufferedResponseFuture {
        Box::pin(async {
            Ok(heycode_http::HttpResponse {
                status: 200,
                content_type: Some("application/json".to_owned()),
                headers: std::collections::BTreeMap::new(),
                body: b"{\"ok\":true}".to_vec(),
            })
        })
    }

    fn sse(
        &self,
        _request: heycode_http::HttpSseRequest,
        _cancellation: CancellationToken,
    ) -> heycode_http::SseEventStream {
        Box::pin(futures::stream::empty())
    }
}

#[tokio::test]
async fn buffered_transports_adapt_to_one_owned_chunk_without_claiming_empty() {
    let mut response = BufferedOnly
        .stream_response(
            HttpRequest::get("https://example.test/mcp").unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(response.content_type(), Some("application/json"));
    assert_eq!(
        response.body_mut().next().await.unwrap().unwrap(),
        b"{\"ok\":true}"
    );
    assert!(response.body_mut().next().await.is_none());
}
