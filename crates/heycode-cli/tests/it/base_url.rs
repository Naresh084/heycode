//! `llm.base_url` is honoured by every request the configured provider makes:
//! the startup credential probe, the connect flow and the catalog. A key
//! issued for a proxy or gateway must never reach the official host.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use heycode_authorization_api_key::ApiKeyValidator as _;
use heycode_config::Config;
use heycode_credentials::CredentialSecret;

/// One-shot HTTP server that records the request line and authorization
/// header it received, then answers 200 with `body`.
async fn recording_server(body: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut request = vec![0_u8; 8192];
            let read = socket.read(&mut request).await.unwrap_or(0);
            let text = String::from_utf8_lossy(&request[..read]).to_string();
            sink.lock().unwrap().push(text);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
        }
    });
    (format!("http://{address}"), seen)
}

#[tokio::test]
async fn the_startup_probe_goes_to_the_configured_base_url_not_the_official_host() {
    let (base_url, seen) = recording_server(r#"{"data":[{"id":"gateway-model"}]}"#).await;
    let mut cfg = Config::defaults();
    cfg.apply_patch("llm.provider=deepseek").unwrap();
    cfg.apply_patch("llm.model=gateway-model").unwrap();
    cfg.apply_patch(&format!("llm.base_url={base_url}/"))
        .unwrap();

    let validator = heycode_cli::preflight_validator(&cfg).unwrap();
    assert_eq!(
        validator.endpoints(),
        (format!("{base_url}/models").as_str(), None),
        "the probe is built from llm.base_url"
    );
    validator
        .validate(
            &CredentialSecret::new("gateway-key"),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let requests = seen.lock().unwrap().clone();
    assert_eq!(requests.len(), 1, "{requests:?}");
    assert!(requests[0].starts_with("GET /models "), "{}", requests[0]);
    assert!(
        requests[0].contains("authorization: Bearer gateway-key")
            || requests[0].contains("Authorization: Bearer gateway-key"),
        "{}",
        requests[0]
    );

    cfg.apply_patch("llm.provider=openrouter").unwrap();
    let validator = heycode_cli::preflight_validator(&cfg).unwrap();
    assert_eq!(
        validator.endpoints(),
        (
            format!("{base_url}/key").as_str(),
            Some(format!("{base_url}/models").as_str())
        )
    );

    let official = Config::defaults();
    assert_eq!(
        heycode_cli::preflight_validator(&official)
            .unwrap()
            .endpoints()
            .0,
        "https://api.deepseek.com/models",
        "without base_url the official host is used"
    );
}

#[test]
fn every_credential_provider_composes_its_inference_route_against_the_base_url() {
    use heycode_cli::testing::RealCompositionHarness;
    for (provider, model) in [
        ("deepseek", "deepseek-v4-flash"),
        ("openrouter", "openai/gpt-5"),
        ("anthropic", "claude-sonnet-4-5"),
        ("openai", heycode_provider_openai::OPENAI_GPT_5_6_SOL),
    ] {
        let mut harness = RealCompositionHarness::new().unwrap();
        harness.config_mut().llm.provider = provider.to_owned();
        harness.config_mut().llm.model = model.to_owned();
        harness.config_mut().llm.api_key_env = Some("HEYCODE_TEST_GATEWAY_KEY".to_owned());
        harness.config_mut().llm.base_url = Some("http://127.0.0.1:9/gateway".to_owned());
        let credential_root = harness.credentials_root();
        std::fs::create_dir_all(&credential_root).unwrap();
        let credential_file = credential_root.join("credentials.toml");
        std::fs::write(
            &credential_file,
            "schema_version = 1\n[credentials]\nHEYCODE_TEST_GATEWAY_KEY = \"test-only-not-live\"\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&credential_root, std::fs::Permissions::from_mode(0o700))
                .unwrap();
            std::fs::set_permissions(&credential_file, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let world = harness
            .without_fake_provider()
            .compose()
            .unwrap_or_else(|error| panic!("{provider}: {error:#}"));
        let providers = world
            .context()
            .get::<heycode_llm::ProviderRegistry>(heycode_llm::SERVICE_PROVIDERS)
            .unwrap();
        assert!(
            providers.get(provider).is_some(),
            "{provider}: the production route composes against a gateway"
        );
        world.shutdown();
    }
}
