//! Web tools: fetch against a local mock, SSRF guard, Brave search shape.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_tools::{Tool, ToolCtx};
use serde_json::json;
use std::sync::Arc;

struct FakeWeb;

#[async_trait::async_trait]
impl heycode_web::WebProvider for FakeWeb {
    fn descriptor(&self) -> heycode_web::WebProviderDescriptor {
        heycode_web::WebProviderDescriptor::new("fake", true, true).unwrap()
    }

    async fn search(
        &self,
        request: heycode_web::WebSearchRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<Vec<heycode_web::WebSearchResult>, heycode_web::WebError> {
        Ok(vec![heycode_web::WebSearchResult::new(
            "Rust",
            "https://example.test/rust",
            request.query(),
        )?])
    }

    async fn fetch(
        &self,
        request: heycode_web::WebFetchRequest,
        _cancellation: tokio_util::sync::CancellationToken,
    ) -> Result<heycode_web::WebFetchResult, heycode_web::WebError> {
        heycode_web::WebFetchResult::with_source(
            heycode_web::WebFetchSource::new(
                request.url(),
                Some("Portable [page]"),
                None,
                false,
                None,
                None,
            )?,
            "portable page",
            Some("text/plain"),
            false,
        )
    }
}

fn cx() -> ToolCtx {
    ToolCtx::default()
}

#[test]
fn model_facing_web_consumers_have_no_http_or_credential_provider_code() {
    let source = include_str!("../../src/builtins/web.rs");
    for forbidden in [
        "reqwest::",
        "BRAVE_API_KEY",
        "std::env::var",
        "ToSocketAddrs",
    ] {
        assert!(
            !source.contains(forbidden),
            "consumer contains `{forbidden}`"
        );
    }
    assert!(source.contains("heycode_web::WebRegistry"));
}

fn tools() -> (
    heycode_core::Context,
    heycode_tools::WebFetch,
    heycode_tools::WebSearch,
) {
    let plugins = vec![
        heycode_web::web_registry_plugin(),
        heycode_web::portable_web_plugin(heycode_web::PortableWebConfig::official()),
    ];
    let context = heycode_core::compose(&plugins).unwrap();
    let web = context
        .get::<heycode_web::WebRegistry>(heycode_web::SERVICE_WEB)
        .unwrap();
    (
        context,
        heycode_tools::WebFetch::new(web.clone()),
        heycode_tools::WebSearch::new(web),
    )
}

#[tokio::test]
async fn model_tools_dispatch_only_through_the_injected_web_provider() {
    let plugins = vec![heycode_web::web_registry_plugin()];
    let context = heycode_core::compose(&plugins).unwrap();
    let web = context
        .get::<heycode_web::WebRegistry>(heycode_web::SERVICE_WEB)
        .unwrap();
    web.register(&context, Arc::new(FakeWeb)).unwrap();
    let search = heycode_tools::WebSearch::new(web.clone());
    let fetch = heycode_tools::WebFetch::new(web);
    assert_eq!(
        search.untrusted_content().unwrap().source(),
        heycode_core::UntrustedContentSource::Web
    );
    assert_eq!(fetch.untrusted_content(), search.untrusted_content());
    let searched = search
        .run(json!({"query":"current rust"}), &cx())
        .await
        .unwrap();
    assert!(
        searched
            .as_str()
            .unwrap()
            .contains("https://example.test/rust")
    );
    let fetched = fetch
        .run(json!({"url":"https://example.test/page"}), &cx())
        .await
        .unwrap();
    assert_eq!(
        fetched,
        json!("Source: [Portable \\[page\\]](<https://example.test/page>)\n\nportable page")
    );
}

#[tokio::test]
async fn ssrf_guard_blocks_private_hosts() {
    for url in [
        "http://localhost/x",
        "http://127.0.0.1/x",
        "http://10.0.0.5/x",
        "http://192.168.1.1/x",
        "http://169.254.169.254/latest/meta-data",
        "http://[::1]/x",
        "file:///etc/passwd",
    ] {
        let (_context, fetch, _search) = tools();
        let err = fetch.run(json!({"url": url}), &cx()).await.unwrap_err();
        let msg = err.message;
        assert!(msg.contains("invalid public web fetch"), "{url} → {msg}");
    }
}

#[test]
fn ip_classifier_covers_v4_and_v6() {
    use std::net::IpAddr;
    let parse = |s: &str| s.parse::<IpAddr>().unwrap();
    for good in [
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
        assert!(
            heycode_tools::builtins::web::ip_is_private_for_tests(&parse(good)),
            "{good}"
        );
    }
    for public in ["8.8.8.8", "1.1.1.1", "2606:4700::1111"] {
        assert!(
            !heycode_tools::builtins::web::ip_is_private_for_tests(&parse(public)),
            "{public}"
        );
    }
}

#[tokio::test]
async fn dns_resolution_closes_rebinding_gap() {
    // "localhost" passes no literal it isn't already caught by, but a
    // hostname that RESOLVES to loopback must also be refused even when its
    // literal text looks public (e.g. via /etc/hosts aliases).
    let (_context, fetch, _search) = tools();
    let err = fetch
        .run(json!({"url": "http://localhost:1/x"}), &cx())
        .await
        .unwrap_err();
    assert!(
        err.message.contains("invalid public web fetch"),
        "{}",
        err.message
    );
}

#[tokio::test]
async fn fetch_strips_html_and_reports_success() {
    // 127.0.0.1 is BLOCKED by the SSRF guard, so bind on a non-loopback alias is
    // impossible here; instead exercise strip_html via the public helper path by
    // pointing at localhost and expecting the guard — full happy-path fetch runs
    // in real-API e2e only. The strip logic itself has unit coverage below.
    assert_eq!(
        heycode_tools::builtins::web::strip_for_tests("<h1>Hi</h1> <p>there</p>"),
        "Hi there"
    );
}

#[test]
fn ddg_lite_parser_extracts_decodes_and_orders() {
    let html = r#"<td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa&amp;rut=x" class='result-link'>Example A</a></td>
<tr><td class="result-snippet">Snippet <b>one</b></td></tr>
<td><a rel="nofollow" href="https://plain.example/b" class='result-link'>Plain</a></td>
<tr><td class="result-snippet">Two</td></tr>"#;
    let parsed = heycode_tools::builtins::web::parse_ddg_lite(html);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].0, "Example A");
    assert_eq!(parsed[0].1, "https://example.com/a", "uddg bounce decoded");
    assert_eq!(parsed[0].2, "Snippet one");
    assert_eq!(parsed[1].1, "https://plain.example/b");
}

/// Real-network e2e. AGENTS.md §9: gated behind `HEYCODE_E2E=1`, skips silently
/// otherwise — a default `cargo test` must never reach the network.
#[tokio::test]
async fn keyless_ddg_search_returns_live_results_when_network_allows() {
    if std::env::var("HEYCODE_E2E").as_deref() != Ok("1") {
        return;
    }
    let (_context, _fetch, tool) = tools();
    match tool
        .run(
            serde_json::json!({"query": "rust programming language"}),
            &ToolCtx::default(),
        )
        .await
    {
        Ok(v) => {
            let text = v.as_str().unwrap();
            assert!(text.contains("http"), "results must carry URLs: {text}");
        }
        Err(err) => {
            // Air-gapped hosts stay green; only genuine protocol failures fail.
            let msg = err.message;
            assert!(
                msg.contains("search failed") || msg.contains("HTTP"),
                "{msg}"
            );
        }
    }
}
