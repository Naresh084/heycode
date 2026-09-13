//! K08 real CLI graph and isolated-activation composition diagnostics.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[test]
fn json_doctor_reports_healthy_graph_and_isolated_activation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--fake",
            "doctor",
            "--composition",
            "--json",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["healthy"], true);
    assert!(
        report["graph"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let tools = report["graph"]["plugins"]
        .as_array()
        .unwrap()
        .iter()
        .find(|plugin| plugin["id"] == "tools")
        .unwrap();
    assert_eq!(tools["state"], "ready");
    assert!(
        tools["provides"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("tools"))
    );
    assert_eq!(report["activation"]["complete"], true);
    assert_eq!(report["activation"]["healthy"], true);
    assert!(
        report["activation"]["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|plugin| plugin["plugin"] == "tools" && plugin["state"] == "activated")
    );
    assert!(
        report["activation"]["suppressed"]
            .as_array()
            .unwrap()
            .contains(&serde_json::json!("persistent_product_state"))
    );
    assert!(!home.join("sessions").exists());
    assert!(!home.join("settings.toml").exists());
    assert!(!home.join("credentials.toml").exists());
}

#[test]
fn broken_named_profile_json_names_dependency_and_returns_nonzero() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        home.join("profiles/broken.toml"),
        "schema_version = 1\nname = \"broken\"\n\n[[plugins]]\nid = \"prompt\"\nenabled = false\n",
    )
    .unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--profile",
            "broken",
            "--fake",
            "doctor",
            "--composition",
            "--json",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["healthy"], false);
    assert_eq!(report["activation"], serde_json::Value::Null);
    assert!(
        report["graph"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| {
                row["code"] == "missing_dependency"
                    && row["plugin"] == "skills"
                    && row["related"]
                        .as_array()
                        .unwrap()
                        .contains(&serde_json::json!("prompt"))
            })
    );
    assert!(!home.join("sessions").exists());
}

#[test]
fn unified_json_doctor_runs_plugin_checks_without_secret_or_product_side_effects() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let canary = "sk-live-canary-never-emit";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args(["--restricted-workspace", "--fake", "doctor", "--json"])
        .env("HEYCODE_HOME", &home)
        .env("OPENROUTER_API_KEY", canary)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let raw = String::from_utf8(output.stdout).unwrap();
    assert!(!raw.contains(canary), "{raw}");
    let report: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["healthy"], true);
    assert_eq!(report["summary"]["failed"], 0);
    let checks = report["checks"].as_array().unwrap();
    for (id, code) in [
        ("settings", "settings.writable"),
        ("credentials", "credentials.providers-ready"),
        ("composition", "composition.healthy"),
    ] {
        assert!(checks.iter().any(|check| {
            check["id"] == id && check["status"] == "pass" && check["code"] == code
        }));
    }
    assert!(!home.join("sessions").exists());
    assert!(!home.join("settings.toml").exists());
    assert!(!home.join("credentials.toml").exists());
}

#[test]
fn unified_human_doctor_and_broken_profile_share_the_same_result_plane() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(
        home.join("profiles/broken.toml"),
        "schema_version = 1\nname = \"broken\"\n\n[[plugins]]\nid = \"prompt\"\nenabled = false\n",
    )
    .unwrap();
    let healthy = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args(["--restricted-workspace", "--fake", "doctor"])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(healthy.status.success());
    let human = String::from_utf8(healthy.stdout).unwrap();
    assert!(human.starts_with("doctor: healthy"), "{human}");
    assert!(human.contains("[pass] composition (composition.healthy)"));

    let broken = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--profile",
            "broken",
            "--fake",
            "doctor",
            "--json",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert_eq!(broken.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&broken.stdout).unwrap();
    let composition = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "composition")
        .unwrap();
    assert_eq!(composition["status"], "failure");
    assert_eq!(composition["code"], "composition.invalid");
    assert!(
        composition["evidence"]["report"]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["code"] == "missing_dependency")
    );
}

#[test]
fn pending_explicit_migration_is_typed_json_and_never_includes_raw_config_secret() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let workspace = dir.path().join("workspace");
    let config = dir.path().join("legacy.toml");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&workspace).unwrap();
    let canary = "migration-raw-secret-canary-never-emit";
    let raw = format!(
        "schema_version = 2\n\n[llm]\nprovider = \"openrouter\"\nmodel = \"custom/model\"\n\n[private_extension]\napi_key = \"{canary}\"\n"
    );
    std::fs::write(&config, &raw).unwrap();

    let output = std::process::Command::new(env!("CARGO_BIN_EXE_heycode"))
        .args([
            "--restricted-workspace",
            "--config",
            config.to_str().unwrap(),
            "--fake",
            "doctor",
            "--json",
        ])
        .env("HEYCODE_HOME", &home)
        .current_dir(&workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(!stdout.contains(canary), "{stdout}");
    assert!(!stderr.contains(canary), "{stderr}");
    assert_eq!(std::fs::read_to_string(&config).unwrap(), raw);
    let report: serde_json::Value = serde_json::from_str(&stdout).unwrap();
    let migration = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["id"] == "config-migration")
        .unwrap();
    assert_eq!(migration["status"], "warning");
    assert_eq!(migration["code"], "config.migration-pending");
    assert_eq!(
        migration["evidence"]["report"]["disposition"]["kind"],
        "user_owned_pending"
    );
    assert_eq!(
        migration["evidence"]["report"]["changes"][0]["kind"],
        "set_schema_version"
    );
    assert!(migration["evidence"]["report"].get("raw").is_none());
}
