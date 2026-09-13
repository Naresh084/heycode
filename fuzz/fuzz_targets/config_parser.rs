#![no_main]

use heycode_config::{
    CONFIG_SCHEMA_VERSION, Config, ConfigMigrationPlan, ConfigVersionState, McpServerTransport,
};
use libfuzzer_sys::fuzz_target;

const MAX_INPUT_BYTES: usize = 64 * 1024;

#[derive(PartialEq, Eq)]
enum TransportFingerprint {
    Stdio {
        command: String,
        args: Vec<String>,
        environment: Vec<(String, String)>,
    },
    Http(String),
    Invalid,
}

#[derive(PartialEq, Eq)]
struct ConfigFingerprint {
    schema_version: u32,
    plugins: Vec<String>,
    provider: String,
    model: String,
    base_url: Option<String>,
    credential_reference: Option<String>,
    terminals_enabled: bool,
    bash_timeout_ms: u64,
    read_max_bytes: usize,
    read_max_lines: usize,
    auto_title: bool,
    approval: String,
    compaction_auto: bool,
    compaction_threshold_bits: u32,
    context_window: u64,
    subagent_depth: u32,
    sandbox: String,
    web_enabled: bool,
    mcp: Vec<(String, TransportFingerprint)>,
}

fn fingerprint(config: &Config) -> ConfigFingerprint {
    let mut mcp = config
        .mcp
        .servers
        .iter()
        .map(|(name, server)| {
            let transport = match server.transport(name) {
                Ok(McpServerTransport::Stdio { command, args, env }) => {
                    let mut environment = env.into_iter().collect::<Vec<_>>();
                    environment.sort();
                    TransportFingerprint::Stdio {
                        command,
                        args,
                        environment,
                    }
                }
                Ok(McpServerTransport::StreamableHttp { url }) => TransportFingerprint::Http(url),
                Err(_) => TransportFingerprint::Invalid,
            };
            (name.clone(), transport)
        })
        .collect::<Vec<_>>();
    mcp.sort_by(|left, right| left.0.cmp(&right.0));

    ConfigFingerprint {
        schema_version: config.schema_version,
        plugins: config.profile.plugins.clone(),
        provider: config.llm.provider.clone(),
        model: config.llm.model.clone(),
        base_url: config.llm.base_url.clone(),
        credential_reference: config.llm.api_key_env.clone(),
        terminals_enabled: config.tools.terminals_enabled,
        bash_timeout_ms: config.tools.bash_timeout_ms,
        read_max_bytes: config.tools.read_max_bytes,
        read_max_lines: config.tools.read_max_lines,
        auto_title: config.ui.auto_title,
        approval: config.approval.mode.to_string(),
        compaction_auto: config.compaction.auto,
        compaction_threshold_bits: config.compaction.threshold_ratio.to_bits(),
        context_window: config.compaction.context_window,
        subagent_depth: config.subagent.max_depth,
        sandbox: config.sandbox.mode.to_string(),
        web_enabled: config.web.enabled,
        mcp,
    }
}

fuzz_target!(|input: &[u8]| {
    let input = &input[..input.len().min(MAX_INPUT_BYTES)];
    let Ok(root) = tempfile::tempdir() else {
        return;
    };
    let path = root.path().join("config.toml");
    if std::fs::write(&path, input).is_err() {
        return;
    }

    let text = std::str::from_utf8(input).ok();
    if let Some(text) = text {
        let first = Config::classify_document(text);
        let second = Config::classify_document(text);
        assert!(
            first.as_ref().ok() == second.as_ref().ok(),
            "configuration classification must be deterministic"
        );
    }

    let first = Config::from_file(&path);
    let second = Config::from_file(&path);
    match (first, second) {
        (Ok(first), Ok(second)) => {
            assert!(
                fingerprint(&first) == fingerprint(&second),
                "configuration parsing and transport resolution must be deterministic"
            );
            assert!(
                first.schema_version <= CONFIG_SCHEMA_VERSION,
                "accepted configuration must not exceed the supported schema"
            );

            if let Some(text) = text {
                let Ok(state) = Config::classify_document(text) else {
                    panic!("accepted configuration must have classifiable schema evidence");
                };
                match state {
                    ConfigVersionState::Unversioned => assert!(
                        first.schema_version == CONFIG_SCHEMA_VERSION,
                        "unversioned configuration must receive the current in-memory schema"
                    ),
                    ConfigVersionState::Older(version) | ConfigVersionState::Current(version) => {
                        assert!(
                            first.schema_version == version,
                            "parsed configuration schema must match classified evidence"
                        )
                    }
                    ConfigVersionState::Newer(_) => {
                        panic!("newer configuration must fail before full parsing")
                    }
                }
            }

            let available = first
                .profile
                .plugins
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>();
            let Ok(resolved) = first.resolve_plugins(&available) else {
                panic!("known plugin rows must preserve their exact order");
            };
            assert!(
                resolved == first.profile.plugins,
                "plugin resolution must preserve configured order"
            );
        }
        (Err(_), Err(_)) => {}
        _ => panic!("configuration parsing outcome must be deterministic"),
    }

    let before = std::fs::read(&path).ok();
    let first_plan = ConfigMigrationPlan::read(&path, &[]);
    let second_plan = ConfigMigrationPlan::read(&path, &[]);
    match (&first_plan, &second_plan) {
        (Ok(Some(first)), Ok(Some(second))) => assert!(
            first.from() == second.from()
                && first.to() == second.to()
                && first.changes() == second.changes(),
            "configuration migration planning must be deterministic"
        ),
        (Ok(None), Ok(None)) | (Err(_), Err(_)) => {}
        _ => panic!("configuration migration-plan outcome must be deterministic"),
    }
    assert!(
        before == std::fs::read(&path).ok(),
        "configuration migration planning must not mutate its source"
    );
});
