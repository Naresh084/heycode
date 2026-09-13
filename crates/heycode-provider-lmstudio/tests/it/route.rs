//! Conservative agent-mode eligibility. Only proven tool training is offered,
//! and an unproven model is never described as incapable.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_llm::CapabilitySupport;
use heycode_provider_lmstudio::{
    LmStudioAgentEligibility, LmStudioAgentRefusal, LmStudioModelKind, LmStudioModelRecord,
    LmStudioRouteError, agent_capable_models, validate_agent_route,
};

/// One library row with only the fields eligibility reads.
fn record(
    key: &str,
    kind: LmStudioModelKind,
    tool_trained: CapabilitySupport,
) -> LmStudioModelRecord {
    LmStudioModelRecord {
        key: key.to_owned(),
        display_name: key.to_owned(),
        kind,
        publisher: None,
        architecture: None,
        quantization: None,
        size_bytes: None,
        params_string: None,
        format: None,
        max_context_length: None,
        loaded_instances: Vec::new(),
        tool_trained,
        vision: CapabilitySupport::Unknown,
        reasoning: CapabilitySupport::Unknown,
        reasoning_options: Vec::new(),
    }
}

fn llm(key: &str, tool_trained: CapabilitySupport) -> LmStudioModelRecord {
    record(key, LmStudioModelKind::Llm, tool_trained)
}

#[test]
fn only_a_proven_tool_trained_chat_model_is_eligible_for_agent_mode() {
    assert_eq!(
        llm("proven", CapabilitySupport::Supported).agent_eligibility(),
        LmStudioAgentEligibility::Eligible
    );
}

#[test]
fn an_unproven_model_is_refused_for_agent_mode() {
    // The conservative rule: Unknown is never promoted to eligible, because a
    // wrong guess costs a silently broken agent loop.
    let eligibility = llm("unproven", CapabilitySupport::Unknown).agent_eligibility();
    assert_eq!(
        eligibility,
        LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::ToolTrainingUnproven)
    );
    assert!(!eligibility.is_eligible());
}

#[test]
fn a_published_denial_is_refused_for_agent_mode() {
    assert_eq!(
        llm("denied", CapabilitySupport::Unsupported).agent_eligibility(),
        LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::NotToolTrained)
    );
}

#[test]
fn unproven_and_proven_incapable_are_different_refusals() {
    let unproven = llm("unproven", CapabilitySupport::Unknown)
        .agent_eligibility()
        .refusal()
        .unwrap();
    let denied = llm("denied", CapabilitySupport::Unsupported)
        .agent_eligibility()
        .refusal()
        .unwrap();
    assert_ne!(unproven, denied);
    assert_ne!(unproven.as_str(), denied.as_str());
    assert_ne!(
        unproven.to_string(),
        denied.to_string(),
        "flattening these two would tell a user their model cannot do tools when nothing said so"
    );
}

#[test]
fn only_a_published_denial_claims_the_capability_is_absent() {
    assert!(LmStudioAgentRefusal::NotToolTrained.is_published_denial());
    assert!(LmStudioAgentRefusal::EmbeddingModel.is_published_denial());
    assert!(
        !LmStudioAgentRefusal::ToolTrainingUnproven.is_published_denial(),
        "missing evidence is not a denial"
    );
    assert!(!LmStudioAgentRefusal::UnrecognizedModelKind.is_published_denial());
}

#[test]
fn an_unproven_refusal_never_claims_the_model_cannot_use_tools() {
    let message = LmStudioAgentRefusal::ToolTrainingUnproven.to_string();
    assert!(
        message.contains("unproven"),
        "the message must name the missing evidence: {message}"
    );
    assert!(
        !message.contains("was not trained"),
        "an unproven model must not be described with the published-denial wording: {message}"
    );
}

#[test]
fn an_embedding_model_is_refused_as_a_non_chat_model_not_as_untrained() {
    // Even with tool training published, an embedding model cannot chat.
    let embedding = record(
        "embed",
        LmStudioModelKind::Embedding,
        CapabilitySupport::Supported,
    );
    assert_eq!(
        embedding.agent_eligibility(),
        LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::EmbeddingModel)
    );
}

#[test]
fn an_unrecognized_model_kind_is_refused_distinctly_and_as_unproven() {
    let unrecognized = record(
        "legacy",
        LmStudioModelKind::Unrecognized("vlm".to_owned()),
        CapabilitySupport::Supported,
    );
    assert_eq!(
        unrecognized.agent_eligibility(),
        LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::UnrecognizedModelKind)
    );
    assert!(
        !LmStudioAgentRefusal::UnrecognizedModelKind.is_published_denial(),
        "an unknown type is unproven, not denied"
    );
}

#[test]
fn every_refusal_carries_one_safe_non_empty_line() {
    for refusal in [
        LmStudioAgentRefusal::NotToolTrained,
        LmStudioAgentRefusal::ToolTrainingUnproven,
        LmStudioAgentRefusal::EmbeddingModel,
        LmStudioAgentRefusal::UnrecognizedModelKind,
    ] {
        let message = refusal.to_string();
        assert!(!message.trim().is_empty(), "{refusal:?}");
        assert_eq!(message.lines().count(), 1, "{refusal:?}: {message}");
        assert!(!refusal.as_str().is_empty());
    }
}

#[test]
fn refusal_identifiers_are_stable_and_distinct() {
    let ids: Vec<&str> = [
        LmStudioAgentRefusal::NotToolTrained,
        LmStudioAgentRefusal::ToolTrainingUnproven,
        LmStudioAgentRefusal::EmbeddingModel,
        LmStudioAgentRefusal::UnrecognizedModelKind,
    ]
    .into_iter()
    .map(LmStudioAgentRefusal::as_str)
    .collect();
    assert_eq!(
        ids,
        vec![
            "not-tool-trained",
            "tool-training-unproven",
            "embedding-model",
            "unrecognized-model-kind",
        ]
    );
}

#[test]
fn an_eligible_model_carries_no_refusal() {
    assert_eq!(LmStudioAgentEligibility::Eligible.refusal(), None);
}

#[test]
fn only_eligible_rows_are_offered_for_agent_mode() {
    let library = vec![
        llm("proven", CapabilitySupport::Supported),
        llm("unproven", CapabilitySupport::Unknown),
        llm("denied", CapabilitySupport::Unsupported),
        record(
            "embed",
            LmStudioModelKind::Embedding,
            CapabilitySupport::Supported,
        ),
        record(
            "legacy",
            LmStudioModelKind::Unrecognized("vlm".to_owned()),
            CapabilitySupport::Supported,
        ),
        llm("also-proven", CapabilitySupport::Supported),
    ];
    let offered: Vec<&str> = agent_capable_models(&library)
        .into_iter()
        .map(|record| record.key.as_str())
        .collect();
    assert_eq!(offered, vec!["proven", "also-proven"]);
}

#[test]
fn a_withheld_model_is_still_a_usable_chat_model() {
    // Agent mode is withheld; chat is not.
    let unproven = llm("unproven", CapabilitySupport::Unknown);
    assert!(!unproven.is_agent_capable());
    assert!(
        unproven.is_chat_model(),
        "withholding agent mode must not make the model unusable for chat"
    );
}

#[test]
fn validating_an_absent_id_reports_an_unknown_model_not_a_refusal() {
    let library = vec![llm("proven", CapabilitySupport::Supported)];
    assert_eq!(
        validate_agent_route(&library, "missing").unwrap_err(),
        LmStudioRouteError::UnknownModel,
        "a model never seen is not a model judged incapable"
    );
}

#[test]
fn validating_an_ineligible_model_returns_its_exact_refusal() {
    let library = vec![
        llm("unproven", CapabilitySupport::Unknown),
        llm("denied", CapabilitySupport::Unsupported),
    ];
    assert_eq!(
        validate_agent_route(&library, "unproven").unwrap_err(),
        LmStudioRouteError::NotAgentCapable(LmStudioAgentRefusal::ToolTrainingUnproven)
    );
    assert_eq!(
        validate_agent_route(&library, "denied").unwrap_err(),
        LmStudioRouteError::NotAgentCapable(LmStudioAgentRefusal::NotToolTrained)
    );
}

#[test]
fn validating_an_eligible_model_returns_that_exact_row() {
    let library = vec![
        llm("unproven", CapabilitySupport::Unknown),
        llm("proven", CapabilitySupport::Supported),
    ];
    let route = validate_agent_route(&library, "proven").unwrap();
    assert_eq!(route.key, "proven");
}

#[test]
fn an_empty_library_offers_no_agent_route_at_all() {
    assert!(agent_capable_models(&[]).is_empty());
    assert_eq!(
        validate_agent_route(&[], "anything").unwrap_err(),
        LmStudioRouteError::UnknownModel
    );
}

#[test]
fn a_route_error_renders_its_underlying_refusal() {
    let error = LmStudioRouteError::NotAgentCapable(LmStudioAgentRefusal::ToolTrainingUnproven);
    assert_eq!(
        error.to_string(),
        LmStudioAgentRefusal::ToolTrainingUnproven.to_string()
    );
    assert_ne!(
        error.to_string(),
        LmStudioRouteError::UnknownModel.to_string()
    );
}

#[test]
fn eligibility_is_decided_by_the_exact_tri_state() {
    for (support, expected) in [
        (
            CapabilitySupport::Supported,
            LmStudioAgentEligibility::Eligible,
        ),
        (
            CapabilitySupport::Unsupported,
            LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::NotToolTrained),
        ),
        (
            CapabilitySupport::Unknown,
            LmStudioAgentEligibility::Refused(LmStudioAgentRefusal::ToolTrainingUnproven),
        ),
    ] {
        assert_eq!(
            llm("model", support).agent_eligibility(),
            expected,
            "{support:?} must map to exactly one eligibility"
        );
    }
}
