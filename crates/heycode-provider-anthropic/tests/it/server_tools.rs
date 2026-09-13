//! PAN03 provider-owned server-tool definitions and pause-state facts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{ProviderProtocol, ProviderStateItem, ProviderStateKind, ServerToolOutcome};
use heycode_llm::{CapabilitySupport, FinishReason, InferenceInput};
use heycode_provider_anthropic::{
    ANTHROPIC_CLAUDE_OPUS_5, ANTHROPIC_SERVER_TOOLS_OPTION_KIND, AnthropicPendingPauseState,
    AnthropicServerToolContinuation, AnthropicServerToolDefinition, AnthropicServerToolFault,
    AnthropicServerToolKind, AnthropicServerToolOutcome, AnthropicServerToolPlan,
    advisor_pair_support, historical_server_tool_beta_headers, server_tool_support,
};

fn state(model: &str, content: Vec<serde_json::Value>) -> ProviderStateItem {
    ProviderStateItem::new(
        "anthropic",
        model,
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({"role":"assistant","content":content}),
    )
    .unwrap()
}

fn search_call(id: &str) -> serde_json::Value {
    serde_json::json!({
        "type":"server_tool_use",
        "id":id,
        "name":"web_search",
        "input":{"query":"private query"}
    })
}

#[test]
fn six_definitions_pin_current_types_names_betas_and_request_extensions() {
    let definitions = [
        AnthropicServerToolDefinition::web_search(),
        AnthropicServerToolDefinition::web_fetch(),
        AnthropicServerToolDefinition::code_execution(),
        AnthropicServerToolDefinition::advisor("claude-opus-5", 25).unwrap(),
        AnthropicServerToolDefinition::tool_search_regex(),
        AnthropicServerToolDefinition::mcp_connector("docs", "https://mcp.example.test/sse")
            .unwrap(),
    ];
    assert_eq!(definitions.len(), AnthropicServerToolKind::ALL.len());
    assert_eq!(
        definitions[0].tool(),
        &serde_json::json!({
            "type":"web_search_20250305",
            "name":"web_search",
            "max_uses":5
        })
    );
    assert_eq!(
        definitions[1].tool(),
        &serde_json::json!({
            "type":"web_fetch_20250910",
            "name":"web_fetch",
            "max_uses":5
        })
    );
    assert_eq!(
        definitions[2].tool(),
        &serde_json::json!({"type":"code_execution_20260521","name":"code_execution"})
    );
    assert_eq!(
        definitions[3].tool(),
        &serde_json::json!({
            "type":"advisor_20260301",
            "name":"advisor",
            "model":"claude-opus-5",
            "max_uses":25
        })
    );
    assert_eq!(definitions[3].beta_headers(), &["advisor-tool-2026-03-01"]);
    assert_eq!(
        definitions[4].tool(),
        &serde_json::json!({
            "type":"tool_search_tool_regex_20251119",
            "name":"tool_search_tool_regex"
        })
    );
    assert_eq!(
        definitions[5].tool(),
        &serde_json::json!({
            "type":"mcp_toolset",
            "mcp_server_name":"docs",
            "default_config":{"enabled":true,"defer_loading":false}
        })
    );
    assert_eq!(definitions[5].beta_headers(), &["mcp-client-2025-11-20"]);
    assert_eq!(
        definitions[5].request_fields(),
        Some(&serde_json::json!({
            "mcp_servers":[{
                "type":"url",
                "url":"https://mcp.example.test/sse",
                "name":"docs"
            }]
        }))
    );
    assert!(
        !definitions[5]
            .request_fields()
            .unwrap()
            .to_string()
            .contains("authorization_token")
    );
}

#[test]
fn model_and_advisor_pair_evidence_stays_tri_state() {
    assert_eq!(
        server_tool_support(ANTHROPIC_CLAUDE_OPUS_5, AnthropicServerToolKind::WebSearch),
        CapabilitySupport::Supported
    );
    assert_eq!(
        server_tool_support(ANTHROPIC_CLAUDE_OPUS_5, AnthropicServerToolKind::WebFetch),
        CapabilitySupport::Unsupported,
        "Anthropic's Opus 5 migration guide explicitly excludes web fetch"
    );
    for model in [
        ANTHROPIC_CLAUDE_OPUS_5,
        "claude-fable-5",
        "claude-mythos-5",
        "claude-sonnet-5",
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-opus-4-5-20251101",
        "claude-sonnet-4-5-20250929",
        "claude-haiku-4-5-20251001",
    ] {
        assert_eq!(
            server_tool_support(model, AnthropicServerToolKind::CodeExecution),
            CapabilitySupport::Supported,
            "{model}"
        );
    }
    for model in [
        "claude-fable-5",
        "claude-mythos-5",
        ANTHROPIC_CLAUDE_OPUS_5,
        "claude-opus-4-8",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-opus-4-5-20251101",
        "claude-sonnet-4-5-20250929",
        "claude-haiku-4-5-20251001",
    ] {
        assert_eq!(
            server_tool_support(model, AnthropicServerToolKind::ToolSearch),
            CapabilitySupport::Supported,
            "{model}"
        );
    }
    assert_eq!(
        server_tool_support(
            "claude-opus-4-1-20250805",
            AnthropicServerToolKind::ToolSearch
        ),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        server_tool_support("unlisted-model", AnthropicServerToolKind::ToolSearch),
        CapabilitySupport::Unknown
    );
    assert_eq!(
        advisor_pair_support("claude-sonnet-5", ANTHROPIC_CLAUDE_OPUS_5),
        CapabilitySupport::Supported
    );
    assert_eq!(
        advisor_pair_support(ANTHROPIC_CLAUDE_OPUS_5, "claude-sonnet-5"),
        CapabilitySupport::Unsupported
    );
    assert_eq!(
        advisor_pair_support("unknown-executor", ANTHROPIC_CLAUDE_OPUS_5),
        CapabilitySupport::Unknown
    );
}

#[test]
fn plan_merges_exact_betas_request_fields_and_durable_option() {
    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::web_search(),
        AnthropicServerToolDefinition::advisor(ANTHROPIC_CLAUDE_OPUS_5, 3).unwrap(),
        AnthropicServerToolDefinition::mcp_connector("docs", "https://mcp.example.test/sse")
            .unwrap(),
    ])
    .unwrap();
    assert_eq!(
        plan.beta_headers(),
        &["advisor-tool-2026-03-01", "mcp-client-2025-11-20"]
    );
    assert_eq!(
        plan.request_fields(),
        &serde_json::json!({
            "mcp_servers":[{
                "type":"url",
                "url":"https://mcp.example.test/sse",
                "name":"docs"
            }]
        })
    );
    assert_eq!(plan.tools_for(ANTHROPIC_CLAUDE_OPUS_5).unwrap().len(), 3);
    let option = plan.provider_option().unwrap();
    assert_eq!(option.provider(), "anthropic");
    assert_eq!(option.kind(), ANTHROPIC_SERVER_TOOLS_OPTION_KIND);
    assert_eq!(option.data()["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        option.data()["request"],
        serde_json::json!({
            "mcp_servers":[{
                "type":"url",
                "url":"https://mcp.example.test/sse",
                "name":"docs"
            }]
        })
    );
    assert!(!format!("{plan:?} {option:?}").contains("mcp.example.test"));
}

#[test]
fn duplicate_non_mcp_family_and_unsafe_mcp_configuration_fail_closed() {
    assert_eq!(
        AnthropicServerToolPlan::new(vec![
            AnthropicServerToolDefinition::web_search(),
            AnthropicServerToolDefinition::web_search(),
        ])
        .unwrap_err(),
        AnthropicServerToolFault::InvalidConfiguration
    );
    for (name, url) in [
        ("docs", "http://mcp.example.test/sse"),
        ("docs", "https://mcp.example.test/sse?token=secret"),
        ("sk-ant-api03 SECRET", "https://mcp.example.test/sse"),
    ] {
        let fault = AnthropicServerToolDefinition::mcp_connector(name, url).unwrap_err();
        assert_eq!(fault, AnthropicServerToolFault::InvalidConfiguration);
        assert!(!format!("{fault:?} {fault}").contains("SECRET"));
    }
}

#[test]
fn multiple_mcp_servers_share_one_exact_server_name_restricted_route() {
    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::mcp_connector("docs", "https://mcp.example.test/docs")
            .unwrap(),
        AnthropicServerToolDefinition::mcp_connector("issues", "https://mcp.example.test/issues")
            .unwrap(),
    ])
    .unwrap();
    let option = plan.provider_option().unwrap();
    assert_eq!(option.data()["routes"].as_array().unwrap().len(), 1);
    assert_eq!(
        option.data()["routes"][0]["mcp_server_names"],
        serde_json::json!(["docs", "issues"])
    );
    assert_eq!(plan.tools_for(ANTHROPIC_CLAUDE_OPUS_5).unwrap().len(), 2);
}

#[test]
fn code_execution_definition_maps_both_real_subcall_routes() {
    let plan = AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::code_execution()])
        .unwrap();
    let option = plan.provider_option().unwrap();
    let routes = option.data()["routes"].as_array().unwrap();
    assert_eq!(routes.len(), 2);
    assert_eq!(
        routes
            .iter()
            .map(|route| route["provider_name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["bash_code_execution", "text_editor_code_execution"]
    );
    assert_eq!(
        routes[0]["result_types"],
        serde_json::json!(["bash_code_execution_tool_result"])
    );
    assert_eq!(
        routes[1]["result_types"],
        serde_json::json!(["text_editor_code_execution_tool_result"])
    );
}

#[test]
fn tool_search_requires_exact_deferred_companion_names() {
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::tool_search_regex()])
            .unwrap();
    assert_eq!(
        plan.tools_for(ANTHROPIC_CLAUDE_OPUS_5).unwrap_err(),
        AnthropicServerToolFault::InvalidConfiguration
    );
    let plan = plan
        .with_deferred_tools(vec!["get_weather".to_owned(), "search_files".to_owned()])
        .unwrap();
    assert_eq!(plan.tools_for(ANTHROPIC_CLAUDE_OPUS_5).unwrap().len(), 1);
    assert_eq!(
        plan.provider_option().unwrap().data()["deferred_tool_names"],
        serde_json::json!(["get_weather", "search_files"])
    );
}

#[test]
fn multi_call_classification_retains_state_and_uses_real_code_execution_names() {
    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::web_search(),
        AnthropicServerToolDefinition::code_execution(),
    ])
    .unwrap();
    let state = state(
        ANTHROPIC_CLAUDE_OPUS_5,
        vec![
            search_call("srvtoolu_search"),
            serde_json::json!({
                "type":"web_search_tool_result",
                "tool_use_id":"srvtoolu_search",
                "content":[{
                    "type":"web_search_result",
                    "url":"https://example.test/source",
                    "title":"Public title",
                    "encrypted_content":"opaque result material"
                }]
            }),
            serde_json::json!({
                "type":"server_tool_use",
                "id":"srvtoolu_code",
                "name":"bash_code_execution",
                "input":{"command":"printf private-output"}
            }),
            serde_json::json!({
                "type":"bash_code_execution_tool_result",
                "tool_use_id":"srvtoolu_code",
                "content":{
                    "type":"bash_code_execution_result",
                    "stdout":"private-output",
                    "stderr":"",
                    "return_code":0,
                    "content":[]
                }
            }),
            serde_json::json!({
                "type":"text",
                "text":"cited answer",
                "citations":[{
                    "type":"web_search_result_location",
                    "url":"https://example.test/source",
                    "title":"Public title",
                    "cited_text":"bounded excerpt",
                    "encrypted_index":"opaque citation material"
                }]
            }),
        ],
    );
    let classified = plan
        .classify(ANTHROPIC_CLAUDE_OPUS_5, state.clone())
        .unwrap();
    assert_eq!(classified.state(), &state);
    assert_eq!(classified.calls().len(), 2);
    assert_eq!(
        classified.calls()[0].call().id().as_str(),
        "srvtoolu_search"
    );
    assert_eq!(classified.calls()[0].call().provider_name(), "web_search");
    assert_eq!(
        classified.calls()[0].outcome(),
        AnthropicServerToolOutcome::Completed
    );
    assert_eq!(
        classified.calls()[0].result().unwrap().outcome(),
        ServerToolOutcome::Success
    );
    assert_eq!(
        classified.calls()[0].result().unwrap().output_count(),
        Some(1)
    );
    assert_eq!(
        classified.calls()[0].result().unwrap().sources()[0].url(),
        "https://example.test/source"
    );
    assert_eq!(
        classified.calls()[1].call().provider_name(),
        "bash_code_execution"
    );
    assert_eq!(classified.citations().len(), 1);
    assert_eq!(
        classified.citations()[0].url(),
        "https://example.test/source"
    );
    assert_eq!(
        classified.citations()[0].cited_text(),
        Some("bounded excerpt")
    );
    let rendered = format!("{classified:?}");
    for private in [
        "private query",
        "private-output",
        "opaque result material",
        "opaque citation material",
        "example.test",
    ] {
        assert!(!rendered.contains(private), "{rendered}");
    }
}

#[test]
fn web_citation_with_a_null_optional_title_normalizes_without_losing_exact_state() {
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let state = state(
        ANTHROPIC_CLAUDE_OPUS_5,
        vec![
            search_call("srvtoolu_nullable_title"),
            serde_json::json!({
                "type":"text",
                "text":"cited answer",
                "citations":[{
                    "type":"web_search_result_location",
                    "url":"https://example.test/source",
                    "title":serde_json::Value::Null,
                    "cited_text":"bounded excerpt",
                    "encrypted_index":"opaque citation material"
                }]
            }),
        ],
    );

    let classified = plan
        .classify(ANTHROPIC_CLAUDE_OPUS_5, state.clone())
        .unwrap();
    assert_eq!(classified.state(), &state);
    assert_eq!(
        classified.state().data()["content"][1]["citations"][0]["title"],
        serde_json::Value::Null
    );
    assert_eq!(classified.citations().len(), 1);
    assert_eq!(classified.citations()[0].title(), None);
}

#[test]
fn mcp_and_provider_error_results_classify_without_inventing_call_ids() {
    let mcp_plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::mcp_connector("docs", "https://mcp.example.test/sse")
            .unwrap(),
    ])
    .unwrap();
    let classified = mcp_plan
        .classify(
            ANTHROPIC_CLAUDE_OPUS_5,
            state(
                ANTHROPIC_CLAUDE_OPUS_5,
                vec![
                    serde_json::json!({
                        "type":"mcp_tool_use",
                        "id":"mcptoolu_real",
                        "name":"lookup",
                        "server_name":"docs",
                        "input":{"query":"private"}
                    }),
                    serde_json::json!({
                        "type":"mcp_tool_result",
                        "tool_use_id":"mcptoolu_real",
                        "is_error":true,
                        "content":[{"type":"text","text":"private remote failure"}]
                    }),
                ],
            ),
        )
        .unwrap();
    assert_eq!(classified.calls()[0].call().id().as_str(), "mcptoolu_real");
    assert_eq!(classified.calls()[0].call().logical(), "remote_mcp");
    assert_eq!(
        classified.calls()[0].outcome(),
        AnthropicServerToolOutcome::Failed
    );
    assert_eq!(
        classified.calls()[0].result().unwrap().error_code(),
        Some("provider_reported_error")
    );
    assert!(!format!("{classified:?}").contains("private remote failure"));

    let search_plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let classified = search_plan
        .classify(
            ANTHROPIC_CLAUDE_OPUS_5,
            state(
                ANTHROPIC_CLAUDE_OPUS_5,
                vec![
                    search_call("srvtoolu_real"),
                    serde_json::json!({
                        "type":"web_search_tool_result",
                        "tool_use_id":"srvtoolu_real",
                        "content":{
                            "type":"web_search_tool_result_error",
                            "error_code":"max_uses_exceeded"
                        }
                    }),
                ],
            ),
        )
        .unwrap();
    assert_eq!(classified.calls()[0].call().id().as_str(), "srvtoolu_real");
    assert_eq!(
        classified.calls()[0].result().unwrap().error_code(),
        Some("max_uses_exceeded")
    );
}

#[test]
fn pause_turn_exposes_exact_replay_state_and_required_configuration() {
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let state = state(
        ANTHROPIC_CLAUDE_OPUS_5,
        vec![search_call("srvtoolu_pending")],
    );
    let classified = plan
        .classify(ANTHROPIC_CLAUDE_OPUS_5, state.clone())
        .unwrap();
    let AnthropicServerToolContinuation::Pause(pause) =
        classified.continuation(FinishReason::Pause, &plan).unwrap()
    else {
        panic!("expected pause continuation")
    };
    assert_pause_state(&pause, &state);
    assert_eq!(
        pause.required_provider_option(),
        &plan.provider_option().unwrap()
    );
    match pause.replay_input() {
        InferenceInput::ProviderState(replayed) => assert_eq!(replayed, state),
        InferenceInput::Message(_) => panic!("pause replay must use exact provider state"),
    }
}

fn assert_pause_state(pause: &AnthropicPendingPauseState, state: &ProviderStateItem) {
    assert_eq!(pause.state(), state);
    assert_eq!(pause.pending_call_ids().len(), 1);
    assert_eq!(pause.pending_call_ids()[0].as_str(), "srvtoolu_pending");
    assert!(!format!("{pause:?}").contains("private query"));
}

#[test]
fn mixed_client_and_server_calls_wait_for_client_results_instead_of_claiming_pause() {
    let plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let state = state(
        ANTHROPIC_CLAUDE_OPUS_5,
        vec![
            search_call("srvtoolu_pending"),
            serde_json::json!({
                "type":"tool_use",
                "id":"toolu_client",
                "name":"run_command",
                "input":{"command":"uname"}
            }),
        ],
    );
    let classified = plan.classify(ANTHROPIC_CLAUDE_OPUS_5, state).unwrap();
    let AnthropicServerToolContinuation::AwaitingClientTools { pending_call_ids } = classified
        .continuation(FinishReason::ToolCalls, &plan)
        .unwrap()
    else {
        panic!("expected mixed client/server continuation")
    };
    assert_eq!(pending_call_ids[0].as_str(), "srvtoolu_pending");
    assert_eq!(
        classified
            .continuation(FinishReason::Pause, &plan)
            .unwrap_err(),
        AnthropicServerToolFault::InvalidState
    );
}

#[test]
fn historical_advisor_and_mcp_blocks_keep_their_beta_without_live_definitions() {
    let history = vec![InferenceInput::ProviderState(state(
        ANTHROPIC_CLAUDE_OPUS_5,
        vec![
            serde_json::json!({
                "type":"advisor_tool_result",
                "tool_use_id":"srvtoolu_old_advisor",
                "content":{
                    "type":"advisor_redacted_result",
                    "encrypted_content":"opaque"
                }
            }),
            serde_json::json!({
                "type":"mcp_tool_result",
                "tool_use_id":"mcptoolu_old_mcp",
                "is_error":false,
                "content":[]
            }),
        ],
    ))];
    assert_eq!(
        historical_server_tool_beta_headers(&history).unwrap(),
        vec!["advisor-tool-2026-03-01", "mcp-client-2025-11-20"]
    );
}

#[test]
fn orphan_duplicate_unadvertised_and_wrong_result_blocks_fail_closed() {
    let plan = AnthropicServerToolPlan::new(vec![
        AnthropicServerToolDefinition::web_search(),
        AnthropicServerToolDefinition::code_execution(),
    ])
    .unwrap();
    let fixtures = [
        vec![serde_json::json!({
            "type":"web_search_tool_result",
            "tool_use_id":"srvtoolu_orphan",
            "content":[]
        })],
        vec![
            search_call("srvtoolu_duplicate"),
            serde_json::json!({
                "type":"server_tool_use",
                "id":"srvtoolu_duplicate",
                "name":"bash_code_execution",
                "input":{"command":"true"}
            }),
        ],
        vec![serde_json::json!({
            "type":"server_tool_use",
            "id":"srvtoolu_unadvertised",
            "name":"advisor",
            "input":{}
        })],
        vec![
            serde_json::json!({
                "type":"server_tool_use",
                "id":"srvtoolu_code",
                "name":"bash_code_execution",
                "input":{"command":"true"}
            }),
            serde_json::json!({
                "type":"text_editor_code_execution_tool_result",
                "tool_use_id":"srvtoolu_code",
                "content":{"type":"text_editor_code_execution_view_result"}
            }),
        ],
    ];
    for content in fixtures {
        assert_eq!(
            plan.classify(
                ANTHROPIC_CLAUDE_OPUS_5,
                state(ANTHROPIC_CLAUDE_OPUS_5, content)
            )
            .unwrap_err(),
            AnthropicServerToolFault::InvalidState
        );
    }
}

#[test]
fn unsupported_unknown_and_wrong_route_refusals_are_distinct_and_body_free() {
    let fetch_plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_fetch()]).unwrap();
    assert_eq!(
        fetch_plan.tools_for(ANTHROPIC_CLAUDE_OPUS_5).unwrap_err(),
        AnthropicServerToolFault::UnsupportedCapability
    );
    assert_eq!(
        fetch_plan.tools_for("unknown-model").unwrap_err(),
        AnthropicServerToolFault::UnprovenCapability
    );
    let search_plan =
        AnthropicServerToolPlan::new(vec![AnthropicServerToolDefinition::web_search()]).unwrap();
    let foreign = ProviderStateItem::new(
        "foreign-provider",
        ANTHROPIC_CLAUDE_OPUS_5,
        ProviderProtocol::AnthropicMessages,
        ProviderStateKind::AnthropicMessage,
        serde_json::json!({
            "role":"assistant",
            "content":[search_call("srvtoolu_secret")]
        }),
    )
    .unwrap();
    let fault = search_plan
        .classify(ANTHROPIC_CLAUDE_OPUS_5, foreign)
        .unwrap_err();
    assert_eq!(fault, AnthropicServerToolFault::WrongRoute);
    assert!(!format!("{fault:?} {fault}").contains("srvtoolu_secret"));
}
