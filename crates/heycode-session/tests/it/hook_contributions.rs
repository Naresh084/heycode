//! O09 durable hook-contribution admission and replay contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    HookContributionEvent, HookContributionHandler, HookContributionPhase, HookContributionRecord,
    OpenError, Role, Session, SessionEventKind, derive_messages, project_inputs_for_route,
};

fn contribution(
    boundary: Option<heycode_core::UntrustedContentBoundary>,
) -> HookContributionRecord {
    HookContributionRecord::new(
        "fixture-owner",
        HookContributionPhase::Pre,
        HookContributionEvent::UserPrompt,
        HookContributionHandler::McpTool,
        boundary,
        "verified context",
    )
    .unwrap()
}

#[test]
fn contribution_is_v2_only_bounded_and_physically_replayable() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    let expected = contribution(Some(heycode_core::UntrustedContentBoundary::mcp()));
    let appended = session
        .append(SessionEventKind::HookContribution {
            contribution: Box::new(expected.clone()),
        })
        .unwrap();
    session.flush().unwrap();
    let directory = session.path().parent().unwrap().to_path_buf();

    let reopened = Session::open(directory).unwrap();
    assert_eq!(
        reopened.events()[appended.seq as usize].kind,
        SessionEventKind::HookContribution {
            contribution: Box::new(expected.clone()),
        }
    );

    let expected_text =
        heycode_core::UntrustedContentBoundary::mcp().render_for_model("verified context");
    let neutral = derive_messages(reopened.events());
    assert_eq!(neutral.len(), 1);
    assert_eq!(neutral[0].role, Role::User);
    assert_eq!(neutral[0].content, expected_text);

    let strict = project_inputs_for_route(
        reopened.events(),
        "provider",
        "model",
        heycode_core::ProviderProtocol::OpenAiChatCompletions,
    )
    .unwrap();
    let heycode_session::ProjectedInput::Message(message) = &strict[0] else {
        panic!("hook contribution must project as a neutral message");
    };
    assert_eq!(message.role, Role::User);
    assert_eq!(message.content, expected_text);
}

#[test]
fn invalid_provenance_and_text_are_refused_before_append() {
    assert!(
        HookContributionRecord::new(
            "",
            HookContributionPhase::Pre,
            HookContributionEvent::UserPrompt,
            HookContributionHandler::Prompt,
            None,
            "context",
        )
        .is_err()
    );
    assert!(
        HookContributionRecord::new(
            "owner",
            HookContributionPhase::Post,
            HookContributionEvent::Subagent,
            HookContributionHandler::Subagent,
            None,
            "x".repeat(64 * 1024 + 1),
        )
        .is_err()
    );
    assert!(!format!("{:?}", contribution(None)).contains("verified context"));
}

#[test]
fn v1_cannot_claim_hook_contributions() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("v1-hook");
    std::fs::create_dir_all(&directory).unwrap();
    let line = serde_json::json!({
        "v": 1,
        "seq": 0,
        "time_ms": 1,
        "kind": "hook/contribution",
        "data": {
            "contribution": {
                "owner": "fixture-owner",
                "phase": "pre",
                "event": "user_prompt",
                "handler": "prompt",
                "text": "context"
            }
        }
    });
    std::fs::write(directory.join("session.jsonl"), format!("{line}\n")).unwrap();
    assert!(matches!(
        Session::open(directory),
        Err(OpenError::UnknownKind { kind, .. }) if kind == "hook/contribution"
    ));
}

#[test]
fn caller_minted_session_id_is_create_new_and_matches_product_route_identity() {
    let root = tempfile::tempdir().unwrap();
    let id = heycode_core::SessionId::from_raw("product-session");
    let mut session = Session::create_with_id(root.path(), id.clone()).unwrap();
    assert_eq!(session.id(), &id);
    session
        .append(SessionEventKind::UserMessage {
            text: "keep".to_owned(),
        })
        .unwrap();
    session.flush().unwrap();
    assert!(Session::create_with_id(root.path(), id).is_err());
    let reopened = Session::open(root.path().join("product-session")).unwrap();
    assert!(matches!(
        &reopened.events()[0].kind,
        SessionEventKind::UserMessage { text } if text == "keep"
    ));
}
