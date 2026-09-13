//! Bounded buffered HTTP for provider catalog/auth discovery.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_http::{HttpRequest, HttpTransport, ReqwestHttpTransport, TransportError};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

async fn server(response: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await;
        socket.write_all(&response).await.unwrap();
        socket.shutdown().await.unwrap();
    });
    (format!("http://{addr}/models"), worker)
}

#[tokio::test]
async fn buffered_send_returns_success_and_error_status_bodies_to_caller() {
    for (status, body) in [
        (200_u16, r#"{"data":[{"id":"model"}]}"#),
        (401, r#"{"error":"unauthorized"}"#),
    ] {
        let response = format!(
            "HTTP/1.1 {status} Result\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let (url, worker) = server(response.into_bytes()).await;
        let transport = ReqwestHttpTransport::new().unwrap();
        let response = transport
            .send(
                HttpRequest::get(url)
                    .unwrap()
                    .header("accept", "application/json")
                    .unwrap()
                    .with_max_response_bytes(1024),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        worker.await.unwrap();
        assert_eq!(response.status, status);
        assert_eq!(response.body, body.as_bytes());
        assert_eq!(response.content_type.as_deref(), Some("application/json"));
    }
}

#[tokio::test]
async fn buffered_send_honors_cancellation_and_response_cap() {
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    let transport = ReqwestHttpTransport::new().unwrap();
    assert!(matches!(
        transport
            .send(
                HttpRequest::get("http://127.0.0.1:9/models").unwrap(),
                cancellation,
            )
            .await,
        Err(TransportError::Cancelled)
    ));

    let body = "x".repeat(32);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, worker) = server(response.into_bytes()).await;
    let error = transport
        .send(
            HttpRequest::get(url).unwrap().with_max_response_bytes(8),
            CancellationToken::new(),
        )
        .await
        .unwrap_err();
    worker.await.unwrap();
    assert!(matches!(
        error,
        TransportError::ResponseTooLarge { max_bytes: 8 }
    ));
}

/// Two connections: the first redirects to the second's origin.
async fn redirecting_pair() -> (String, String, tokio::task::JoinHandle<Vec<String>>) {
    let target = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let target_addr = target.local_addr().unwrap();
    let first = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let first_addr = first.local_addr().unwrap();
    let target_url = format!("http://{target_addr}/moved");
    let redirect = target_url.clone();
    let worker = tokio::spawn(async move {
        let mut seen = Vec::new();
        let (mut socket, _) = first.accept().await.unwrap();
        let mut buffer = [0_u8; 2048];
        let read = socket.read(&mut buffer).await.unwrap();
        seen.push(String::from_utf8_lossy(&buffer[..read]).into_owned());
        let body = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        socket.write_all(body.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        // Only reached if the transport followed the redirect.
        if let Ok(Ok((mut socket, _))) =
            tokio::time::timeout(std::time::Duration::from_millis(300), target.accept()).await
        {
            let read = socket.read(&mut buffer).await.unwrap_or(0);
            seen.push(String::from_utf8_lossy(&buffer[..read]).into_owned());
            let _ = socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        }
        seen
    });
    (format!("http://{first_addr}/start"), target_url, worker)
}

#[tokio::test]
async fn a_cross_origin_redirect_is_returned_rather_than_followed_with_our_headers() {
    let (url, _target, worker) = redirecting_pair().await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let response = transport
        .send(
            HttpRequest::get(url)
                .unwrap()
                // reqwest strips `Authorization` across origins but NOT custom
                // headers, so an automatic hop would leak this one.
                .header("mcp-session-id", "private-session-canary")
                .unwrap()
                .with_max_response_bytes(1024),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let seen = worker.await.unwrap();

    assert_eq!(response.status, 307, "the caller owns per-hop policy");
    assert_eq!(
        seen.len(),
        1,
        "the transport must not open a connection to the redirect target"
    );
    assert!(
        !seen
            .iter()
            .skip(1)
            .any(|request| request.contains("private-session-canary")),
        "protocol identity must never reach another origin"
    );
    // The redirect target is still reported so a caller can apply its own rules.
    assert!(response.header("location").is_some());
}

#[tokio::test]
async fn response_headers_are_lowercased_case_insensitive_and_bounded() {
    let oversized = "v".repeat(8 * 1024);
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nMCP-Session-Id: sess-42\r\nX-Huge: {oversized}\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
    );
    let (url, worker) = server(response.into_bytes()).await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let response = transport
        .send(
            HttpRequest::get(url).unwrap().with_max_response_bytes(1024),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    worker.await.unwrap();

    // Protocol identity that lives in a header is now observable.
    assert_eq!(response.header("Mcp-Session-Id"), Some("sess-42"));
    assert_eq!(response.header("mcp-session-id"), Some("sess-42"));
    assert!(response.headers.contains_key("content-type"));
    assert!(
        !response.headers.contains_key("x-huge"),
        "an oversized value is dropped, never truncated into something misleading"
    );
}

#[tokio::test]
async fn delete_reaches_the_wire_as_an_exact_bodyless_method() {
    let response =
        "HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_owned();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let worker = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buffer = [0_u8; 1024];
        let read = socket.read(&mut buffer).await.unwrap();
        socket.write_all(response.as_bytes()).await.unwrap();
        socket.shutdown().await.unwrap();
        String::from_utf8_lossy(&buffer[..read]).into_owned()
    });
    let transport = ReqwestHttpTransport::new().unwrap();
    let response = transport
        .send(
            HttpRequest::delete(format!("http://{addr}/session"))
                .unwrap()
                .header("mcp-session-id", "sess-42")
                .unwrap()
                .with_max_response_bytes(1024),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let request = worker.await.unwrap();
    assert_eq!(response.status, 204);
    assert!(request.starts_with("DELETE /session "), "exact method");
}

#[tokio::test]
async fn debug_renders_no_header_value_and_no_body_byte() {
    let body = r#"{"secret":"private-body-canary"}"#;
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nSet-Cookie: session=private-cookie-canary\r\nMcp-Session-Id: private-session-canary\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let (url, worker) = server(response.into_bytes()).await;
    let transport = ReqwestHttpTransport::new().unwrap();
    let response = transport
        .send(
            HttpRequest::get(url).unwrap().with_max_response_bytes(1024),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    worker.await.unwrap();

    // The values are still reachable by an intentional caller.
    assert_eq!(
        response.header("mcp-session-id"),
        Some("private-session-canary")
    );

    // A server controls both header values and the body, so neither may reach
    // a log or panic message through Debug.
    let rendered = format!("{response:?}");
    for canary in [
        "private-cookie-canary",
        "private-session-canary",
        "private-body-canary",
    ] {
        assert!(
            !rendered.contains(canary),
            "Debug leaked {canary}: {rendered}"
        );
    }
    // Names and sizes remain useful for diagnosis.
    assert!(rendered.contains("set-cookie"));
    assert!(rendered.contains("body_bytes"));
}
