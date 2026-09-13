//! Replaceable JSONL-truth session query service contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{Context, ProviderProtocol, compose};
use heycode_session::{
    ForkBoundary, RequestAuthenticationSnapshot, RequestContextSnapshot, RequestHeaderSnapshot,
    RequestOptionsSnapshot, RequestTargetSnapshot, SERVICE_SESSION, SERVICE_SESSION_QUERY, Session,
    SessionActivityStatus, SessionCreationMetadata, SessionEventKind, SessionFilter, SessionQuery,
    SessionQueryBackend, SessionQueryError, SessionQueryService, SessionSource, TurnEndReason,
    session_query_jsonl_plugin, session_with_metadata_plugin,
};

fn metadata(cwd: &str, runtime: &str, source: SessionSource) -> SessionCreationMetadata {
    SessionCreationMetadata::new(
        Some(std::path::PathBuf::from(cwd)),
        Some(runtime.to_owned()),
        source,
    )
    .unwrap()
}

fn request_header(provider: &str) -> RequestHeaderSnapshot {
    RequestHeaderSnapshot::new(
        provider,
        "model",
        ProviderProtocol::OpenAiChatCompletions,
        RequestTargetSnapshot::Http {
            base_url: "https://example.test/v1".to_owned(),
        },
        RequestAuthenticationSnapshot::None,
        Some("system".to_owned()),
        Vec::new(),
        RequestOptionsSnapshot {
            input_modalities: vec!["text".to_owned()],
            reasoning_effort: None,
            defaulted_reasoning_effort: false,
            structured_output: None,
            native_features: Vec::new(),
            native_tool_routes: Vec::new(),
            provider_options: Vec::new(),
            temperature: None,
            max_output_tokens: None,
            defaulted_max_output_tokens: false,
            purpose: "conversation".to_owned(),
            retry: None,
        },
    )
    .unwrap()
}

struct SessionFixture<'a> {
    title: &'a str,
    cwd: &'a str,
    runtime: &'a str,
    source: SessionSource,
    prompt: Option<&'a str>,
    provider: Option<&'a str>,
    leave_open: bool,
}

fn create_session(root: &std::path::Path, fixture: SessionFixture<'_>) -> Session {
    let mut session =
        Session::create_with_metadata(root, metadata(fixture.cwd, fixture.runtime, fixture.source))
            .unwrap();
    session
        .append(SessionEventKind::SessionTitle {
            title: fixture.title.to_owned(),
        })
        .unwrap();
    if let Some(prompt) = fixture.prompt {
        session
            .append(SessionEventKind::TurnStart { turn: 1 })
            .unwrap();
        session
            .append(SessionEventKind::UserMessage {
                text: prompt.to_owned(),
            })
            .unwrap();
        if let Some(provider) = fixture.provider {
            let request_id = heycode_core::RequestId::generate();
            session
                .append(SessionEventKind::RequestHeader {
                    turn: 1,
                    step: 0,
                    request_id: request_id.clone(),
                    header: Box::new(request_header(provider)),
                })
                .unwrap();
            session
                .append(SessionEventKind::RequestContext {
                    request_id,
                    context: RequestContextSnapshot::new(None, None, None, None, 1).unwrap(),
                })
                .unwrap();
        }
        if !fixture.leave_open {
            session
                .append(SessionEventKind::TurnEnd {
                    turn: 1,
                    reason: TurnEndReason::Stop,
                })
                .unwrap();
        }
    }
    session
}

fn ids(page: &heycode_session::SessionPage) -> Vec<String> {
    page.items()
        .iter()
        .map(|summary| summary.id().as_str().to_owned())
        .collect()
}

struct RefusingBackend;

impl SessionQueryBackend for RefusingBackend {
    fn query(
        &self,
        _query: &SessionQuery,
    ) -> Result<heycode_session::SessionPage, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn resume(&self, _id: &heycode_core::SessionId) -> Result<Session, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn fork(
        &self,
        _id: &heycode_core::SessionId,
        _boundary: ForkBoundary,
    ) -> Result<Session, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn create(
        &self,
        _request: &heycode_session::SessionCreateRequest,
    ) -> Result<Session, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn rename(
        &self,
        _id: &heycode_core::SessionId,
        _title: &heycode_session::SessionTitle,
    ) -> Result<heycode_session::SessionSummary, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn rename_open(
        &self,
        _session: &mut Session,
        _title: &heycode_session::SessionTitle,
    ) -> Result<heycode_session::SessionSummary, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn archive(
        &self,
        _id: &heycode_core::SessionId,
        _action: heycode_session::SessionArchiveAction,
    ) -> Result<heycode_session::SessionSummary, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn delete(
        &self,
        _request: &heycode_session::SessionDeleteRequest,
    ) -> Result<heycode_session::SessionDeleteReceipt, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn restore_deleted(
        &self,
        _receipt: &heycode_session::SessionDeleteReceipt,
    ) -> Result<(), SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }

    fn export(
        &self,
        _id: &heycode_core::SessionId,
        _format: heycode_session::SessionExportFormat,
    ) -> Result<heycode_session::SessionExportReceipt, SessionQueryError> {
        Err(SessionQueryError::StoreUnavailable)
    }
}

#[test]
fn service_dispatches_through_a_replaceable_backend() {
    let service = SessionQueryService::new(std::sync::Arc::new(RefusingBackend));
    assert!(matches!(
        service.query(&SessionQuery::new(SessionFilter::new(), 10).unwrap()),
        Err(SessionQueryError::StoreUnavailable)
    ));
    assert!(matches!(
        service.stats(),
        Err(SessionQueryError::StatisticsUnavailable)
    ));
}

#[test]
fn stats_scan_counts_physical_suffixes_archives_and_telemetry_without_fork_inflation() {
    let root = tempfile::tempdir().unwrap();
    let mut parent = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    parent
        .append(SessionEventKind::TurnStart { turn: 1 })
        .unwrap();
    parent
        .append(SessionEventKind::UserMessage {
            text: "parent prompt".to_owned(),
        })
        .unwrap();
    let parent_request = heycode_core::RequestId::from_raw("stats_parent_request");
    parent
        .append(SessionEventKind::RequestHeader {
            turn: 1,
            step: 0,
            request_id: parent_request.clone(),
            header: Box::new(request_header("provider-a")),
        })
        .unwrap();
    parent
        .append(SessionEventKind::AssistantMessage {
            turn: 1,
            step: 0,
            content: "parent answer".to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: Some(heycode_core::TokenUsage {
                prompt_tokens: 10,
                completion_tokens: 2,
            }),
        })
        .unwrap();
    parent
        .append(SessionEventKind::AssistantResponseMetadata {
            turn: 1,
            step: 0,
            request_id: parent_request,
            metadata: Box::new(
                heycode_core::ProviderResponseMetadata::new(
                    Some(heycode_core::ProviderCacheUsage::new(10, 2, 4, 3).unwrap()),
                    Vec::new(),
                    None,
                )
                .unwrap(),
            ),
        })
        .unwrap();
    parent
        .append(SessionEventKind::TurnEnd {
            turn: 1,
            reason: TurnEndReason::Stop,
        })
        .unwrap();
    let parent_id = parent.id().clone();
    let mut child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    child
        .append(SessionEventKind::TurnStart { turn: 2 })
        .unwrap();
    child
        .append(SessionEventKind::UserMessage {
            text: "child prompt".to_owned(),
        })
        .unwrap();
    child
        .append(SessionEventKind::RequestHeader {
            turn: 2,
            step: 0,
            request_id: heycode_core::RequestId::from_raw("stats_child_request"),
            header: Box::new(request_header("provider-a")),
        })
        .unwrap();
    child
        .append(SessionEventKind::AssistantMessage {
            turn: 2,
            step: 0,
            content: "child answer".to_owned(),
            reasoning: None,
            tool_calls: None,
            usage: Some(heycode_core::TokenUsage {
                prompt_tokens: 5,
                completion_tokens: 1,
            }),
        })
        .unwrap();
    child
        .append(SessionEventKind::TurnEnd {
            turn: 2,
            reason: TurnEndReason::Stop,
        })
        .unwrap();
    let child_id = child.id().clone();
    drop((parent, child));

    let service = SessionQueryService::local(root.path().to_path_buf());
    service
        .archive(&parent_id, heycode_session::SessionArchiveAction::Archive)
        .unwrap();
    let stats = service.stats().unwrap();
    assert_eq!(stats.readable_sessions(), 2);
    assert_eq!(stats.unreadable_sessions(), 0);
    assert_eq!(stats.used_sessions(), 2);
    assert_eq!(stats.active_used_sessions(), 1);
    assert_eq!(stats.archived_used_sessions(), 1);
    assert_eq!(stats.user_messages(), 2);
    assert_eq!(stats.assistant_messages(), 2);
    assert_eq!(stats.reported_input_tokens(), 15);
    assert_eq!(stats.reported_output_tokens(), 3);
    assert_eq!(stats.cache_read_tokens(), 4);
    assert_eq!(stats.cache_write_tokens(), 3);
    assert_eq!(stats.cache_reports(), 1);
    assert_eq!(stats.unreported_assistant_messages(), 0);
    assert_eq!(stats.unattributed_assistant_messages(), 0);
    assert_eq!(stats.models().len(), 1);
    assert_eq!(stats.models()[0].provider(), "provider-a");
    assert_eq!(stats.models()[0].model(), "model");
    assert_eq!(stats.models()[0].responses(), 2);
    assert_eq!(stats.active_days(), 1);
    assert_eq!(stats.days()[0].sessions(), 2);

    service
        .delete(&heycode_session::SessionDeleteRequest::new(child_id))
        .unwrap();
    let after_delete = service.stats().unwrap();
    assert_eq!(after_delete.used_sessions(), 1);
    assert_eq!(after_delete.archived_used_sessions(), 1);
    assert_eq!(after_delete.user_messages(), 1);
    assert_eq!(after_delete.reported_input_tokens(), 10);
    assert_eq!(after_delete.cache_read_tokens(), 4);

    let broken_id = "00000000-0000-4000-8000-00000000cafe";
    let broken = root.path().join(broken_id);
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join("session.jsonl"), "{broken\n").unwrap();
    let partial = service.stats().unwrap();
    assert_eq!(partial.used_sessions(), 1);
    assert_eq!(partial.unreadable_sessions(), 1);
    assert_eq!(partial.reported_input_tokens(), 10);
}

#[test]
fn stats_empty_means_no_used_or_unreadable_sessions() {
    let root = tempfile::tempdir().unwrap();
    let session = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    drop(session);
    let stats = SessionQueryService::local(root.path().to_path_buf())
        .stats()
        .unwrap();
    assert!(stats.is_empty());
    assert_eq!(stats.readable_sessions(), 1);
    assert_eq!(stats.used_sessions(), 0);
    assert_eq!(stats.total_messages(), 0);
}

#[test]
fn metadata_session_plugin_commits_creation_facts_before_publication() {
    let root = tempfile::tempdir().unwrap();
    let plugins = vec![session_with_metadata_plugin(
        root.path().to_path_buf(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )];
    let mut context = compose(&plugins).unwrap();
    let session = context
        .get::<std::sync::Mutex<Session>>(SERVICE_SESSION)
        .unwrap();
    let guard = session.lock().unwrap();
    assert_eq!(guard.events().len(), 1);
    assert!(guard.is_fresh());
    assert_eq!(guard.events()[0].kind.name(), "session/created");
    assert_eq!(
        guard.metadata().unwrap().cwd().unwrap(),
        std::path::Path::new("/work/project")
    );
    drop(guard);
    context.shutdown();
}

#[test]
fn exact_title_filters_are_bound_to_the_cursor() {
    let root = tempfile::tempdir().unwrap();
    for title in ["Exact title", "Exact title", "Exact title extra"] {
        drop(create_session(
            root.path(),
            SessionFixture {
                title,
                cwd: "/work/project",
                runtime: "native",
                source: SessionSource::Interactive,
                prompt: None,
                provider: None,
                leave_open: false,
            },
        ));
    }
    let service = SessionQueryService::local(root.path().to_path_buf());
    let filter = SessionFilter::new()
        .with_exact_title("Exact title")
        .unwrap();
    let first = service
        .query(&SessionQuery::new(filter.clone(), 1).unwrap())
        .unwrap();
    assert_eq!(first.total_matches(), 2);
    let cursor = first.next_cursor().unwrap().clone();
    let second = service
        .query(
            &SessionQuery::new(filter, 1)
                .unwrap()
                .with_cursor(cursor.clone()),
        )
        .unwrap();
    assert_eq!(second.total_matches(), 2);
    assert_ne!(first.items()[0].id(), second.items()[0].id());
    let changed = SessionQuery::new(
        SessionFilter::new()
            .with_exact_title("Exact title extra")
            .unwrap(),
        1,
    )
    .unwrap()
    .with_cursor(cursor);
    assert!(matches!(
        service.query(&changed),
        Err(SessionQueryError::InvalidCursor)
    ));
}

#[test]
fn deterministic_keyset_pagination_matches_one_complete_query() {
    let root = tempfile::tempdir().unwrap();
    for (index, title) in ["one", "two", "three", "four"].into_iter().enumerate() {
        let session = create_session(
            root.path(),
            SessionFixture {
                title,
                cwd: "/work/project",
                runtime: "native",
                source: SessionSource::Interactive,
                prompt: Some(&format!("prompt {index}")),
                provider: Some("provider"),
                leave_open: false,
            },
        );
        drop(session);
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let service = SessionQueryService::local(root.path().to_path_buf());
    let all = service
        .query(&SessionQuery::new(SessionFilter::new(), 100).unwrap())
        .unwrap();
    assert_eq!(all.items().len(), 4);

    let first = service
        .query(&SessionQuery::new(SessionFilter::new(), 2).unwrap())
        .unwrap();
    assert_eq!(first.total_matches(), 4);
    let second = service
        .query(
            &SessionQuery::new(SessionFilter::new(), 2)
                .unwrap()
                .with_cursor(first.next_cursor().unwrap().clone()),
        )
        .unwrap();
    assert_eq!(second.total_matches(), 4);
    assert!(second.next_cursor().is_none());
    let mut paged = ids(&first);
    paged.extend(ids(&second));
    assert_eq!(paged, ids(&all));
    assert!(all.items().windows(2).all(|pair| {
        pair[0].last_activity_ms() > pair[1].last_activity_ms()
            || (pair[0].last_activity_ms() == pair[1].last_activity_ms()
                && pair[0].id().as_str() < pair[1].id().as_str())
    }));
}

#[test]
fn equal_activity_timestamps_use_session_id_as_the_stable_keyset_tiebreaker() {
    let root = tempfile::tempdir().unwrap();
    let mut expected = Vec::new();
    for title in ["alpha", "beta"] {
        let session = create_session(
            root.path(),
            SessionFixture {
                title,
                cwd: "/work/project",
                runtime: "native",
                source: SessionSource::Interactive,
                prompt: None,
                provider: None,
                leave_open: false,
            },
        );
        expected.push(session.id().as_str().to_owned());
        let path = session.path().to_path_buf();
        drop(session);
        let rewritten = std::fs::read_to_string(&path)
            .unwrap()
            .lines()
            .map(|line| {
                let mut value: serde_json::Value = serde_json::from_str(line).unwrap();
                value["time_ms"] = serde_json::json!(100);
                value.to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        std::fs::write(path, rewritten).unwrap();
    }
    expected.sort();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let first = service
        .query(&SessionQuery::new(SessionFilter::new(), 1).unwrap())
        .unwrap();
    let second = service
        .query(
            &SessionQuery::new(SessionFilter::new(), 1)
                .unwrap()
                .with_cursor(first.next_cursor().unwrap().clone()),
        )
        .unwrap();
    let mut actual = ids(&first);
    actual.extend(ids(&second));
    assert_eq!(actual, expected);
}

#[test]
fn filters_cover_text_provider_runtime_source_status_cwd_and_lineage() {
    let root = tempfile::tempdir().unwrap();
    let parent = create_session(
        root.path(),
        SessionFixture {
            title: "Unsafe\n\u{1b}[31m Rust session",
            cwd: "/work/a",
            runtime: "native",
            source: SessionSource::Interactive,
            prompt: Some("needle in transcript"),
            provider: Some("deepseek"),
            leave_open: false,
        },
    );
    let parent_id = parent.id().clone();
    let child = parent.fork(root.path(), ForkBoundary::Latest).unwrap();
    let child_id = child.id().clone();
    drop(child);
    drop(parent);
    let open = create_session(
        root.path(),
        SessionFixture {
            title: "Open",
            cwd: "/work/b",
            runtime: "delegated-agent",
            source: SessionSource::Delegated,
            prompt: Some("other"),
            provider: Some("anthropic"),
            leave_open: true,
        },
    );
    let open_id = open.id().clone();
    drop(open);
    let empty = create_session(
        root.path(),
        SessionFixture {
            title: "Empty",
            cwd: "/work/c",
            runtime: "native",
            source: SessionSource::Headless,
            prompt: None,
            provider: None,
            leave_open: false,
        },
    );
    let empty_id = empty.id().clone();
    drop(empty);

    let service = SessionQueryService::local(root.path().to_path_buf());
    let cases = [
        (
            SessionFilter::new().with_text("NEEDLE").unwrap(),
            vec![child_id.clone(), parent_id.clone()],
        ),
        (
            SessionFilter::new().with_provider("anthropic").unwrap(),
            vec![open_id.clone()],
        ),
        (
            SessionFilter::new()
                .with_runtime("delegated-agent")
                .unwrap(),
            vec![open_id.clone()],
        ),
        (
            SessionFilter::new().with_source(SessionSource::Headless),
            vec![empty_id.clone()],
        ),
        (
            SessionFilter::new().with_status(SessionActivityStatus::OpenTurn),
            vec![open_id.clone()],
        ),
        (
            SessionFilter::new()
                .with_cwd(std::path::PathBuf::from("/work/c"))
                .unwrap(),
            vec![empty_id.clone()],
        ),
        (
            SessionFilter::new().with_parent(parent_id.clone()).unwrap(),
            vec![child_id.clone()],
        ),
    ];
    for (filter, expected) in cases {
        let page = service
            .query(&SessionQuery::new(filter, 100).unwrap())
            .unwrap();
        let found = page
            .items()
            .iter()
            .map(|summary| summary.id().clone())
            .collect::<Vec<_>>();
        assert_eq!(found, expected);
    }

    let parent_summary = service
        .query(
            &SessionQuery::new(
                SessionFilter::new().with_parent(parent_id.clone()).unwrap(),
                10,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(parent_summary.items()[0].id(), &child_id);
    assert!(
        parent_summary.items()[0].event_count() > parent_summary.items()[0].local_event_count()
    );
    assert_eq!(
        parent_summary.items()[0]
            .lineage()
            .unwrap()
            .parent_session_id(),
        &parent_id
    );
    let safe_title = service
        .resume(&parent_id)
        .and_then(|_| service.latest(&SessionFilter::new().with_text("Rust").unwrap()))
        .unwrap()
        .unwrap()
        .title()
        .unwrap()
        .to_owned();
    assert!(!safe_title.contains('\n'));
    assert!(!safe_title.contains('\u{1b}'));
}

#[test]
fn latest_resume_and_cursor_filter_binding_are_fail_loud() {
    let root = tempfile::tempdir().unwrap();
    let older = create_session(
        root.path(),
        SessionFixture {
            title: "older",
            cwd: "/work/a",
            runtime: "native",
            source: SessionSource::Interactive,
            prompt: Some("alpha"),
            provider: None,
            leave_open: false,
        },
    );
    let older_id = older.id().clone();
    drop(older);
    std::thread::sleep(std::time::Duration::from_millis(2));
    let newer = create_session(
        root.path(),
        SessionFixture {
            title: "newer",
            cwd: "/work/a",
            runtime: "native",
            source: SessionSource::Interactive,
            prompt: Some("beta"),
            provider: None,
            leave_open: false,
        },
    );
    let newer_id = newer.id().clone();
    drop(newer);

    let service = SessionQueryService::local(root.path().to_path_buf());
    assert_eq!(
        service.latest(&SessionFilter::new()).unwrap().unwrap().id(),
        &newer_id
    );
    assert_eq!(service.resume(&older_id).unwrap().id(), &older_id);
    assert!(matches!(
        service.resume(&heycode_core::SessionId::from_raw("missing")),
        Err(SessionQueryError::SessionNotFound)
    ));
    let forked = service.fork(&older_id, ForkBoundary::Latest).unwrap();
    assert_eq!(forked.lineage().unwrap().parent_session_id(), &older_id);
    drop(forked);

    let first = service
        .query(&SessionQuery::new(SessionFilter::new(), 1).unwrap())
        .unwrap();
    let mismatched = SessionQuery::new(SessionFilter::new().with_text("alpha").unwrap(), 1)
        .unwrap()
        .with_cursor(first.next_cursor().unwrap().clone());
    assert!(matches!(
        service.query(&mismatched),
        Err(SessionQueryError::InvalidCursor)
    ));
    assert!(SessionQuery::new(SessionFilter::new(), 0).is_err());
    assert!(SessionQuery::new(SessionFilter::new(), 101).is_err());
    assert!(SessionFilter::new().with_text("bad\u{1b}query").is_err());
    assert!(SessionFilter::new().with_runtime("Not-Kebab").is_err());
    assert!(
        SessionFilter::new()
            .with_cwd(std::path::PathBuf::from("relative"))
            .is_err()
    );
    assert!(
        SessionFilter::new()
            .with_parent(heycode_core::SessionId::from_raw("../escape"))
            .is_err()
    );
    assert!(
        SessionFilter::new()
            .with_parent(heycode_core::SessionId::from_raw("CON"))
            .is_err()
    );
}

#[test]
fn query_plugin_is_effect_owned_and_held_service_stops_after_shutdown() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join(".heycode-fork-stale.tmp")).unwrap();
    let plugins = vec![session_query_jsonl_plugin(root.path().to_path_buf())];
    let mut context: Context = compose(&plugins).unwrap();
    assert_eq!(
        context.owner_of(SERVICE_SESSION_QUERY),
        Some("session-query-jsonl")
    );
    let service = context
        .get::<SessionQueryService>(SERVICE_SESSION_QUERY)
        .unwrap();
    assert!(
        service
            .query(&SessionQuery::new(SessionFilter::new(), 10).unwrap())
            .is_ok()
    );
    context.shutdown();
    assert!(matches!(
        service.query(&SessionQuery::new(SessionFilter::new(), 10).unwrap()),
        Err(SessionQueryError::ServiceStopped)
    ));
    assert!(matches!(
        service.create(&heycode_session::SessionCreateRequest::new(metadata(
            "/work/project",
            "native",
            SessionSource::Interactive,
        ))),
        Err(SessionQueryError::ServiceStopped)
    ));
}

#[cfg(unix)]
#[test]
fn symlinked_session_entry_is_not_followed() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let session = create_session(
        outside.path(),
        SessionFixture {
            title: "outside",
            cwd: "/work/a",
            runtime: "native",
            source: SessionSource::Interactive,
            prompt: None,
            provider: None,
            leave_open: false,
        },
    );
    symlink(session.path().parent().unwrap(), root.path().join("linked")).unwrap();
    drop(session);

    let service = SessionQueryService::local(root.path().to_path_buf());
    assert!(matches!(
        service.query(&SessionQuery::new(SessionFilter::new(), 10).unwrap()),
        Err(SessionQueryError::InvalidSession)
    ));
}

#[test]
fn legacy_empty_session_reports_unknown_metadata_without_inventing_facts() {
    let root = tempfile::tempdir().unwrap();
    let session = Session::create(root.path()).unwrap();
    let id = session.id().clone();
    drop(session);
    let service = SessionQueryService::local(root.path().to_path_buf());
    let page = service
        .query(&SessionQuery::new(SessionFilter::new(), 10).unwrap())
        .unwrap();
    let summary = page
        .items()
        .iter()
        .find(|summary| summary.id() == &id)
        .unwrap();
    assert_eq!(summary.status(), SessionActivityStatus::Empty);
    assert!(summary.title().is_none());
    assert!(summary.cwd().is_none());
    assert!(summary.runtime().is_none());
    assert!(summary.source().is_none());
    assert!(summary.provider().is_none());
    assert!(summary.created_at_ms().is_none());
    assert!(summary.last_activity_ms().is_none());
}

#[test]
fn corrupt_session_failure_is_fixed_and_does_not_echo_log_or_query_text() {
    let root = tempfile::tempdir().unwrap();
    let session = create_session(
        root.path(),
        SessionFixture {
            title: "PRIVATE-CANARY",
            cwd: "/work/a",
            runtime: "native",
            source: SessionSource::Interactive,
            prompt: Some("secret transcript"),
            provider: None,
            leave_open: false,
        },
    );
    let path = session.path().to_path_buf();
    let id = session.id().clone();
    drop(session);
    std::fs::write(path, "{broken\n").unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    // A corrupt log is one unreadable row: the listing succeeds, and nothing
    // of the damaged log's content — not even its title — is surfaced from it.
    let listed = service
        .query(&SessionQuery::new(SessionFilter::new(), 10).unwrap())
        .unwrap();
    let row = listed
        .items()
        .iter()
        .find(|summary| summary.id() == &id)
        .expect("the damaged session is listed");
    assert!(!row.is_readable());
    assert!(
        row.title().is_none(),
        "no title is read out of a corrupt log"
    );
    // A text filter cannot match content inside it, and the error for
    // targeting it is fixed text that echoes neither log nor query.
    let query = SessionQuery::new(
        SessionFilter::new().with_text("secret transcript").unwrap(),
        10,
    )
    .unwrap();
    assert!(service.query(&query).unwrap().items().is_empty());
    let error = service
        .resume(&id)
        .err()
        .expect("a corrupt session is refused as a target");
    assert_eq!(error, SessionQueryError::InvalidSession);
    let rendered = error.to_string();
    assert!(!rendered.contains("PRIVATE-CANARY"));
    assert!(!rendered.contains("secret transcript"));
    assert!(!rendered.contains(root.path().to_string_lossy().as_ref()));
}

#[test]
fn one_session_this_build_cannot_project_does_not_deny_the_whole_store() {
    let root = tempfile::tempdir().unwrap();
    let mut healthy = Session::create(root.path()).unwrap();
    healthy
        .append(SessionEventKind::SessionTitle {
            title: "still resumable".to_owned(),
        })
        .unwrap();
    let healthy_id = healthy.id().clone();
    drop(healthy);

    // Well-formed JSONL that `Session::open` still refuses: two lines claim
    // the same sequence, the exact shape two concurrent writers used to leave
    // behind. One such log must not hide every healthy session in the store.
    let broken_id = "00000000-0000-4000-8000-00000000beef";
    let broken = root.path().join(broken_id);
    std::fs::create_dir(&broken).unwrap();
    let line = |seq: u64| {
        format!(
            "{}\n",
            serde_json::json!({
                "v": 2, "seq": seq, "time_ms": 1_730_000_000_000_i64,
                "kind": "user/message", "data": {"text": "duplicated"}
            })
        )
    };
    std::fs::write(
        broken.join("session.jsonl"),
        format!("{}{}", line(0), line(0)),
    )
    .unwrap();

    let service = SessionQueryService::local(root.path().to_path_buf());
    let page = service
        .query(&SessionQuery::new(SessionFilter::new(), 20).unwrap())
        .unwrap();
    assert_eq!(page.items().len(), 2);
    let listed = page
        .items()
        .iter()
        .find(|summary| summary.id().as_str() == broken_id)
        .unwrap();
    assert!(!listed.is_readable());
    assert!(listed.title().is_none(), "no fact is invented for it");

    // `heycode --continue` still lands on the newest session it can actually open.
    let latest = service.latest(&SessionFilter::new()).unwrap().unwrap();
    assert_eq!(latest.id(), &healthy_id);
    assert!(latest.is_readable());
    assert!(service.resume(&healthy_id).is_ok());
    match service.resume(&heycode_core::SessionId::from_raw(broken_id)) {
        Err(error) => assert_eq!(
            error,
            SessionQueryError::InvalidSession,
            "an unreadable session is still refused loudly when it is the target"
        ),
        Ok(_) => panic!("an unopenable session must not resume"),
    }
}

#[test]
fn resuming_through_the_query_backend_never_takes_the_writer_lease() {
    let root = tempfile::tempdir().unwrap();
    let session = Session::create(root.path()).unwrap();
    let id = session.id().clone();
    let directory = session.path().parent().unwrap().to_path_buf();
    drop(session);

    // The backend hands out a deliberately UNLEASED handle: `fork` is built on
    // `resume` and forking the live session is a product feature, and the TUI
    // opens one as a probe. So the backend can never observe
    // `OpenError::AlreadyOpen`, and resuming must leave the process's real
    // writer — the one `session-resume` leased — still current.
    let mut writer = Session::open_for_writing(&directory).unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let resumed = service.resume(&id).unwrap();
    assert!(resumed.events().is_empty());
    writer
        .append(SessionEventKind::SessionTitle {
            title: "still the writer".to_owned(),
        })
        .unwrap();
}

/// A crash in the middle of an append leaves an unterminated last line. That
/// is the commonest damage a real store carries, and it is damage to ONE log:
/// it must list as an unreadable row while every other session, `--continue`
/// and the picker keep working.
#[test]
fn a_truncated_log_is_one_unreadable_row_not_a_dead_store() {
    let root = tempfile::tempdir().unwrap();
    let mut healthy = Session::create(root.path()).unwrap();
    healthy
        .append(SessionEventKind::SessionTitle {
            title: "still resumable".to_owned(),
        })
        .unwrap();
    let healthy_id = healthy.id().clone();
    drop(healthy);

    let mut victim = Session::create(root.path()).unwrap();
    victim
        .append(SessionEventKind::UserMessage {
            text: "about to be cut off".to_owned(),
        })
        .unwrap();
    let victim_id = victim.id().clone();
    let victim_log = victim.path().to_path_buf();
    drop(victim);
    let bytes = std::fs::read(&victim_log).unwrap();
    std::fs::write(&victim_log, &bytes[..bytes.len() - 40]).unwrap();

    let service = SessionQueryService::local(root.path().to_path_buf());
    let page = service
        .query(&SessionQuery::new(SessionFilter::new(), 20).unwrap())
        .expect("one truncated log must not deny the whole store");
    assert_eq!(page.items().len(), 2);
    let listed = page
        .items()
        .iter()
        .find(|summary| summary.id() == &victim_id)
        .unwrap();
    assert!(!listed.is_readable());
    let latest = service.latest(&SessionFilter::new()).unwrap().unwrap();
    assert_eq!(
        latest.id(),
        &healthy_id,
        "--continue lands on the newest openable session"
    );
    assert!(
        service.resume(&victim_id).is_err(),
        "the damaged log is still refused as a target"
    );
}

/// A directory the product itself creates under `sessions/` for another
/// purpose — or any stray non-session directory — is not a session and is
/// simply not listed. Only unsafe entries (symlinks) fail the scan.
#[test]
fn a_stray_non_session_directory_does_not_break_the_listing() {
    let root = tempfile::tempdir().unwrap();
    let session = Session::create(root.path()).unwrap();
    let id = session.id().clone();
    drop(session);
    std::fs::create_dir(root.path().join("reviews")).unwrap();
    std::fs::write(root.path().join("reviews").join("note.txt"), "x").unwrap();

    let service = SessionQueryService::local(root.path().to_path_buf());
    let page = service
        .query(&SessionQuery::new(SessionFilter::new(), 20).unwrap())
        .expect("a stray directory is skipped, not fatal");
    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].id(), &id);
}

#[test]
fn conversation_names_are_derived_for_old_logs_and_manual_names_win() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create_with_metadata(
        root.path(),
        metadata("/work/project", "native", SessionSource::Interactive),
    )
    .unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: "  Fix login redirects\nMore detailed requirements follow".into(),
        })
        .unwrap();
    let id = session.id().clone();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let list = || {
        service
            .query(&SessionQuery::new(SessionFilter::new(), 10).unwrap())
            .unwrap()
    };
    assert_eq!(list().items()[0].title(), Some("Fix login redirects"));
    service
        .rename_open(
            &mut session,
            &heycode_session::SessionTitle::new("Authentication repair").unwrap(),
        )
        .unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: "Now add a regression test".into(),
        })
        .unwrap();
    drop(session);
    let page = list();
    assert_eq!(page.items()[0].title(), Some("Authentication repair"));
    assert_eq!(page.items()[0].id(), &id);
    assert_eq!(
        service
            .query(
                &SessionQuery::new(SessionFilter::new().with_text("regression").unwrap(), 10)
                    .unwrap()
            )
            .unwrap()
            .total_matches(),
        1
    );
}

#[test]
fn activation_is_durable_but_never_enters_the_model_request() {
    let root = tempfile::tempdir().unwrap();
    let mut session = Session::create(root.path()).unwrap();
    session
        .append(SessionEventKind::UserMessage {
            text: "Explain the build".into(),
        })
        .unwrap();
    let before = heycode_session::derive_messages(session.events());
    session
        .append(SessionEventKind::SessionActivated {})
        .unwrap();
    let path = session.path().to_path_buf();
    drop(session);
    let resumed = Session::open(path.parent().unwrap()).unwrap();
    assert!(matches!(
        resumed.events().last().unwrap().kind,
        SessionEventKind::SessionActivated {}
    ));
    let after = heycode_session::derive_messages(resumed.events());
    assert_eq!(before, after);
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].content, "Explain the build");
}
