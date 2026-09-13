//! An isolated `HEYCODE_HOME` owns its own credential stores, `heycode setup` can
//! undo what it wrote, and a pasted key is checked before it is kept.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_authorization_api_key::ApiKeyValidationFailure;
use heycode_cli::{
    NewKeyCheck, check_new_key, credential_source_at, credential_source_label,
    delete_credential_at, lookup_credential_at, write_credential_at,
};
use heycode_config::Config;

#[test]
fn credential_homes_are_isolated_and_lexical_aliases_share_the_same_file() {
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let reference = "HEYCODE_STORAGE_ISOLATION_7FA";
    assert_eq!(
        write_credential_at(reference, "first-secret", &first).unwrap(),
        "file"
    );
    assert_eq!(lookup_credential_at(reference, &second).unwrap(), None);
    assert_eq!(
        write_credential_at(reference, "second-secret", &second).unwrap(),
        "file"
    );
    assert_eq!(
        lookup_credential_at(reference, &first).unwrap().as_deref(),
        Some("first-secret")
    );
    assert_eq!(
        lookup_credential_at(reference, &first.join("."))
            .unwrap()
            .as_deref(),
        Some("first-secret")
    );
    assert!(first.join("credentials.toml").is_file());
    assert!(second.join("credentials.toml").is_file());
}

#[test]
fn a_credential_written_this_run_can_be_removed_again_and_its_source_is_named() {
    let root = tempfile::tempdir().unwrap();
    let mut cfg = Config::defaults();
    cfg.apply_patch("llm.provider=deepseek").unwrap();
    cfg.apply_patch("llm.api_key_env=HEYCODE_TEST_ISOLATION_KEY")
        .unwrap();
    assert_eq!(credential_source_at(&cfg, root.path()).unwrap(), None);
    assert_eq!(credential_source_label(None), "unknown");

    let provider =
        write_credential_at("HEYCODE_TEST_ISOLATION_KEY", "secret-value", root.path()).unwrap();
    assert_eq!(provider, "file");
    assert_eq!(
        lookup_credential_at("HEYCODE_TEST_ISOLATION_KEY", root.path()).unwrap(),
        Some("secret-value".to_owned())
    );
    let source = credential_source_at(&cfg, root.path()).unwrap();
    assert_eq!(credential_source_label(source), "credentials file");

    let removed = delete_credential_at("HEYCODE_TEST_ISOLATION_KEY", root.path()).unwrap();
    assert_eq!(removed, Some(provider));
    assert_eq!(
        lookup_credential_at("HEYCODE_TEST_ISOLATION_KEY", root.path()).unwrap(),
        None,
        "rollback leaves nothing behind"
    );
    assert_eq!(
        delete_credential_at("HEYCODE_TEST_ISOLATION_KEY", root.path()).unwrap(),
        None,
        "deleting twice is idempotent"
    );
}

async fn one_response(status: u16, body: &'static str) -> String {
    // One connection per call: every validator here makes exactly one request.
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0_u8; 4096];
        let _ = socket.read(&mut request).await.unwrap();
        let response = format!(
            "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        socket.write_all(response.as_bytes()).await.unwrap();
    });
    format!("http://{address}")
}

#[tokio::test]
async fn a_pasted_key_is_checked_against_the_provider_before_it_is_stored() {
    let rejected = one_response(401, "{}").await;
    assert_eq!(
        check_new_key("deepseek", Some(&rejected), "typo").await,
        Err(ApiKeyValidationFailure::Unauthorized)
    );
    let accepted = one_response(200, r#"{"data":{"label":"fixture"}}"#).await;
    assert_eq!(
        check_new_key("openrouter", Some(&accepted), "real").await,
        Ok(NewKeyCheck::Accepted)
    );
    // Every credential provider heycode can route to is checkable: OpenAI and
    // Anthropic through their own validators, Gemini through `x-goog-api-key`.
    for provider in ["openai", "anthropic"] {
        let rejected = one_response(401, "{}").await;
        assert_eq!(
            check_new_key(provider, Some(&rejected), "typo").await,
            Err(ApiKeyValidationFailure::Unauthorized),
            "{provider} rejects a bad key before it is stored"
        );
    }
    let rejected = one_response(400, r#"{"error":{"status":"INVALID_ARGUMENT"}}"#).await;
    assert_eq!(
        check_new_key("google", Some(&rejected), "typo").await,
        Err(ApiKeyValidationFailure::Unauthorized),
        "Gemini answers a bad key with 400 as often as 401"
    );
    let accepted = one_response(200, r#"{"models":[]}"#).await;
    assert_eq!(
        check_new_key("google", Some(&accepted), "real").await,
        Ok(NewKeyCheck::Accepted)
    );
    assert_eq!(
        check_new_key("ollama", None, "anything").await,
        Ok(NewKeyCheck::NotCheckable),
        "a provider that needs no key is stored unverified, honestly"
    );
    // Nothing listens here: the check cannot decide, and says so.
    assert_eq!(
        check_new_key("deepseek", Some("http://127.0.0.1:9"), "k").await,
        Err(ApiKeyValidationFailure::Network)
    );
}

#[test]
fn shipping_dependencies_cannot_initialize_an_os_credential_store() {
    let lock = include_str!("../../../../Cargo.lock");
    for package in [
        "keyring",
        "heycode-credentials-keychain",
        "apple-native-keyring-store",
        "windows-native-keyring-store",
        "dbus-secret-service-keyring-store",
    ] {
        assert!(
            !lock.contains(&format!("name = \"{package}\"")),
            "forbidden native credential dependency: {package}"
        );
    }
}

#[test]
fn default_home_credentials_child() {
    if std::env::var_os("HEYCODE_TEST_DEFAULT_FILE_HOME").is_none() {
        return;
    }
    let home = heycode_cli::heycode_home().unwrap();
    assert_eq!(
        home,
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap()).join(".heycode")
    );
    let reference = "HEYCODE_DEFAULT_FILE_TEST_91A";
    assert_eq!(heycode_cli::lookup_credential(reference).unwrap(), None);
    assert_eq!(
        heycode_cli::write_credential(reference, "private-test-key").unwrap(),
        "file"
    );
    assert_eq!(
        heycode_cli::lookup_credential(reference)
            .unwrap()
            .as_deref(),
        Some("private-test-key")
    );
    assert_eq!(
        delete_credential_at(reference, &home).unwrap().as_deref(),
        Some("file")
    );
    assert_eq!(heycode_cli::lookup_credential(reference).unwrap(), None);
    let _setup = heycode_cli::compose_setup_world(heycode_cli::SetupWorldOptions {
        settings_user_path: home.join("settings.toml"),
        credentials_root: home.clone(),
        catalog_cache_path: home.join("cache/models.json"),
    })
    .unwrap();
    assert!(home.join("credentials.toml").is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&home).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(home.join("credentials.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn default_home_uses_files_without_a_heycode_home_override() {
    let root = tempfile::tempdir().unwrap();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "it::credentials_isolation::default_home_credentials_child",
            "--nocapture",
        ])
        .env("HOME", root.path())
        .env("USERPROFILE", root.path())
        .env("HEYCODE_TEST_DEFAULT_FILE_HOME", "1")
        .env_remove("HEYCODE_HOME")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "default-home credential child failed");
            break;
        }
        if std::time::Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("default-home credentials blocked instead of using the home file");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(root.path().join(".heycode/credentials.toml").is_file());
}

#[test]
fn composed_file_credentials_are_writable_and_effect_owned() {
    use heycode_credentials::{
        CredentialKind, CredentialQuery, CredentialReference, CredentialSecret, CredentialSource,
        CredentialsService, SERVICE_CREDENTIALS,
    };
    let harness = heycode_cli::testing::RealCompositionHarness::new().unwrap();
    let world = harness.compose().unwrap();
    let service = world
        .context()
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    assert_eq!(service.provider_count().unwrap(), 3);
    let query = CredentialQuery::new(
        CredentialReference::new("HEYCODE_COMPOSED_FILE_TEST_74B").unwrap(),
        CredentialKind::new("api-key").unwrap(),
    );
    assert!(!service.describe(&query).unwrap().configured);
    assert!(service.describe(&query).unwrap().writable);
    assert_eq!(
        service
            .write(&query, &CredentialSecret::new("file-test-secret"))
            .unwrap()
            .as_str(),
        "file"
    );
    assert_eq!(
        service.describe(&query).unwrap().source,
        Some(CredentialSource::File)
    );
    assert_eq!(
        service.resolve(&query).unwrap().unwrap().expose(),
        "file-test-secret"
    );
    assert_eq!(service.delete(&query).unwrap().unwrap().as_str(), "file");
    assert!(!service.describe(&query).unwrap().configured);
    drop(world);
    assert_eq!(service.provider_count().unwrap(), 0);
}

#[test]
fn an_invalid_file_store_fails_without_falling_back_to_an_os_store() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("credentials.toml"), "not valid toml").unwrap();
    assert!(
        write_credential_at("HEYCODE_INVALID_FILE_TEST_48A", "never-saved", root.path()).is_err()
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("credentials.toml")).unwrap(),
        "not valid toml"
    );
}

#[test]
fn a_profile_cannot_reenable_the_retired_os_store() {
    let mut harness = heycode_cli::testing::RealCompositionHarness::new().unwrap();
    harness.config_mut().profile.plugins = vec!["credentials-keychain".to_owned()];
    let error = harness
        .compose()
        .err()
        .expect("the OS store has no shipping factory")
        .to_string();
    assert!(error.contains("credentials-keychain"), "{error}");
    assert!(error.contains("credentials-file"), "{error}");
}
