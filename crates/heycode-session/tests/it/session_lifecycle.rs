//! U15/CMD06 durable session lifecycle operations.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_session::{
    ForkBoundary, Session, SessionActivityStatus, SessionArchiveAction, SessionCreateRequest,
    SessionDeleteRequest, SessionExportFormat, SessionFilter, SessionLineageFilter, SessionQuery,
    SessionQueryError, SessionQueryService, SessionSource, SessionStorageFilter,
    SessionStorageState, SessionTitle,
};

fn metadata(source: SessionSource) -> heycode_session::SessionCreationMetadata {
    heycode_session::SessionCreationMetadata::new(
        Some(std::path::PathBuf::from("/work/project")),
        Some("native".to_owned()),
        source,
    )
    .unwrap()
}

fn only_summary(
    service: &SessionQueryService,
    filter: SessionFilter,
) -> heycode_session::SessionSummary {
    let page = service
        .query(&SessionQuery::new(filter, 20).unwrap())
        .unwrap();
    assert_eq!(page.items().len(), 1);
    page.items()[0].clone()
}

#[test]
fn create_rename_archive_restore_and_filters_keep_jsonl_and_lineage_intact() {
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let request = SessionCreateRequest::new(metadata(SessionSource::Interactive))
        .with_title(SessionTitle::new("Initial title").unwrap());
    let mut parent = service.create(&request).unwrap();
    parent
        .append(heycode_session::SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    parent
        .append(heycode_session::SessionEventKind::UserMessage {
            text: "keep this inherited prompt".to_owned(),
        })
        .unwrap();
    parent
        .append(heycode_session::SessionEventKind::TurnEnd {
            turn: 1,
            reason: heycode_session::TurnEndReason::Stop,
        })
        .unwrap();
    assert_eq!(
        service
            .rename_open(
                &mut parent,
                &SessionTitle::new("Current parent renamed").unwrap()
            )
            .unwrap()
            .title(),
        Some("Current parent renamed")
    );
    let parent_id = parent.id().clone();
    drop(parent);

    let child = service.fork(&parent_id, ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();
    drop(child);
    service
        .rename(&child_id, &SessionTitle::new("Renamed child").unwrap())
        .unwrap();

    let archived = service
        .archive(&child_id, SessionArchiveAction::Archive)
        .unwrap();
    assert_eq!(archived.storage(), SessionStorageState::Archived);
    assert_eq!(archived.title(), Some("Renamed child"));
    assert!(
        service
            .latest(&SessionFilter::new().with_text("Renamed child").unwrap())
            .unwrap()
            .is_none(),
        "the ordinary picker excludes archived sessions"
    );
    let archived_summary = only_summary(
        &service,
        SessionFilter::new()
            .with_storage(SessionStorageFilter::Archived)
            .with_lineage(SessionLineageFilter::Forks),
    );
    assert_eq!(archived_summary.id(), &child_id);
    assert_eq!(
        archived_summary.lineage().unwrap().parent_session_id(),
        &parent_id
    );

    let restored = service
        .archive(&child_id, SessionArchiveAction::Restore)
        .unwrap();
    assert_eq!(restored.storage(), SessionStorageState::Active);
    let reopened = service.resume(&child_id).unwrap();
    assert_eq!(
        heycode_session::derive_messages(reopened.events())[0].content,
        "keep this inherited prompt"
    );
    assert_eq!(reopened.lineage().unwrap().parent_session_id(), &parent_id);
}

#[test]
fn delete_is_confirmable_by_the_ui_but_service_safety_is_not_optional() {
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let parent = service
        .create(&SessionCreateRequest::new(metadata(
            SessionSource::Interactive,
        )))
        .unwrap();
    let parent_id = parent.id().clone();
    let child = service.fork(&parent_id, ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();

    assert_eq!(
        service.delete(&SessionDeleteRequest::new(parent_id.clone())),
        Err(SessionQueryError::HasDescendants)
    );
    assert_eq!(
        service.delete(&SessionDeleteRequest::new(child_id.clone()).with_current(child_id.clone())),
        Err(SessionQueryError::CurrentSession)
    );
    assert_eq!(
        service.delete(&SessionDeleteRequest::new(child_id.clone())),
        Err(SessionQueryError::OpenSession)
    );
    assert!(root.path().join(child_id.as_str()).is_dir());

    drop(child);
    let receipt = service
        .delete(&SessionDeleteRequest::new(child_id.clone()))
        .unwrap();
    assert_eq!(receipt.session_id(), &child_id);
    assert!(!root.path().join(child_id.as_str()).exists());
    assert!(service.resume(&child_id).is_err());

    service.restore_deleted(&receipt).unwrap();
    assert_eq!(service.resume(&child_id).unwrap().id(), &child_id);
    drop(parent);
}

#[test]
fn lossless_jsonl_export_copies_the_verified_lineage_bundle_and_markdown_is_human() {
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let mut parent = service
        .create(
            &SessionCreateRequest::new(metadata(SessionSource::Headless))
                .with_title(SessionTitle::new("Export parent").unwrap()),
        )
        .unwrap();
    parent
        .append(heycode_session::SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    parent
        .append(heycode_session::SessionEventKind::UserMessage {
            text: "exported question".to_owned(),
        })
        .unwrap();
    parent
        .append(heycode_session::SessionEventKind::AssistantMessage {
            turn: 1,
            step: 1,
            content: "exported answer".to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: None,
        })
        .unwrap();
    parent
        .append(heycode_session::SessionEventKind::TurnEnd {
            turn: 1,
            reason: heycode_session::TurnEndReason::Stop,
        })
        .unwrap();
    let parent_id = parent.id().clone();
    drop(parent);
    let child = service.fork(&parent_id, ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();
    drop(child);

    let jsonl = service
        .export(&child_id, SessionExportFormat::LosslessJsonl)
        .unwrap();
    assert!(jsonl.path().is_dir());
    assert_eq!(jsonl.format(), SessionExportFormat::LosslessJsonl);
    let exported_parent =
        std::fs::read(jsonl.path().join(parent_id.as_str()).join("session.jsonl")).unwrap();
    let original_parent =
        std::fs::read(root.path().join(parent_id.as_str()).join("session.jsonl")).unwrap();
    assert_eq!(exported_parent, original_parent);
    let exported_child = Session::open(jsonl.path().join(child_id.as_str())).unwrap();
    assert_eq!(exported_child.id(), &child_id);
    assert_eq!(
        heycode_session::derive_messages(exported_child.events())[0].content,
        "exported question"
    );

    let markdown = service
        .export(&child_id, SessionExportFormat::Markdown)
        .unwrap();
    let text = std::fs::read_to_string(markdown.path()).unwrap();
    assert!(text.contains("# Export parent"));
    assert!(text.contains("## User\n\nexported question"));
    assert!(text.contains("## Assistant\n\nexported answer"));
    assert!(!text.contains("session/created"));
}

#[test]
fn redacted_support_export_has_no_value_slot_for_session_content_or_host_paths() {
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let secret = "sk-proj-SUPPORT-BUNDLE-SECRET-CANARY";
    let mut session = service
        .create(
            &SessionCreateRequest::new(
                heycode_session::SessionCreationMetadata::new(
                    Some(std::path::PathBuf::from("/private/customer/workspace")),
                    Some("native".to_owned()),
                    SessionSource::Interactive,
                )
                .unwrap(),
            )
            .with_title(SessionTitle::new("Private customer title").unwrap()),
        )
        .unwrap();
    let id = session.id().clone();
    session
        .append(heycode_session::SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    session
        .append(heycode_session::SessionEventKind::UserMessage {
            text: format!("customer prompt {secret}"),
        })
        .unwrap();
    session
        .append(heycode_session::SessionEventKind::AssistantMessage {
            turn: 1,
            step: 1,
            content: "private answer".to_owned(),
            reasoning: Some("private reasoning".to_owned()),
            tool_calls: None,
            usage: Some(heycode_core::TokenUsage {
                prompt_tokens: 12,
                completion_tokens: 3,
            }),
        })
        .unwrap();
    session
        .append(heycode_session::SessionEventKind::TurnEnd {
            turn: 1,
            reason: heycode_session::TurnEndReason::Stop,
        })
        .unwrap();
    drop(session);

    let receipt = service
        .export(&id, SessionExportFormat::RedactedSupport)
        .unwrap();
    assert_eq!(receipt.format(), SessionExportFormat::RedactedSupport);
    let bytes = std::fs::read(receipt.path()).unwrap();
    let text = String::from_utf8(bytes.clone()).unwrap();
    for forbidden in [
        secret,
        "customer prompt",
        "private answer",
        "private reasoning",
        "Private customer title",
        "/private/customer/workspace",
        id.as_str(),
    ] {
        assert!(!text.contains(forbidden), "leaked {forbidden}: {text}");
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["schemaVersion"], 1);
    assert_eq!(value["redacted"], true);
    assert_eq!(value["eventCount"], 6);
    assert_eq!(value["events"][2]["kind"], "turn/start");
    assert_eq!(value["events"][4]["usage"]["promptTokens"], 12);
}

#[test]
fn lifecycle_inputs_and_reserved_store_entries_fail_loud_or_stay_out_of_queries() {
    assert!(SessionTitle::new("").is_err());
    assert!(SessionTitle::new("bad\u{1b}title").is_err());
    assert!(SessionTitle::new("x".repeat(201)).is_err());

    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let session = service
        .create(&SessionCreateRequest::new(metadata(
            SessionSource::Interactive,
        )))
        .unwrap();
    let id = session.id().clone();
    drop(session);
    service.archive(&id, SessionArchiveAction::Archive).unwrap();
    let all = service
        .query(
            &SessionQuery::new(
                SessionFilter::new().with_storage(SessionStorageFilter::All),
                20,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(all.items().len(), 1);
    assert_eq!(all.items()[0].status(), SessionActivityStatus::Empty);

    #[cfg(unix)]
    {
        std::fs::remove_file(root.path().join(id.as_str()).join(".archived")).unwrap();
        std::os::unix::fs::symlink(
            root.path().join(id.as_str()).join("session.jsonl"),
            root.path().join(id.as_str()).join(".archived"),
        )
        .unwrap();
        assert!(matches!(
            service.query(
                &SessionQuery::new(
                    SessionFilter::new().with_storage(SessionStorageFilter::All),
                    20,
                )
                .unwrap()
            ),
            Err(SessionQueryError::InvalidSession)
        ));
    }
}

#[test]
fn rename_normalizes_derives_and_disambiguates_without_changing_conversation() {
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let request = SessionCreateRequest::new(metadata(SessionSource::Interactive));
    let title = SessionTitle::from_input("  Fix\tlogin\nbug\u{1b}\u{202e}  ").unwrap();
    assert_eq!(title.as_str(), "Fix login bug");
    let spaced = SessionTitle::from_input("  Parity   command session  ").unwrap();
    assert_eq!(spaced.as_str(), "Parity   command session");
    let first = service
        .create(&request.clone().with_title(title.clone()))
        .unwrap();
    let first_id = first.id().clone();
    drop(first);
    let mut second = service.create(&request).unwrap();
    second
        .append(heycode_session::SessionEventKind::UserMessage {
            text: "Fix login bug".into(),
        })
        .unwrap();
    let generated = SessionTitle::from_conversation(second.events());
    assert_eq!(generated, title);
    let original = second.events().to_vec();
    assert_eq!(
        service
            .rename_open(&mut second, &generated)
            .unwrap()
            .title(),
        Some("Fix login bug (2)")
    );
    assert_eq!(
        service
            .rename_open(&mut second, &generated)
            .unwrap()
            .title(),
        Some("Fix login bug (2)")
    );
    assert_eq!(
        serde_json::to_value(&second.events()[..original.len()]).unwrap(),
        serde_json::to_value(original).unwrap()
    );
    assert_eq!(
        service.rename(&first_id, &title).unwrap().title(),
        Some("Fix login bug")
    );
    let third = service.create(&request).unwrap();
    let third_id = third.id().clone();
    drop(third);
    assert_eq!(
        service.rename(&third_id, &title).unwrap().title(),
        Some("Fix login bug (3)")
    );
    let long = SessionTitle::from_input(&"界".repeat(205)).unwrap();
    assert_eq!(long.as_str().chars().count(), 200);
    service.rename(&first_id, &long).unwrap();
    let renamed = service.rename(&third_id, &long).unwrap();
    assert_eq!(renamed.title().unwrap().chars().count(), 200);
    assert!(renamed.title().unwrap().ends_with(" (2)"));
    assert!(SessionTitle::from_input("\u{1b}\u{202e} ").is_err());
    assert_eq!(
        SessionTitle::from_conversation(&[]).as_str(),
        "Untitled session"
    );
}

#[test]
fn independent_store_owners_serialize_duplicate_rename_decisions() {
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let request = SessionCreateRequest::new(metadata(SessionSource::Interactive));
    let a = service.create(&request).unwrap();
    let b = service.create(&request).unwrap();
    let ids = [a.id().clone(), b.id().clone()];
    drop((a, b));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ids
        .into_iter()
        .map(|id| {
            let path = root.path().to_path_buf();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let service = SessionQueryService::local(path);
                barrier.wait();
                service
                    .rename(&id, &SessionTitle::new("Shared name").unwrap())
                    .unwrap()
                    .title()
                    .unwrap()
                    .to_owned()
            })
        })
        .collect();
    let mut titles: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    titles.sort();
    assert_eq!(titles, ["Shared name", "Shared name (2)"]);
}
