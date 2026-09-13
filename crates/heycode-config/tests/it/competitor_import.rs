//! S13 competitor metadata import: preview safe facts, never credential values.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_config::{
    CompetitorConfigKind, Config, ImportAuthority, ImportExclusionReason, ImportReadiness,
    ImportSettingTarget, ImportedMcpTransport, preview_competitor_config,
};

fn has_unknown(preview: &heycode_config::CompetitorImportPreview, path: &str) -> bool {
    preview
        .unknown_fields()
        .iter()
        .any(|field| field.path() == path)
}

fn has_exclusion(
    preview: &heycode_config::CompetitorImportPreview,
    path: &str,
    reason: ImportExclusionReason,
) -> bool {
    preview
        .excluded_fields()
        .iter()
        .any(|field| field.path() == path && field.reason() == reason)
}

#[test]
fn codex_preview_imports_safe_typed_metadata_and_structurally_drops_credentials() {
    let raw = r#"
model = "gpt-5.5"
model_provider = "openai"
sandbox_mode = "workspace-write"
approval_policy = { granular = { sandbox_approval = true } }
future_toggle = true

[model_providers.openai]
base_url = "https://api.openai.com/v1"
name = "OpenAI"
env_key = "OPENAI_API_KEY"
experimental_bearer_token = "sk-proj-CODEX-CANARY"
http_headers = { Authorization = "Bearer CODEX-HEADER-CANARY" }

[mcp_servers.docs]
command = "npx"
args = ["-y", "@example/docs-mcp"]
enabled = true
future_mode = "parallel"

[mcp_servers.secure-disabled]
command = "secure-mcp"
enabled = false
env = { DOCS_TOKEN = "CODEX-MCP-CANARY" }
"#;
    let original = raw.as_bytes().to_vec();
    let preview = preview_competitor_config(
        CompetitorConfigKind::Codex,
        ImportAuthority::user(true),
        raw,
    )
    .unwrap();

    let provider = preview.provider().unwrap();
    assert_eq!(provider.provider(), "openai");
    assert_eq!(provider.model(), Some("gpt-5.5"));
    assert_eq!(provider.base_url(), Some("https://api.openai.com/v1"));
    assert_eq!(provider.readiness(), ImportReadiness::Ready);

    assert_eq!(preview.mcp_servers().len(), 2);
    let server = preview
        .mcp_servers()
        .iter()
        .find(|server| server.name() == "docs")
        .unwrap();
    assert_eq!(server.name(), "docs");
    assert!(server.enabled());
    assert_eq!(server.readiness(), ImportReadiness::Ready);
    assert_eq!(
        server.transport(),
        &ImportedMcpTransport::Stdio {
            command: "npx".to_owned(),
            args: vec!["-y".to_owned(), "@example/docs-mcp".to_owned()],
        }
    );
    assert!(preview.settings().iter().any(|setting| {
        setting.path() == "sandbox_mode"
            && setting.target() == ImportSettingTarget::PreviewOnly
            && setting.text() == Some("workspace-write")
    }));

    assert!(has_unknown(&preview, "future_toggle"));
    assert!(has_unknown(
        &preview,
        "approval_policy.granular.sandbox_approval"
    ));
    assert!(has_unknown(&preview, "model_providers.openai.name"));
    assert!(has_unknown(&preview, "mcp_servers.docs.future_mode"));
    for path in [
        "model_providers.openai.env_key",
        "model_providers.openai.experimental_bearer_token",
        "model_providers.openai.http_headers",
        "mcp_servers.secure-disabled.env",
    ] {
        assert!(has_exclusion(
            &preview,
            path,
            ImportExclusionReason::CredentialMaterial
        ));
    }
    let debug = format!("{preview:?}");
    for canary in ["CODEX-CANARY", "CODEX-HEADER-CANARY", "CODEX-MCP-CANARY"] {
        assert!(
            !debug.contains(canary),
            "preview retained {canary}: {debug}"
        );
    }

    let candidate = preview.config_candidate(&Config::defaults()).unwrap();
    assert_eq!(candidate.llm.provider, "openai");
    assert_eq!(candidate.llm.model, "gpt-5.5");
    assert_eq!(
        candidate.llm.base_url.as_deref(),
        Some("https://api.openai.com/v1")
    );
    assert!(candidate.llm.api_key_env.is_none());
    assert!(candidate.mcp.servers["docs"].env.is_empty());
    assert!(!candidate.mcp.servers.contains_key("secure-disabled"));
    assert_eq!(
        raw.as_bytes(),
        original,
        "preview must not mutate source bytes"
    );
}

#[test]
fn claude_settings_and_mcp_keep_model_metadata_separate_from_authority() {
    assert!(
        preview_competitor_config(
            CompetitorConfigKind::ClaudeSettings,
            ImportAuthority::user(false),
            "{// comments are not valid Claude settings\n\"model\":\"claude-opus-4-8\"}",
        )
        .is_err(),
        "Claude settings are strict JSON, unlike OpenCode JSONC"
    );
    let settings = r#"{
      "$schema":"https://json.schemastore.org/claude-code-settings.json",
      "model":"claude-opus-4-8",
      "editorMode":"vim",
      "env":{"ANTHROPIC_API_KEY":"CLAUDE-ENV-CANARY"},
      "apiKeyHelper":"security find-generic-password CLAUDE-HELPER-CANARY",
      "hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"CLAUDE-HOOK-CANARY"}]}]},
      "futureSetting":{"mode":"safe"}
    }"#;
    let preview = preview_competitor_config(
        CompetitorConfigKind::ClaudeSettings,
        ImportAuthority::user(false),
        settings,
    )
    .unwrap();
    let provider = preview.provider().unwrap();
    assert_eq!(provider.provider(), "anthropic");
    assert_eq!(provider.model(), Some("claude-opus-4-8"));
    assert!(preview.settings().iter().any(|setting| {
        setting.path() == "editorMode"
            && setting.target() == ImportSettingTarget::PreviewOnly
            && setting.text() == Some("vim")
    }));
    assert!(has_unknown(&preview, "futureSetting.mode"));
    assert!(has_exclusion(
        &preview,
        "env",
        ImportExclusionReason::CredentialMaterial
    ));
    assert!(has_exclusion(
        &preview,
        "apiKeyHelper",
        ImportExclusionReason::CredentialMaterial
    ));
    assert!(has_exclusion(
        &preview,
        "hooks",
        ImportExclusionReason::ExecutableAuthority
    ));
    let debug = format!("{preview:?}");
    assert!(!debug.contains("CLAUDE-ENV-CANARY"));
    assert!(!debug.contains("CLAUDE-HELPER-CANARY"));
    assert!(!debug.contains("CLAUDE-HOOK-CANARY"));

    let mcp = r#"{
      "mcpServers": {
        "local": {
          "command": "node",
          "args": ["server.js"],
          "env": {"TOKEN":"CLAUDE-MCP-CANARY"}
        },
        "remote": {
          "type": "http",
          "url": "https://mcp.example.test/v1",
          "headers": {"Authorization":"Bearer CLAUDE-HEADER-CANARY"}
        }
      }
    }"#;
    let blocked = preview_competitor_config(
        CompetitorConfigKind::ClaudeMcp,
        ImportAuthority::user(false),
        mcp,
    )
    .unwrap();
    assert_eq!(blocked.mcp_servers().len(), 2);
    assert_eq!(
        blocked
            .mcp_servers()
            .iter()
            .find(|server| server.name() == "local")
            .unwrap()
            .readiness(),
        ImportReadiness::ExecutableAuthorityRequired
    );
    assert_eq!(
        blocked
            .mcp_servers()
            .iter()
            .find(|server| server.name() == "remote")
            .unwrap()
            .readiness(),
        ImportReadiness::ExcludedMetadataRequired
    );
    assert!(blocked.config_candidate(&Config::defaults()).is_err());
    assert!(!format!("{blocked:?}").contains("CLAUDE-MCP-CANARY"));
    assert!(!format!("{blocked:?}").contains("CLAUDE-HEADER-CANARY"));

    let allowed = preview_competitor_config(
        CompetitorConfigKind::ClaudeMcp,
        ImportAuthority::user(true),
        mcp,
    )
    .unwrap();
    assert!(
        allowed
            .mcp_servers()
            .iter()
            .all(|server| server.readiness() == ImportReadiness::ExcludedMetadataRequired)
    );
    assert!(allowed.config_candidate(&Config::defaults()).is_err());

    let clean_mcp = r#"{
      "mcpServers": {
        "local": {"command":"node","args":["server.js"]},
        "remote": {"type":"http","url":"https://mcp.example.test/v1"}
      }
    }"#;
    let clean = preview_competitor_config(
        CompetitorConfigKind::ClaudeMcp,
        ImportAuthority::user(true),
        clean_mcp,
    )
    .unwrap();
    let candidate = clean.config_candidate(&Config::defaults()).unwrap();
    assert_eq!(candidate.mcp.servers.len(), 2);
    assert!(
        candidate
            .mcp
            .servers
            .values()
            .all(|server| server.env.is_empty())
    );
}

#[test]
fn opencode_jsonc_preview_requires_project_trust_then_executable_authority() {
    let raw = r#"{
      // OpenCode officially accepts JSONC and trailing commas.
      "$schema": "https://opencode.ai/config.json",
      "model": "openrouter/z-ai/glm-5.3-flash",
      "provider": {
        "openrouter": {
          "options": {
            "baseURL": "https://openrouter.ai/api/v1",
            "apiKey": "{env:OPENROUTER_API_KEY}",
            "timeout": 600000
          }
        }
      },
      "compaction": {"auto": false, "prune": true},
      "mcp": {
        "local": {
          "type": "local",
          "command": ["npx", "-y", "@example/local"],
          "environment": {"TOKEN":"OPENCODE-MCP-CANARY"},
        },
        "remote": {
          "type": "remote",
          "url": "https://mcp.example.test/v1",
          "headers": {"Authorization":"Bearer OPENCODE-HEADER-CANARY"}
        }
      },
      "future": "sk-proj-OPENCODE-UNKNOWN-CANARY",
    }"#;
    let original = raw.as_bytes().to_vec();

    let untrusted = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(false, true),
        raw,
    )
    .unwrap();
    assert_eq!(
        untrusted.provider().unwrap().readiness(),
        ImportReadiness::ProjectTrustRequired
    );
    assert!(
        untrusted
            .mcp_servers()
            .iter()
            .all(|server| server.readiness() == ImportReadiness::ProjectTrustRequired)
    );
    assert!(untrusted.config_candidate(&Config::defaults()).is_err());

    let no_exec = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(true, false),
        raw,
    )
    .unwrap();
    assert_eq!(
        no_exec.provider().unwrap().readiness(),
        ImportReadiness::Ready
    );
    assert_eq!(
        no_exec
            .mcp_servers()
            .iter()
            .find(|server| server.name() == "local")
            .unwrap()
            .readiness(),
        ImportReadiness::ExecutableAuthorityRequired
    );
    assert_eq!(
        no_exec
            .mcp_servers()
            .iter()
            .find(|server| server.name() == "remote")
            .unwrap()
            .readiness(),
        ImportReadiness::ExcludedMetadataRequired
    );
    assert!(no_exec.config_candidate(&Config::defaults()).is_err());

    let allowed = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(true, true),
        raw,
    )
    .unwrap();
    let provider = allowed.provider().unwrap();
    assert_eq!(provider.provider(), "openrouter");
    assert_eq!(provider.model(), Some("z-ai/glm-5.3-flash"));
    assert_eq!(provider.base_url(), Some("https://openrouter.ai/api/v1"));
    assert!(allowed.settings().iter().any(|setting| {
        setting.path() == "compaction.auto"
            && setting.target() == ImportSettingTarget::ConfigCompactionAuto
            && setting.boolean() == Some(false)
    }));
    assert!(has_unknown(&allowed, "provider.openrouter.options.timeout"));
    assert!(has_unknown(&allowed, "compaction.prune"));
    assert!(has_unknown(&allowed, "future"));
    assert!(has_exclusion(
        &allowed,
        "provider.openrouter.options.apiKey",
        ImportExclusionReason::CredentialMaterial
    ));
    assert!(has_exclusion(
        &allowed,
        "mcp.local.environment",
        ImportExclusionReason::CredentialMaterial
    ));
    assert!(has_exclusion(
        &allowed,
        "mcp.remote.headers",
        ImportExclusionReason::CredentialMaterial
    ));
    let debug = format!("{allowed:?}");
    for canary in [
        "OPENROUTER_API_KEY",
        "OPENCODE-MCP-CANARY",
        "OPENCODE-HEADER-CANARY",
        "OPENCODE-UNKNOWN-CANARY",
    ] {
        assert!(
            !debug.contains(canary),
            "preview retained {canary}: {debug}"
        );
    }
    assert!(allowed.config_candidate(&Config::defaults()).is_err());

    let clean_raw = r#"{
      "model":"openrouter/z-ai/glm-5.3-flash",
      "provider":{"openrouter":{"options":{"baseURL":"https://openrouter.ai/api/v1"}}},
      "compaction":{"auto":false},
      "mcp":{"remote":{"type":"remote","url":"https://mcp.example.test/v1"}}
    }"#;
    let clean = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::project(true, true),
        clean_raw,
    )
    .unwrap();
    let candidate = clean.config_candidate(&Config::defaults()).unwrap();
    assert_eq!(candidate.llm.provider, "openrouter");
    assert_eq!(candidate.llm.model, "z-ai/glm-5.3-flash");
    assert_eq!(candidate.mcp.servers.len(), 1);
    assert!(!candidate.compaction.auto);

    let v2 = r#"{
      "mcp":{"servers":{"nested":{"type":"remote","url":"https://nested.example.test/mcp"}}}
    }"#;
    let nested = preview_competitor_config(
        CompetitorConfigKind::OpenCode,
        ImportAuthority::user(true),
        v2,
    )
    .unwrap()
    .config_candidate(&Config::defaults())
    .unwrap();
    assert!(nested.mcp.servers.contains_key("nested"));
    assert_eq!(
        raw.as_bytes(),
        original,
        "preview must not mutate source bytes"
    );
}

#[test]
fn unsafe_urls_and_secret_shaped_known_values_are_excluded_not_retained() {
    let raw = r#"
model = "sk-proj-MODEL-CANARY"
model_provider = "openai"

[model_providers.openai]
base_url = "https://user:password@example.test/v1?token=URL-CANARY"

[mcp_servers.remote]
url = "https://mcp.example.test/v1?api_key=MCP-URL-CANARY"
"#;
    let preview = preview_competitor_config(
        CompetitorConfigKind::Codex,
        ImportAuthority::user(true),
        raw,
    )
    .unwrap();
    assert!(preview.provider().is_none());
    assert!(preview.mcp_servers().is_empty());
    assert!(has_exclusion(
        &preview,
        "model",
        ImportExclusionReason::UnsafeValue
    ));
    assert!(has_exclusion(
        &preview,
        "model_providers.openai.base_url",
        ImportExclusionReason::CredentialMaterial
    ));
    assert!(has_exclusion(
        &preview,
        "mcp_servers.remote.url",
        ImportExclusionReason::CredentialMaterial
    ));
    let debug = format!("{preview:?}");
    for canary in ["MODEL-CANARY", "URL-CANARY", "MCP-URL-CANARY"] {
        assert!(
            !debug.contains(canary),
            "preview retained {canary}: {debug}"
        );
    }
}
