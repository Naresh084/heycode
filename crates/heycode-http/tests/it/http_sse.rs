//! Raw HTTP/SSE transport integration without provider JSON semantics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use futures::StreamExt as _;
use heycode_http::{
    HttpRetryAfter, HttpSseRequest, HttpTransport, ReqwestHttpTransport, SseEvent, SseEventStream,
    TransportError,
};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(5);

async fn server(response_fragments: Vec<Vec<u8>>) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut captured = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            let count = socket.read(&mut chunk).await.unwrap();
            if count == 0 {
                break;
            }
            captured.extend_from_slice(&chunk[..count]);
            let Some(head_end) = captured.windows(4).position(|window| window == b"\r\n\r\n")
            else {
                continue;
            };
            let head_end = head_end + 4;
            let head = String::from_utf8_lossy(&captured[..head_end]).to_lowercase();
            let content_length = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .map(|value| value.trim().parse::<usize>().unwrap())
                .unwrap_or(0);
            if captured.len() >= head_end + content_length {
                break;
            }
        }
        for fragment in response_fragments {
            socket.write_all(&fragment).await.unwrap();
            tokio::task::yield_now().await;
        }
        socket.shutdown().await.unwrap();
        String::from_utf8_lossy(&captured).into_owned()
    });
    (format!("http://{addr}/events"), worker)
}

#[tokio::test]
async fn transport_sends_opaque_bytes_and_yields_raw_events_across_fragments() {
    let head = b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n";
    let body = "event: provider\ndata: {\"opaque\":true}\n\ndata: [DONE]\n\n".as_bytes();
    let mut fragments = vec![head.to_vec()];
    fragments.extend(body.chunks(3).map(<[u8]>::to_vec));
    let (url, captured) = server(fragments).await;
    let request = HttpSseRequest::post(url, br#"{"request":"opaque"}"#.to_vec())
        .unwrap()
        .header("content-type", "application/json")
        .unwrap()
        .header("x-test", "yes")
        .unwrap();
    let transport = ReqwestHttpTransport::new().unwrap();

    let events = tokio::time::timeout(
        TIMEOUT,
        transport
            .sse(request, CancellationToken::new())
            .collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert_eq!(
        events,
        [
            Ok(SseEvent {
                event: "provider".to_owned(),
                data: "{\"opaque\":true}".to_owned(),
                id: None,
                retry_ms: None,
            }),
            Ok(SseEvent {
                event: "message".to_owned(),
                data: "[DONE]".to_owned(),
                id: None,
                retry_ms: None,
            }),
        ]
    );
    let request = captured.await.unwrap().to_lowercase();
    assert!(request.starts_with("post /events "));
    assert!(request.contains("x-test: yes"));
    assert!(request.contains("{\"request\":\"opaque\"}"));
}

#[tokio::test]
async fn pre_cancelled_request_never_becomes_a_network_error() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let request = HttpSseRequest::get("http://127.0.0.1:9/events").unwrap();
    let transport = ReqwestHttpTransport::new().unwrap();
    let items = transport
        .sse(request, cancellation)
        .collect::<Vec<_>>()
        .await;
    assert_eq!(items, [Err(TransportError::Cancelled)]);
}

#[tokio::test]
async fn non_success_status_is_one_bounded_http_error() {
    let body = "x".repeat(3_000);
    let response = format!(
        "HTTP/1.1 429 Too Many Requests\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let (url, worker) = server(vec![response.into_bytes()]).await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let items = transport
        .sse(HttpSseRequest::get(url).unwrap(), CancellationToken::new())
        .collect::<Vec<_>>()
        .await;
    worker.await.unwrap();

    assert_eq!(items.len(), 1);
    assert!(matches!(
        &items[0],
        Err(TransportError::Http { status: 429, body, .. })
            if body.as_str().chars().count() == 2_048
    ));
}

#[tokio::test]
async fn non_success_metadata_is_semantic_and_body_is_explicitly_redacted() {
    for (header, expected) in [
        (
            "Retry-After: 7",
            HttpRetryAfter::Delay(Duration::from_secs(7)),
        ),
        (
            "Retry-After: Wed, 21 Oct 2015 07:28:00 GMT",
            HttpRetryAfter::At(std::time::UNIX_EPOCH + Duration::from_secs(1_445_412_480)),
        ),
    ] {
        let response = format!(
            "HTTP/1.1 429 Too Many Requests\r\n{header}\r\nX-Should-Retry: true\r\nContent-Length: 24\r\nConnection: close\r\n\r\nsecret-provider-message!"
        );
        let (url, worker) = server(vec![response.into_bytes()]).await;
        let transport = ReqwestHttpTransport::new().unwrap();
        let items = transport
            .sse(HttpSseRequest::get(url).unwrap(), CancellationToken::new())
            .collect::<Vec<_>>()
            .await;
        worker.await.unwrap();
        let error = items.into_iter().next().unwrap().unwrap_err();
        assert!(!format!("{error:?} {error}").contains("secret-provider-message"));
        match error {
            TransportError::Http {
                status,
                body,
                metadata,
            } => {
                assert_eq!(status, 429);
                assert_eq!(body.as_str(), "secret-provider-message!");
                assert_eq!(metadata.retry_after(), Some(expected));
                assert_eq!(metadata.should_retry(), Some(true));
            }
            other => panic!("expected HTTP error, got {other:?}"),
        }
    }
}

#[tokio::test]
async fn malformed_retry_headers_are_ignored_and_never_retained_as_text() {
    let response = "HTTP/1.1 503 Service Unavailable\r\nRetry-After: provider-secret-canary\r\nX-Should-Retry: perhaps\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}";
    let (url, worker) = server(vec![response.as_bytes().to_vec()]).await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let error = transport
        .sse(HttpSseRequest::get(url).unwrap(), CancellationToken::new())
        .collect::<Vec<_>>()
        .await
        .remove(0)
        .unwrap_err();
    worker.await.unwrap();
    match &error {
        TransportError::Http { metadata, .. } => {
            assert_eq!(metadata.retry_after(), None);
            assert_eq!(metadata.should_retry(), None);
        }
        other => panic!("expected HTTP error, got {other:?}"),
    }
    assert!(!format!("{error:?} {error}").contains("provider-secret-canary"));
}

#[tokio::test]
async fn successful_non_sse_content_type_is_rejected() {
    let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}".to_vec();
    let (url, worker) = server(vec![response]).await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let items = transport
        .sse(HttpSseRequest::get(url).unwrap(), CancellationToken::new())
        .collect::<Vec<_>>()
        .await;
    worker.await.unwrap();
    assert!(matches!(
        &items[..],
        [Err(TransportError::InvalidSse { message })]
            if message.contains("content-type")
    ));
}

#[tokio::test]
async fn cancellation_after_an_event_terminates_the_active_body_read() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: first\n\n",
            )
            .await
            .unwrap();
        futures::future::pending::<()>().await;
    });
    let cancellation = CancellationToken::new();
    let transport = ReqwestHttpTransport::new().unwrap();
    let mut stream = transport.sse(
        HttpSseRequest::get(format!("http://{addr}/events")).unwrap(),
        cancellation.clone(),
    );
    assert!(matches!(
        tokio::time::timeout(TIMEOUT, stream.next()).await.unwrap(),
        Some(Ok(SseEvent { data, .. })) if data == "first"
    ));
    cancellation.cancel();
    assert_eq!(
        tokio::time::timeout(TIMEOUT, stream.next()).await.unwrap(),
        Some(Err(TransportError::Cancelled))
    );
    worker.abort();
}

#[tokio::test]
async fn reqwest_deadline_is_a_typed_timeout_not_a_generic_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        futures::future::pending::<()>().await;
    });
    let transport = ReqwestHttpTransport::with_timeout(Duration::from_millis(20)).unwrap();
    let items = transport
        .sse(
            HttpSseRequest::get(format!("http://{addr}/events")).unwrap(),
            CancellationToken::new(),
        )
        .collect::<Vec<_>>()
        .await;
    assert_eq!(items, [Err(TransportError::Timeout)]);
    worker.abort();
}

#[tokio::test]
async fn non_success_body_read_has_an_independent_deadline() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(
                b"HTTP/1.1 503 Service Unavailable\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        futures::future::pending::<()>().await;
    });
    let transport = ReqwestHttpTransport::new().unwrap();
    let items = tokio::time::timeout(
        Duration::from_secs(2),
        transport
            .sse(
                HttpSseRequest::get(format!("http://{addr}/events")).unwrap(),
                CancellationToken::new(),
            )
            .collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert!(matches!(
        &items[..],
        [Err(TransportError::Http { status: 503, .. })]
    ));
    worker.abort();
}

#[tokio::test]
async fn non_success_body_read_has_a_total_byte_budget() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        socket
            .write_all(
                b"HTTP/1.1 429 Too Many Requests\r\nTransfer-Encoding: chunked\r\nConnection: keep-alive\r\n\r\n",
            )
            .await
            .unwrap();
        let body = vec![b'x'; 8 * 1024];
        let head = format!("{:X}\r\n", body.len());
        for _ in 0..300 {
            if socket.write_all(head.as_bytes()).await.is_err()
                || socket.write_all(&body).await.is_err()
                || socket.write_all(b"\r\n").await.is_err()
            {
                return;
            }
        }
        futures::future::pending::<()>().await;
    });
    let transport = ReqwestHttpTransport::new().unwrap();
    let items = tokio::time::timeout(
        Duration::from_secs(2),
        transport
            .sse(
                HttpSseRequest::get(format!("http://{addr}/events")).unwrap(),
                CancellationToken::new(),
            )
            .collect::<Vec<_>>(),
    )
    .await
    .unwrap();
    assert!(matches!(
        &items[..],
        [Err(TransportError::Http { status: 429, body, .. })]
            if body.as_str().len() <= 16 * 1024
    ));
    worker.abort();
}

#[tokio::test]
async fn reqwest_network_errors_never_retain_the_request_url_or_query() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let canary = "private-query-canary";
    let transport = ReqwestHttpTransport::new().unwrap();
    let error = transport
        .sse(
            HttpSseRequest::get(format!("http://{addr}/events?token={canary}")).unwrap(),
            CancellationToken::new(),
        )
        .collect::<Vec<_>>()
        .await
        .remove(0)
        .unwrap_err();
    assert!(matches!(error, TransportError::Network { .. }));
    let rendered = format!("{error:?} {error}");
    assert!(!rendered.contains(canary), "{rendered}");
    assert!(!rendered.contains("?token="), "{rendered}");
}

#[tokio::test]
async fn an_sse_exchange_observes_response_headers_including_on_a_rejected_status() {
    // A 429's rate-limit headers are exactly the ones a caller most needs, so
    // headers publish for every response that arrived — not only successes.
    for (status, body) in [
        (
            200_u16,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nAnthropic-RateLimit-Unified-Reset: 30\r\nConnection: close\r\n\r\ndata: hi\n\n",
        ),
        (
            429,
            "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 12\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
        ),
    ] {
        let (url, worker) = server(vec![body.as_bytes().to_vec()]).await;
        let transport = ReqwestHttpTransport::new().unwrap();
        let exchange =
            transport.sse_exchange(HttpSseRequest::get(url).unwrap(), CancellationToken::new());
        // The slot is empty until the response actually arrives — unknown, not
        // "no headers".
        assert!(exchange.headers.get().is_none());

        let mut events = exchange.events;
        while let Some(item) = events.next().await {
            if item.is_err() {
                break;
            }
        }
        worker.await.unwrap();

        let headers = exchange
            .headers
            .get()
            .unwrap_or_else(|| panic!("status {status} must publish its headers"));
        assert!(headers.contains_key("content-type"));
        if status == 200 {
            assert_eq!(
                exchange.headers.header("anthropic-ratelimit-unified-reset"),
                Some("30"),
                "declared header names are case-insensitive"
            );
        } else {
            assert_eq!(exchange.headers.header("Retry-After"), Some("12"));
        }
    }
}

#[tokio::test]
async fn a_transport_that_cannot_report_headers_says_unknown_not_empty() {
    struct EventsOnly;
    impl HttpTransport for EventsOnly {
        fn sse(
            &self,
            _request: HttpSseRequest,
            _cancellation: CancellationToken,
        ) -> SseEventStream {
            Box::pin(futures::stream::empty())
        }
    }
    // The default `sse_exchange` must not fabricate an empty header map, which
    // would read as "the response had no headers".
    let exchange = EventsOnly.sse_exchange(
        HttpSseRequest::get("https://example.test/stream").unwrap(),
        CancellationToken::new(),
    );
    assert!(exchange.headers.get().is_none());
    assert!(exchange.headers.header("retry-after").is_none());
}
