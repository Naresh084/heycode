//! Project config layers over home config; unknown keys warn; type errors name the key.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{CONFIG_SCHEMA_VERSION, Config, ConfigPaths, ConfigSource, ConfigWarning};

const PROFILE: &[&str] = &["session", "prompt", "tools", "llm"];

fn write(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
    let path = dir.join(name);
    std::fs::write(
        &path,
        body.replace(
            "schema_version = 1",
            &format!("schema_version = {CONFIG_SCHEMA_VERSION}"),
        ),
    )
    .unwrap();
    path
}

#[test]
fn a_project_file_layers_key_by_key_over_the_home_file() {
    let dir = tempfile::tempdir().unwrap();
    let home = write(
        dir.path(),
        "config.toml",
        "schema_version = 1\n[llm]\nprovider = \"openai\"\nmodel = \"home-model\"\n[approval]\nmode = \"deny\"\n[tools]\nbash_timeout_ms = 1234\n",
    );
    let project = write(
        dir.path(),
        "heycode.toml",
        "[llm]\nmodel = \"project-model\"\n",
    );

    let loaded = Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project.clone()),
            home: Some(home.clone()),
        },
        PROFILE,
    )
    .unwrap();

    assert_eq!(
        loaded.config.llm.model, "project-model",
        "project wins the key it sets"
    );
    assert_eq!(
        loaded.config.llm.provider, "openai",
        "sibling keys survive from home"
    );
    assert_eq!(
        loaded.config.approval.mode,
        Some(heycode_config::ApprovalMode::Deny),
        "sections the project file does not mention are kept from home"
    );
    assert_eq!(loaded.config.tools.bash_timeout_ms, 1234);
    assert_eq!(loaded.source, ConfigSource::Layered { home, project });
    assert!(loaded.warnings.is_empty(), "{:?}", loaded.warnings);
    assert!(
        loaded.migration.is_none(),
        "a partial project overlay without schema_version is not a pending migration"
    );
}

#[test]
fn project_only_and_home_only_keep_their_single_source_identity() {
    let dir = tempfile::tempdir().unwrap();
    let project = write(
        dir.path(),
        "heycode.toml",
        "[llm]\nmodel = \"project-model\"\n",
    );
    let loaded = Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project.clone()),
            home: None,
        },
        PROFILE,
    )
    .unwrap();
    assert_eq!(loaded.source, ConfigSource::Project(project));
    assert_eq!(loaded.config.llm.model, "project-model");
    assert!(loaded.migration.is_none());

    let home = write(
        dir.path(),
        "config.toml",
        "schema_version = 1\n[llm]\nmodel = \"h\"\n",
    );
    let loaded = Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: None,
            home: Some(home.clone()),
        },
        PROFILE,
    )
    .unwrap();
    assert_eq!(loaded.source, ConfigSource::Home(home));

    let loaded = Config::load_paths(ConfigPaths::default(), PROFILE).unwrap();
    assert_eq!(loaded.source, ConfigSource::BuiltIn);
}

#[test]
fn an_explicit_file_is_the_whole_authority_and_ignores_both_others() {
    let dir = tempfile::tempdir().unwrap();
    let explicit = write(
        dir.path(),
        "x.toml",
        "schema_version = 1\n[llm]\nmodel = \"x\"\n",
    );
    let project = write(dir.path(), "heycode.toml", "[llm]\nmodel = \"p\"\n");
    let home = write(
        dir.path(),
        "config.toml",
        "schema_version = 1\n[llm]\nmodel = \"h\"\n",
    );
    let loaded = Config::load_paths(
        ConfigPaths {
            explicit: Some(explicit.clone()),
            project: Some(project),
            home: Some(home),
        },
        PROFILE,
    )
    .unwrap();
    assert_eq!(loaded.source, ConfigSource::Explicit(explicit));
    assert_eq!(loaded.config.llm.model, "x");
}

#[test]
fn unknown_keys_are_warnings_that_name_the_file_and_the_key() {
    let dir = tempfile::tempdir().unwrap();
    let home = write(
        dir.path(),
        "config.toml",
        "schema_version = 1\n[llm]\nmodel = \"h\"\nmodle = \"typo\"\n",
    );
    let project = write(dir.path(), "heycode.toml", "[approvals]\nmode = \"auto\"\n");
    let loaded = Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project.clone()),
            home: Some(home.clone()),
        },
        PROFILE,
    )
    .unwrap();
    assert_eq!(
        loaded.warnings,
        vec![
            ConfigWarning::UnknownKey {
                path: home.clone(),
                key: "llm.modle".to_owned(),
            },
            ConfigWarning::UnknownKey {
                path: project.clone(),
                key: "approvals".to_owned(),
            },
        ]
    );
    let rendered = loaded.warnings[1].to_string();
    assert!(rendered.contains("heycode.toml"), "{rendered}");
    assert!(rendered.contains("`approvals`"), "{rendered}");
    assert!(rendered.contains("ignored"), "{rendered}");
    assert_eq!(
        loaded.config.llm.model, "h",
        "unknown keys never abort startup"
    );
}

#[test]
fn a_type_error_names_the_file_and_the_dotted_key() {
    let dir = tempfile::tempdir().unwrap();
    let project = write(
        dir.path(),
        "heycode.toml",
        "[tools]\nbash_timeout_ms = \"soon\"\n",
    );
    let error = match Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project),
            home: None,
        },
        PROFILE,
    ) {
        Ok(_) => panic!("a string where an integer is required must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("heycode.toml"), "{error}");
    assert!(error.contains("tools.bash_timeout_ms"), "{error}");
    assert!(error.contains("line 2"), "{error}");
}

#[test]
fn a_type_error_in_the_home_layer_is_blamed_on_the_home_file_not_the_overlay() {
    let dir = tempfile::tempdir().unwrap();
    let home = write(
        dir.path(),
        "config.toml",
        "schema_version = 1\n\n[llm]\nmodel = \"h\"\n\n[tools]\nread_max_lines = \"many\"\n",
    );
    let project = write(dir.path(), "heycode.toml", "[llm]\nmodel = \"p\"\n");
    let error = match Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project),
            home: Some(home),
        },
        PROFILE,
    ) {
        Ok(_) => panic!("must fail"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("config.toml"), "{error}");
    assert!(!error.contains("heycode.toml"), "{error}");
    assert!(error.contains("tools.read_max_lines"), "{error}");
    assert!(error.contains("line 7"), "{error}");
}

#[test]
fn the_effective_report_names_the_source_of_every_value_and_hides_env_values() {
    use heycode_config::ConfigValueSource;

    let dir = tempfile::tempdir().unwrap();
    let home = write(
        dir.path(),
        "config.toml",
        "schema_version = 1\n[llm]\nprovider = \"openai\"\nmodel = \"home-model\"\n[mcp.servers.fs]\ncommand = \"fs-server\"\nenv = { TOKEN = \"hunter2\" }\n",
    );
    let project = write(
        dir.path(),
        "heycode.toml",
        "[llm]\nmodel = \"project-model\"\n",
    );
    let mut loaded = Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project.clone()),
            home: Some(home.clone()),
        },
        PROFILE,
    )
    .unwrap();
    loaded.config.apply_patch("llm.model=flag-model").unwrap();
    loaded
        .config
        .apply_patch("tools.bash_timeout_ms=99")
        .unwrap();

    let report = loaded.config.report();
    let row = |key: &str| {
        report
            .rows()
            .iter()
            .find(|row| row.key == key)
            .unwrap_or_else(|| panic!("no row for {key}"))
            .clone()
    };
    assert_eq!(
        row("llm.model").value,
        "\"flag-model\"",
        "values are TOML-rendered"
    );
    assert_eq!(row("llm.model").source, ConfigValueSource::Flag);
    assert_eq!(
        row("llm.provider").source,
        ConfigValueSource::File(home.clone())
    );
    assert_eq!(row("tools.bash_timeout_ms").source, ConfigValueSource::Flag);
    assert_eq!(
        row("tools.read_max_lines").source,
        ConfigValueSource::Default
    );
    assert_eq!(
        row("mcp.servers.fs.command").source,
        ConfigValueSource::File(home.clone())
    );
    assert_eq!(
        row("mcp.servers.fs.env.TOKEN").value,
        "•••",
        "env values may be secrets"
    );
    assert!(!report.render().contains("hunter2"), "{}", report.render());
    let rendered = report.render();
    assert!(
        rendered.contains("llm.model = \"flag-model\"  (command line)"),
        "{rendered}"
    );
    assert!(rendered.contains("tools.read_max_lines = "), "{rendered}");
    assert!(rendered.contains("(default)"), "{rendered}");
    assert!(
        rendered.contains("compaction.threshold_ratio = 0.8  (default)"),
        "an f32 setting renders as the user wrote it: {rendered}"
    );
    assert!(
        rendered.contains(&format!("({})", home.display())),
        "{rendered}"
    );
    assert!(
        rendered.lines().next().unwrap().starts_with("config"),
        "{rendered}"
    );
    assert!(
        rendered.contains(&format!(
            "layers: defaults → {} → {} → command line",
            home.display(),
            project.display()
        )),
        "{rendered}"
    );

    // A value the project overlay set and no flag touched belongs to the project.
    let mut plain = Config::load_paths(
        ConfigPaths {
            explicit: None,
            project: Some(project.clone()),
            home: Some(home),
        },
        PROFILE,
    )
    .unwrap();
    plain.config.apply_patch("ui.accent=cyan").unwrap();
    let report = plain.config.report();
    let model = report
        .rows()
        .iter()
        .find(|row| row.key == "llm.model")
        .unwrap();
    assert_eq!(model.source, ConfigValueSource::File(project));
}
