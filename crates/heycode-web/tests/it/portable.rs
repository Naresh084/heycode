//! N03 portable provider parsing, SSRF and lifecycle fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_web::{
    PortableWebConfig, SERVICE_WEB, WebFetchRequest, WebRegistry, ip_is_private_for_tests,
    parse_ddg_lite, portable_web_plugin, strip_html_for_tests, web_registry_plugin,
};
use tokio_util::sync::CancellationToken;

#[test]
fn ddg_lite_parser_extracts_decodes_deduplicates_and_orders() {
    let html = r#"<td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&amp;rut=x" class='result-link'>Example A</a></td>
<tr><td class="result-snippet">Snippet <b>one</b></td></tr>
<td><a rel="nofollow" href="https://plain.example/b" class='result-link'>Plain</a></td>
<tr><td class="result-snippet">Two</td></tr>"#;
    let parsed = parse_ddg_lite(html);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].0, "Example A");
    assert_eq!(parsed[0].1, "https://example.com/a");
    assert_eq!(parsed[0].2, "Snippet one");
    assert_eq!(parsed[1].1, "https://plain.example/b");
    assert_eq!(strip_html_for_tests("<h1>Hi</h1> <p>there</p>"), "Hi there");
}

#[test]
fn ip_classifier_covers_v4_and_v6() {
    let parse = |value: &str| value.parse::<std::net::IpAddr>().unwrap();
    for private in [
        "127.0.0.1",
        "10.1.2.3",
        "172.16.0.9",
        "192.168.0.1",
        "169.254.1.1",
        "100.64.0.1",
        "0.0.0.0",
        "::1",
        "::ffff:127.0.0.1",
        "fc00::1",
        "fe80::1",
    ] {
        assert!(ip_is_private_for_tests(&parse(private)), "{private}");
    }
    for public in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
        assert!(!ip_is_private_for_tests(&parse(public)), "{public}");
    }
}

#[test]
fn portable_plugin_registers_and_private_fetch_fails_before_network() {
    let plugins = vec![
        web_registry_plugin(),
        portable_web_plugin(PortableWebConfig::official()),
    ];
    let mut context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    assert_eq!(registry.descriptors().unwrap()[0].id(), "portable");
    let error = WebFetchRequest::new("http://127.0.0.1/private", 65_536).unwrap_err();
    assert_eq!(error.class(), heycode_web::WebErrorClass::InvalidRequest);
    assert!(
        context
            .plugin_inventory()
            .snapshot()
            .unwrap()
            .contributions
            .iter()
            .any(|row| row.plugin == "web-portable"
                && row.kind == heycode_core::ContributionKind::WebProvider
                && row.name == "portable")
    );
    context.shutdown();
}

#[tokio::test]
async fn portable_search_provider_normalizes_brave_or_keyless_wire_to_one_contract() {
    use std::io::{Read as _, Write as _};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 16 * 1024];
        let read = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..read]);
        let (content_type, body) = if request.contains("/res/v1/web/search") {
            (
                "application/json",
                r#"{"web":{"results":[{"title":"Rust","url":"https://example.test/rust","description":"Systems language"}]}}"#,
            )
        } else {
            (
                "text/html",
                r#"<td><a href="https://example.test/rust" class='result-link'>Rust</a></td><tr><td class="result-snippet">Systems language</td></tr>"#,
            )
        };
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    });
    let config = PortableWebConfig::new(
        format!("http://{address}"),
        format!("http://{address}/lite/"),
    )
    .unwrap();
    let plugins = vec![web_registry_plugin(), portable_web_plugin(config)];
    let context = heycode_core::compose(&plugins).unwrap();
    let registry = context.get::<WebRegistry>(SERVICE_WEB).unwrap();
    let results = registry
        .search(
            heycode_web::WebSearchRequest::new("rust", 8).unwrap(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].title(), "Rust");
    assert_eq!(results[0].url(), "https://example.test/rust");
    assert_eq!(results[0].snippet(), "Systems language");
    server.join().unwrap();
}
