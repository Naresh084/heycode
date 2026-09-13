//! Owner-only file behavior and legacy migration contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::compose;
use heycode_credentials::{
    CredentialKind, CredentialQuery, CredentialReference, CredentialSecret, CredentialSource,
    CredentialsService, SERVICE_CREDENTIALS, credentials_plugin,
};
use heycode_credentials_file::{FileCredentialConfig, file_credentials_plugin};
use heycode_settings::{SettingsDocuments, settings_plugin};

fn query(name: &str) -> CredentialQuery {
    CredentialQuery::new(
        CredentialReference::new(name).unwrap(),
        CredentialKind::new("api-key").unwrap(),
    )
}

#[test]
fn legacy_file_migrates_with_exact_backup_owner_modes_and_idempotence() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("heycode-home");
    std::fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let legacy = root.join("credentials");
    let legacy_raw = "# legacy\nOPENROUTER_API_KEY=sk-or-legacy\nDEEPSEEK_API_KEY=sk-deepseek\n";
    std::fs::write(&legacy, legacy_raw).unwrap();

    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        file_credentials_plugin(FileCredentialConfig::new(root.clone())),
    ];
    let mut context = compose(&plugins).unwrap();
    let service = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    let descriptor = service.describe(&query("OPENROUTER_API_KEY")).unwrap();
    assert!(descriptor.configured);
    assert_eq!(descriptor.source, Some(CredentialSource::File));
    assert!(descriptor.writable);
    assert_eq!(
        service
            .resolve(&query("OPENROUTER_API_KEY"))
            .unwrap()
            .unwrap()
            .expose(),
        "sk-or-legacy"
    );
    assert!(!legacy.exists());
    assert_eq!(
        std::fs::read_to_string(root.join("credentials.legacy.bak")).unwrap(),
        legacy_raw
    );
    let current = root.join("credentials.toml");
    let current_raw = std::fs::read_to_string(&current).unwrap();
    assert!(current_raw.contains("schema_version = 1"));
    assert!(current_raw.contains("OPENROUTER_API_KEY"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o077,
            0
        );
        assert_eq!(
            std::fs::metadata(&current).unwrap().permissions().mode() & 0o077,
            0
        );
        assert_eq!(
            std::fs::metadata(root.join("credentials.legacy.bak"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
    context.shutdown();

    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        file_credentials_plugin(FileCredentialConfig::new(root.clone())),
    ];
    let mut resumed = compose(&plugins).unwrap();
    assert_eq!(
        resumed
            .get::<CredentialsService>(SERVICE_CREDENTIALS)
            .unwrap()
            .resolve(&query("DEEPSEEK_API_KEY"))
            .unwrap()
            .unwrap()
            .expose(),
        "sk-deepseek"
    );
    resumed.shutdown();
}

#[test]
fn unconfigured_file_is_writable_and_write_resolve_delete_round_trips() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("home");
    let plugins = vec![
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        file_credentials_plugin(FileCredentialConfig::new(root.clone())),
    ];
    let mut context = compose(&plugins).unwrap();
    let service = context
        .get::<CredentialsService>(SERVICE_CREDENTIALS)
        .unwrap();
    let descriptor = service.describe(&query("MINIMAX_API_KEY")).unwrap();
    assert!(!descriptor.configured);
    assert!(descriptor.writable);
    assert_eq!(descriptor.provider.unwrap().as_str(), "file");
    service
        .write(
            &query("MINIMAX_API_KEY"),
            &CredentialSecret::new("minimax-secret"),
        )
        .unwrap();
    assert_eq!(
        service
            .resolve(&query("MINIMAX_API_KEY"))
            .unwrap()
            .unwrap()
            .expose(),
        "minimax-secret"
    );
    service.delete(&query("MINIMAX_API_KEY")).unwrap();
    assert!(
        !service
            .describe(&query("MINIMAX_API_KEY"))
            .unwrap()
            .configured
    );
    context.shutdown();
}

#[test]
fn conflicting_current_and_legacy_values_fail_without_deleting_legacy() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("home");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("credentials.toml"),
        "schema_version = 1\n[credentials]\nOPENROUTER_API_KEY = \"new\"\n",
    )
    .unwrap();
    std::fs::write(
        root.join("credentials"),
        "OPENROUTER_API_KEY=legacy-different\n",
    )
    .unwrap();
    let error = match compose(&[
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        file_credentials_plugin(FileCredentialConfig::new(root.clone())),
    ]) {
        Ok(_) => panic!("conflicting migration must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("conflicting values"), "{error}");
    assert!(root.join("credentials").exists());
    assert!(!root.join("credentials.legacy.bak").exists());
}

#[cfg(unix)]
#[test]
fn symbolic_link_current_store_fails_at_plugin_load() {
    use std::os::unix::fs::symlink;

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("home");
    std::fs::create_dir_all(&root).unwrap();
    let target = directory.path().join("target.toml");
    std::fs::write(
        &target,
        "schema_version = 1\n[credentials]\nOPENROUTER_API_KEY = \"untouched\"\n",
    )
    .unwrap();
    symlink(&target, root.join("credentials.toml")).unwrap();
    let error = match compose(&[
        settings_plugin(SettingsDocuments::new()),
        credentials_plugin(),
        file_credentials_plugin(FileCredentialConfig::new(root)),
    ]) {
        Ok(_) => panic!("symlink store must fail composition"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("symbolic link"), "{error}");
    assert!(
        std::fs::read_to_string(target)
            .unwrap()
            .contains("untouched")
    );
}
