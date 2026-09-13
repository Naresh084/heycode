//! MCP01 registry, redaction and atomic generation contracts.

#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

use std::collections::{BTreeMap, BTreeSet};

use heycode_core::{Context, Plugin, compose};
use heycode_mcp::{
    McpApprovalMode, McpArgument, McpAuthenticationState, McpCapabilitySet,
    McpConnectionProviderId, McpConnectionState, McpContributionCounts, McpDefinitionScope,
    McpEnvironmentValue, McpExposurePolicy, McpFailureCode, McpGenerationCandidate,
    McpGenerationRetention, McpReconnectPolicy, McpRegistry, McpRegistryError, McpSecretReference,
    McpServerDefinition, McpServerId, McpStdioTransport, McpStreamableHttpTransport, McpTimeouts,
    McpToolPolicy, McpTransportDefinition, SERVICE_MCP, mcp_registry_plugin,
};

fn secret_reference(value: &str) -> McpSecretReference {
    McpSecretReference::new(value).unwrap()
}

fn stdio_definition(id: &str, display_name: &str) -> McpServerDefinition {
    let token = secret_reference("mcp.github.token");
    let transport = McpStdioTransport::new(
        "/usr/bin/example-mcp",
        "/tmp",
        vec![
            McpArgument::literal("--token").unwrap(),
            McpArgument::literal("ARGUMENT_SECRET_CANARY").unwrap(),
            McpArgument::credential(token.clone()),
        ],
        BTreeMap::from([
            (
                "FIXTURE_LITERAL".to_owned(),
                McpEnvironmentValue::literal("ENVIRONMENT_SECRET_CANARY").unwrap(),
            ),
            (
                "FIXTURE_TOKEN".to_owned(),
                McpEnvironmentValue::credential(token),
            ),
        ]),
    )
    .unwrap();
    McpServerDefinition::new(
        id,
        display_name,
        McpDefinitionScope::User,
        McpTransportDefinition::Stdio(transport),
    )
    .unwrap()
    .with_required(true)
    .with_timeouts(McpTimeouts::new(5_000, 30_000, 60_000, 5_000).unwrap())
    .with_reconnect(McpReconnectPolicy::new(true, 250, 10_000, 8).unwrap())
    .with_tool_policy(
        McpToolPolicy::new(
            Some(BTreeSet::from(["read".to_owned(), "search".to_owned()])),
            BTreeSet::from(["delete".to_owned()]),
            McpApprovalMode::Prompt,
            BTreeMap::from([("read".to_owned(), McpApprovalMode::Allow)]),
        )
        .unwrap(),
    )
    .with_exposure(McpExposurePolicy {
        resources: true,
        prompts: false,
        instructions: true,
    })
}

fn http_definition(id: &str, display_name: &str) -> McpServerDefinition {
    let transport = McpStreamableHttpTransport::new(
        "https://mcp.example.test/v1",
        BTreeMap::from([(
            "authorization".to_owned(),
            secret_reference("mcp.remote.authorization"),
        )]),
    )
    .unwrap();
    McpServerDefinition::new(
        id,
        display_name,
        McpDefinitionScope::Managed,
        McpTransportDefinition::StreamableHttp(transport),
    )
    .unwrap()
    .with_enabled(false)
}

fn candidate(observed_at_ms: u64, tool_count: u32) -> McpGenerationCandidate {
    McpGenerationCandidate::new(
        "2025-03-26",
        "fixture-server",
        "1.2.3",
        McpCapabilitySet {
            tools: true,
            resources: true,
            prompts: false,
            logging: true,
            roots: false,
            elicitation: false,
            sampling: false,
        },
        McpContributionCounts {
            tools: tool_count,
            resources: 2,
            prompts: 0,
        },
        observed_at_ms,
    )
    .unwrap()
}

#[test]
fn registry_plugin_publishes_the_service_definition() {
    let plugins: Vec<Box<dyn Plugin>> = vec![mcp_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<McpRegistry>(SERVICE_MCP).unwrap();
    assert!(registry.snapshot().unwrap().active());
    assert!(registry.snapshot().unwrap().servers().is_empty());
    assert_eq!(context.owner_of(SERVICE_MCP), Some("mcp-registry"));
    assert_eq!(plugins[0].provides(), &[SERVICE_MCP]);

    context.shutdown();
    assert!(!registry.snapshot().unwrap().active());
    assert!(matches!(
        registry.register_definition(&Context::new(), stdio_definition("late", "Late")),
        Err(McpRegistryError::RegistryClosed)
    ));
}

#[test]
fn definitions_are_sorted_exactly_inspectable_and_publicly_redacted() {
    let registry = McpRegistry::new();
    let mut owner = Context::new();
    registry
        .register_definition(&owner, stdio_definition("zeta", "Zeta"))
        .unwrap();
    registry
        .register_definition(&owner, http_definition("alpha", "Alpha"))
        .unwrap();

    let snapshot = registry.snapshot().unwrap();
    assert_eq!(snapshot.schema_version(), 1);
    assert_eq!(snapshot.revision(), 2);
    assert_eq!(
        snapshot
            .servers()
            .iter()
            .map(|server| server.definition().id().as_str())
            .collect::<Vec<_>>(),
        ["alpha", "zeta"]
    );
    assert_eq!(snapshot.servers()[0].state(), &McpConnectionState::Disabled);
    assert_eq!(snapshot.servers()[1].state(), &McpConnectionState::Inactive);

    let public_json = serde_json::to_string(&*snapshot).unwrap();
    let public_debug = format!("{snapshot:?}");
    for canary in ["ARGUMENT_SECRET_CANARY", "ENVIRONMENT_SECRET_CANARY"] {
        assert!(!public_json.contains(canary), "{public_json}");
        assert!(!public_debug.contains(canary), "{public_debug}");
    }
    assert!(public_json.contains("mcp.github.token"));
    assert!(public_json.contains("mcp.remote.authorization"));
    assert!(public_json.contains("\"schema_version\":1"));

    let exact = registry
        .definition(&McpServerId::new("zeta").unwrap())
        .unwrap()
        .unwrap();
    let McpTransportDefinition::Stdio(stdio) = exact.transport() else {
        panic!("expected exact stdio definition");
    };
    assert_eq!(
        stdio.arguments()[1].literal_value(),
        Some("ARGUMENT_SECRET_CANARY")
    );
    assert_eq!(
        stdio.environment()["FIXTURE_LITERAL"].literal_value(),
        Some("ENVIRONMENT_SECRET_CANARY")
    );

    owner.shutdown();
    let empty = registry.snapshot().unwrap();
    assert!(empty.servers().is_empty());
    assert_eq!(empty.revision(), 4);
}

#[test]
fn generations_swap_atomically_and_failure_retention_is_explicit() {
    let registry = McpRegistry::new();
    let mut definition_owner = Context::new();
    registry
        .register_definition(&definition_owner, stdio_definition("github", "GitHub"))
        .unwrap();
    let mut connection_owner = Context::new();
    let publisher = registry
        .register_connection(
            &connection_owner,
            &McpServerId::new("github").unwrap(),
            McpConnectionProviderId::new("stdio-local").unwrap(),
            100,
        )
        .unwrap();

    let starting = registry.snapshot().unwrap();
    assert_eq!(
        starting.servers()[0].state(),
        &McpConnectionState::Starting { since_ms: 100 }
    );
    assert!(starting.servers()[0].last_good_generation().is_none());

    let first = publisher.publish_generation(candidate(200, 2)).unwrap();
    assert_eq!(first.number(), 1);
    assert_eq!(first.definition_revision(), 1);
    assert_eq!(first.contributions().tools, 2);
    let first_snapshot = registry.snapshot().unwrap();
    assert_eq!(
        first_snapshot.servers()[0].state(),
        &McpConnectionState::Ready { since_ms: 200 }
    );
    assert_eq!(
        first_snapshot.servers()[0].authentication(),
        McpAuthenticationState::Connected,
        "a successful credential-backed generation proves authentication"
    );
    assert!(matches!(
        publisher.report_reconnecting(1, 7, 250),
        Err(McpRegistryError::InvalidField { .. })
    ));
    publisher.report_reconnecting(1, 8, 250).unwrap();
    let reconnecting_snapshot = registry.snapshot().unwrap();

    let invalid = McpGenerationCandidate::new(
        "\n",
        "fixture-server",
        "1",
        McpCapabilitySet::default(),
        McpContributionCounts::default(),
        250,
    );
    assert!(matches!(
        invalid,
        Err(McpRegistryError::InvalidField { .. })
    ));
    let unchanged = registry.snapshot().unwrap();
    assert_eq!(unchanged.revision(), reconnecting_snapshot.revision());
    assert_eq!(
        unchanged.servers()[0]
            .last_good_generation()
            .unwrap()
            .number(),
        1
    );

    let second = publisher.publish_generation(candidate(300, 4)).unwrap();
    assert_eq!(second.number(), 2);
    assert_eq!(second.contributions().tools, 4);
    assert_eq!(
        first_snapshot.servers()[0]
            .last_good_generation()
            .unwrap()
            .number(),
        1,
        "published snapshots are immutable generations"
    );
    publisher
        .report_failure(
            McpFailureCode::Transport,
            400,
            McpGenerationRetention::KeepLastGood,
        )
        .unwrap();
    let degraded = registry.snapshot().unwrap();
    assert_eq!(
        degraded.servers()[0].state(),
        &McpConnectionState::Degraded {
            code: McpFailureCode::Transport,
            observed_at_ms: 400,
        }
    );
    assert_eq!(
        degraded.servers()[0]
            .last_good_generation()
            .unwrap()
            .number(),
        2
    );

    publisher
        .report_failure(
            McpFailureCode::ReconnectExhausted,
            500,
            McpGenerationRetention::Remove,
        )
        .unwrap();
    let failed = registry.snapshot().unwrap();
    assert_eq!(
        failed.servers()[0].state(),
        &McpConnectionState::Failed {
            code: McpFailureCode::ReconnectExhausted,
            observed_at_ms: 500,
        }
    );
    assert!(failed.servers()[0].last_good_generation().is_none());

    let third = publisher.publish_generation(candidate(600, 1)).unwrap();
    assert_eq!(third.number(), 3, "generation numbers are never reused");
    connection_owner.shutdown();
    let inactive = registry.snapshot().unwrap();
    assert_eq!(inactive.servers()[0].state(), &McpConnectionState::Inactive);
    assert!(inactive.servers()[0].last_good_generation().is_none());
    assert!(matches!(
        publisher.publish_generation(candidate(700, 1)),
        Err(McpRegistryError::StaleRegistration { .. })
    ));
    definition_owner.shutdown();
}

#[test]
fn stale_connection_handles_cannot_clobber_a_replacement() {
    let registry = McpRegistry::new();
    let mut definition_owner = Context::new();
    let id = McpServerId::new("server").unwrap();
    registry
        .register_definition(&definition_owner, stdio_definition("server", "Server"))
        .unwrap();

    let mut first_owner = Context::new();
    let first = registry
        .register_connection(
            &first_owner,
            &id,
            McpConnectionProviderId::new("stdio-local").unwrap(),
            10,
        )
        .unwrap();
    assert!(matches!(
        registry.register_connection(
            &Context::new(),
            &id,
            McpConnectionProviderId::new("streamable-http").unwrap(),
            11,
        ),
        Err(McpRegistryError::DuplicateConnection { .. })
    ));
    first.publish_generation(candidate(20, 1)).unwrap();
    first_owner.shutdown();

    let mut second_owner = Context::new();
    let second = registry
        .register_connection(
            &second_owner,
            &id,
            McpConnectionProviderId::new("stdio-local").unwrap(),
            30,
        )
        .unwrap();
    second.publish_generation(candidate(40, 2)).unwrap();
    assert!(matches!(
        first.report_authentication(McpAuthenticationState::Expired),
        Err(McpRegistryError::StaleRegistration { .. })
    ));
    let current = registry.snapshot().unwrap();
    assert_eq!(
        current.servers()[0].connection_provider().unwrap().as_str(),
        "stdio-local"
    );
    assert_eq!(
        current.servers()[0]
            .last_good_generation()
            .unwrap()
            .number(),
        2
    );

    definition_owner.shutdown();
    assert!(registry.snapshot().unwrap().servers().is_empty());
    assert!(matches!(
        second.report_reconnecting(1, 3, 50),
        Err(McpRegistryError::StaleRegistration { .. })
    ));
    second_owner.shutdown();
}

#[test]
fn validation_rejects_ambiguous_or_secret_prone_boundaries() {
    assert!(McpServerId::new("not valid").is_err());
    assert!(McpConnectionProviderId::new("UPPER").is_err());
    assert!(McpSecretReference::new(" token ").is_err());
    assert_eq!(
        McpArgument::literal("line\nbreak").unwrap().literal_value(),
        Some("line\nbreak")
    );
    assert!(McpArgument::literal("contains\0nul").is_err());
    assert!(McpEnvironmentValue::literal("contains\0nul").is_err());
    assert!(McpStdioTransport::new("python", "relative", vec![], BTreeMap::new()).is_err());
    assert!(
        McpStreamableHttpTransport::new("https://user:secret@example.test/mcp", BTreeMap::new())
            .is_err()
    );
    assert!(
        McpStreamableHttpTransport::new("https://example.test/mcp?token=secret", BTreeMap::new())
            .is_err()
    );
    assert!(
        McpStreamableHttpTransport::new("https://example.test:65536/mcp", BTreeMap::new()).is_err()
    );
    assert!(
        McpStreamableHttpTransport::new("https://user%40example.test/mcp", BTreeMap::new())
            .is_err()
    );
    assert!(McpTimeouts::new(0, 1, 1, 1).is_err());
    assert!(McpReconnectPolicy::new(true, 100, 99, 1).is_err());
    assert!(
        McpGenerationCandidate::new(
            "2025-03-26",
            "fixture",
            "1",
            McpCapabilitySet::default(),
            McpContributionCounts {
                tools: 1,
                ..McpContributionCounts::default()
            },
            1,
        )
        .is_err()
    );
    assert!(
        McpToolPolicy::new(
            Some(BTreeSet::from(["same".to_owned()])),
            BTreeSet::from(["same".to_owned()]),
            McpApprovalMode::Prompt,
            BTreeMap::new(),
        )
        .is_err()
    );
    assert!(
        McpToolPolicy::new(
            Some(BTreeSet::from(["read".to_owned()])),
            BTreeSet::new(),
            McpApprovalMode::Prompt,
            BTreeMap::from([("search".to_owned(), McpApprovalMode::Allow)]),
        )
        .is_err()
    );

    let no_auth_transport =
        McpStdioTransport::new("/usr/bin/example-mcp", "/tmp", Vec::new(), BTreeMap::new())
            .unwrap();
    let no_auth = McpServerDefinition::new(
        "no-auth",
        "No auth",
        McpDefinitionScope::User,
        McpTransportDefinition::Stdio(no_auth_transport),
    )
    .unwrap();
    let no_auth_registry = McpRegistry::new();
    let mut no_auth_definition_owner = Context::new();
    no_auth_registry
        .register_definition(&no_auth_definition_owner, no_auth)
        .unwrap();
    let mut no_auth_connection_owner = Context::new();
    let no_auth_publisher = no_auth_registry
        .register_connection(
            &no_auth_connection_owner,
            &McpServerId::new("no-auth").unwrap(),
            McpConnectionProviderId::new("stdio-local").unwrap(),
            1,
        )
        .unwrap();
    assert!(matches!(
        no_auth_publisher.report_authentication(McpAuthenticationState::Expired),
        Err(McpRegistryError::InvalidField { .. })
    ));
    assert!(matches!(
        no_auth_publisher.report_authentication_required(2),
        Err(McpRegistryError::InvalidField { .. })
    ));
    no_auth_connection_owner.shutdown();
    no_auth_definition_owner.shutdown();

    let registry = McpRegistry::new();
    let owner = Context::new();
    registry
        .register_definition(&owner, http_definition("disabled", "Disabled"))
        .unwrap();
    assert!(matches!(
        registry.register_connection(
            &Context::new(),
            &McpServerId::new("disabled").unwrap(),
            McpConnectionProviderId::new("streamable-http").unwrap(),
            1,
        ),
        Err(McpRegistryError::ServerDisabled { .. })
    ));
}
