//! MCP14: S13 metadata becomes exact definitions or unresolved references,
//! never credential values.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;
use std::path::Path;

use heycode_config::{
    CompetitorConfigKind, ImportAuthority, ImportReadiness, ImportScope, preview_competitor_config,
};
use heycode_mcp::{
    McpCompetitorImportError, McpDefinitionScope, McpImportAuthRole, McpImportAuthorityRequirement,
    McpServerId, McpTransportDefinition, McpTransportKind, preview_competitor_mcp_import,
};

fn codex(raw: &str, executable: bool) -> heycode_config::CompetitorImportPreview {
    preview_competitor_config(
        CompetitorConfigKind::Codex,
        ImportAuthority::user(executable),
        raw,
    )
    .unwrap()
}

#[test]
fn clean_preview_builds_exact_typed_definitions_without_copying_unrelated_values() {
    let source = codex(
        r#"
model = "openai/PROVIDER-METADATA-CANARY"
future_value = "sk-proj-UNKNOWN-VALUE-CANARY"

[mcp_servers.docs]
command = "npx"
args = ["-y", "@example/docs-mcp"]

[mcp_servers.remote]
url = "https://clean-mcp.example.test/v1"
"#,
        true,
    );
    let preview = preview_competitor_mcp_import(&source, Path::new("/workspace")).unwrap();

    assert_eq!(preview.source_kind(), CompetitorConfigKind::Codex);
    assert_eq!(preview.scope(), ImportScope::User);
    assert_eq!(preview.rows().len(), 2);
    let row = preview
        .rows()
        .iter()
        .find(|row| row.id().as_str() == "docs")
        .unwrap();
    assert_eq!(row.id().as_str(), "docs");
    assert!(row.enabled());
    assert_eq!(row.scope(), McpDefinitionScope::User);
    assert_eq!(row.transport_kind(), McpTransportKind::Stdio);
    assert_eq!(row.target(), "npx");
    assert_eq!(row.argument_count(), 2);
    assert_eq!(row.readiness(), ImportReadiness::Ready);
    assert!(row.unresolved_auth().is_empty());

    let definitions = preview.definitions(&BTreeSet::new()).unwrap();
    assert_eq!(definitions.len(), 2);
    let definition = definitions
        .iter()
        .find(|definition| definition.id().as_str() == "docs")
        .unwrap();
    assert_eq!(definition.id().as_str(), "docs");
    assert_eq!(definition.scope(), McpDefinitionScope::User);
    assert!(definition.enabled());
    assert!(!definition.reconnect().enabled());
    let McpTransportDefinition::Stdio(stdio) = definition.transport() else {
        panic!("expected stdio")
    };
    assert_eq!(stdio.command(), "npx");
    assert_eq!(stdio.cwd(), Path::new("/workspace"));
    assert_eq!(stdio.arguments().len(), 2);
    assert_eq!(stdio.arguments()[0].literal_value(), Some("-y"));
    assert_eq!(
        stdio.arguments()[1].literal_value(),
        Some("@example/docs-mcp")
    );
    assert!(stdio.environment().is_empty());
    let remote = definitions
        .iter()
        .find(|definition| definition.id().as_str() == "remote")
        .unwrap();
    let McpTransportDefinition::StreamableHttp(http) = remote.transport() else {
        panic!("expected Streamable HTTP")
    };
    assert_eq!(http.url(), "https://clean-mcp.example.test/v1");
    assert!(http.headers().is_empty());

    let debug = format!("{preview:?}");
    for forbidden in [
        "PROVIDER-METADATA-CANARY",
        "UNKNOWN-VALUE-CANARY",
        "@example/docs-mcp",
        "clean-mcp.example.test",
    ] {
        assert!(
            !debug.contains(forbidden),
            "import preview retained {forbidden}: {debug}"
        );
    }

    let unrelated_changed = codex(
        r#"
model = "different/UNRELATED-PROVIDER-VALUE"
future_value = "different unknown value"
[mcp_servers.docs]
command = "npx"
args = ["-y", "@example/docs-mcp"]
[mcp_servers.remote]
url = "https://clean-mcp.example.test/v1"
"#,
        true,
    );
    assert_eq!(
        preview,
        preview_competitor_mcp_import(&unrelated_changed, Path::new("/workspace")).unwrap(),
        "provider/settings/unknown values must not enter the MCP preview"
    );
}

#[test]
fn credential_fields_create_only_unresolved_references_and_enabled_rows_refuse() {
    let enabled = codex(
        r#"
[mcp_servers.secure]
url = "https://mcp.example.test/v1"
http_headers = { Authorization = "Bearer HTTP-SECRET-CANARY" }
oauth = { client_id = "OAUTH-ID-CANARY" }
"#,
        true,
    );
    let preview = preview_competitor_mcp_import(&enabled, Path::new("/workspace")).unwrap();
    let row = &preview.rows()[0];
    assert_eq!(row.readiness(), ImportReadiness::ExcludedMetadataRequired);
    assert_eq!(row.unresolved_auth().len(), 2);
    assert_eq!(
        row.unresolved_auth()
            .iter()
            .map(|reference| reference.role())
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([McpImportAuthRole::HttpHeaders, McpImportAuthRole::OAuth])
    );
    for (index, reference) in row.unresolved_auth().iter().enumerate() {
        assert!(reference.field_path().starts_with("mcp_servers.secure."));
        assert_eq!(
            reference.reference().as_str(),
            format!(
                "import/codex/secure/{}/{}",
                reference.role().as_str(),
                index + 1
            )
        );
    }
    assert_eq!(
        preview
            .definitions(&BTreeSet::new())
            .err()
            .expect("enabled incomplete row must fail"),
        McpCompetitorImportError::IncompleteEnabled(McpServerId::new("secure").unwrap())
    );
    let debug = format!("{preview:?}");
    assert!(!debug.contains("HTTP-SECRET-CANARY"));
    assert!(!debug.contains("OAUTH-ID-CANARY"));

    let different_secrets = codex(
        r#"
[mcp_servers.secure]
url = "https://mcp.example.test/v1"
http_headers = { Authorization = "Bearer DIFFERENT-HTTP-SECRET" }
oauth = { client_id = "DIFFERENT-OAUTH-ID" }
"#,
        true,
    );
    assert_eq!(
        preview,
        preview_competitor_mcp_import(&different_secrets, Path::new("/workspace")).unwrap(),
        "changing excluded credential values must not change the reference-only preview"
    );

    let disabled = codex(
        r#"
[mcp_servers.secure]
url = "https://mcp.example.test/v1"
enabled = false
http_headers = { Authorization = "Bearer DISABLED-SECRET-CANARY" }
"#,
        true,
    );
    let disabled = preview_competitor_mcp_import(&disabled, Path::new("/workspace")).unwrap();
    assert!(!disabled.rows()[0].enabled());
    assert_eq!(disabled.rows()[0].unresolved_auth().len(), 1);
    assert!(disabled.definitions(&BTreeSet::new()).unwrap().is_empty());
    assert!(!format!("{disabled:?}").contains("DISABLED-SECRET-CANARY"));

    let claude = preview_competitor_config(
        CompetitorConfigKind::ClaudeMcp,
        ImportAuthority::user(true),
        r#"{
          "mcpServers": {
            "local": {
              "command":"node",
              "enabled":false,
              "env":{"TOKEN":"CLAUDE-ENV-CANARY"}
            }
          }
        }"#,
    )
    .unwrap();
    let claude = preview_competitor_mcp_import(&claude, Path::new("/workspace")).unwrap();
    assert_eq!(
        claude.rows()[0].unresolved_auth()[0].role(),
        McpImportAuthRole::Environment
    );
    assert!(!format!("{claude:?}").contains("CLAUDE-ENV-CANARY"));
}

#[test]
fn s13_project_and_executable_authority_are_preserved_without_an_override_knob() {
    let raw = r#"{
      "mcp": {
        "local": {"type":"local","command":["npx","-y","safe-mcp"]}
      }
    }"#;
    let untrusted = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(false, true),
        raw,
    )
    .unwrap();
    let untrusted = preview_competitor_mcp_import(&untrusted, Path::new("/workspace")).unwrap();
    assert_eq!(
        untrusted.rows()[0].readiness(),
        ImportReadiness::ProjectTrustRequired
    );
    assert_eq!(
        untrusted
            .definitions(&BTreeSet::new())
            .err()
            .expect("untrusted project row must fail"),
        McpCompetitorImportError::AuthorityRequired {
            server: McpServerId::new("local").unwrap(),
            requirement: McpImportAuthorityRequirement::ProjectTrust,
        }
    );

    let no_exec = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(true, false),
        raw,
    )
    .unwrap();
    let no_exec = preview_competitor_mcp_import(&no_exec, Path::new("/workspace")).unwrap();
    assert_eq!(
        no_exec.rows()[0].readiness(),
        ImportReadiness::ExecutableAuthorityRequired
    );
    assert_eq!(
        no_exec
            .definitions(&BTreeSet::new())
            .err()
            .expect("unauthorized executable row must fail"),
        McpCompetitorImportError::AuthorityRequired {
            server: McpServerId::new("local").unwrap(),
            requirement: McpImportAuthorityRequirement::Executable,
        }
    );

    let allowed = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(true, true),
        raw,
    )
    .unwrap();
    let allowed = preview_competitor_mcp_import(&allowed, Path::new("/workspace")).unwrap();
    assert_eq!(allowed.rows()[0].readiness(), ImportReadiness::Ready);
    let definitions = allowed.definitions(&BTreeSet::new()).unwrap();
    assert_eq!(definitions[0].scope(), McpDefinitionScope::Project);
}

#[test]
fn exact_definition_validation_and_existing_names_fail_before_any_candidate_escapes() {
    let source = codex(
        r#"
[mcp_servers.docs]
command = "npx"
args = ["-y", "safe-mcp"]
"#,
        true,
    );
    assert_eq!(
        preview_competitor_mcp_import(&source, Path::new("relative/cwd")).unwrap_err(),
        McpCompetitorImportError::InvalidDefinition
    );

    let preview = preview_competitor_mcp_import(&source, Path::new("/workspace")).unwrap();
    let existing = BTreeSet::from([McpServerId::new("docs").unwrap()]);
    assert_eq!(
        preview
            .definitions(&existing)
            .err()
            .expect("existing server id must fail"),
        McpCompetitorImportError::DuplicateServer(McpServerId::new("docs").unwrap())
    );

    let bad_name = codex(
        r#"
[mcp_servers."Bad.Name"]
url = "https://mcp.example.test/v1"
"#,
        true,
    );
    assert_eq!(
        preview_competitor_mcp_import(&bad_name, Path::new("/workspace")).unwrap_err(),
        McpCompetitorImportError::InvalidServerId
    );
}
