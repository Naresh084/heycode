//! U15/CMD06 session browser, actions and typed recomposition outcomes.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use heycode_session::{
    SessionCreateRequest, SessionCreationMetadata, SessionEventKind, SessionQueryService,
    SessionSource, SessionTitle,
};
use heycode_tui::app::accessibility::ScreenReaderSnapshot;
use heycode_tui::app::{AppState, SessionRecompose, TuiRunOutcome};
use heycode_tui::session_browser::{
    SessionCommandRequest, SessionDeleteChoice, SessionStorageView,
};
use ratatui::{Terminal, backend::TestBackend};

fn metadata(cwd: &str) -> SessionCreationMetadata {
    SessionCreationMetadata::new(
        Some(std::path::PathBuf::from(cwd)),
        Some("native".to_owned()),
        SessionSource::Interactive,
    )
    .unwrap()
}

fn create_named(service: &SessionQueryService, title: &str, cwd: &str) -> heycode_core::SessionId {
    let session = service
        .create(
            &SessionCreateRequest::new(metadata(cwd)).with_title(SessionTitle::new(title).unwrap()),
        )
        .unwrap();
    let id = session.id().clone();
    drop(session);
    id
}

fn key(code: KeyCode) -> Event {
    Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
}

#[test]
fn browser_pages_deterministically_excludes_current_and_shows_latest_and_safe_facts() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    for index in 0..14 {
        create_named(
            &service,
            &format!("Saved {index:02}"),
            if index % 2 == 0 {
                "/work/project"
            } else {
                "/work/other"
            },
        );
    }
    let current = service
        .create(
            &SessionCreateRequest::new(metadata("/work/project"))
                .with_title(SessionTitle::new("Current session").unwrap()),
        )
        .unwrap();
    let current_id = current.id().clone();
    let current = Arc::new(Mutex::new(current));

    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(service, current, metadata("/work/project"));
    state.open_session_browser();
    assert_eq!(
        state.session_browser().unwrap().total_matches(),
        7,
        "the initial list is scoped to this folder"
    );
    state.handle_terminal_event(&key(KeyCode::Char('w')));
    let browser = state.session_browser().unwrap();
    assert_eq!(browser.rows().len(), 10, "the page is bounded");
    assert_eq!(browser.total_matches(), 14);
    assert_eq!(
        browser.rows().iter().filter(|row| row.is_current()).count(),
        0
    );
    assert_eq!(
        browser.rows().iter().filter(|row| row.is_latest()).count(),
        1
    );
    assert!(
        browser
            .rows()
            .iter()
            .all(|row| row.summary().id() != &current_id)
    );
    assert!(
        browser
            .rows()
            .iter()
            .all(|row| row.summary().runtime() == Some("native"))
    );
    let first_ids = browser
        .rows()
        .iter()
        .map(|row| row.summary().id().clone())
        .collect::<Vec<_>>();

    state.handle_terminal_event(&key(KeyCode::PageDown));
    let second = state.session_browser().unwrap();
    assert_eq!(second.page_index(), 1);
    assert_eq!(second.rows().len(), 4);
    assert!(second.rows().iter().all(|row| row.summary().id() != &current_id && !first_ids.contains(row.summary().id())));
    state.handle_terminal_event(&key(KeyCode::PageUp));
    assert_eq!(state.session_browser().unwrap().page_index(), 0);
}

#[test]
fn browser_filters_archive_and_lineage_and_lists_corruption_as_an_unreadable_row() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let current_id = current.id().clone();
    let child = service
        .fork(&current_id, heycode_session::ForkBoundary::Latest)
        .unwrap();
    let child_id = child.id().clone();
    drop(child);
    service
        .archive(&child_id, heycode_session::SessionArchiveAction::Archive)
        .unwrap();

    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.open_session_browser();
    state
        .session_browser_mut()
        .unwrap()
        .set_storage(SessionStorageView::Archived);
    state.session_browser_mut().unwrap().show_forks_only();
    state.refresh_session_browser();
    let browser = state.session_browser().unwrap();
    assert_eq!(browser.rows().len(), 1);
    assert_eq!(browser.rows()[0].summary().id(), &child_id);
    assert_eq!(
        browser.rows()[0]
            .summary()
            .lineage()
            .unwrap()
            .parent_session_id(),
        &current_id
    );

    // A corrupt log is one unreadable row in the browser — visibly damaged,
    // never silently skipped, and never a reason to show no sessions at all.
    let corrupt = root.path().join("corrupt-session");
    std::fs::create_dir(&corrupt).unwrap();
    std::fs::write(corrupt.join("session.jsonl"), b"not json\n").unwrap();
    state
        .session_browser_mut()
        .unwrap()
        .set_storage(SessionStorageView::All);
    state.session_browser_mut().unwrap().show_all_lineage();
    // Unknown-folder damaged logs appear only after explicitly showing all folders.
    state.handle_terminal_event(&key(KeyCode::Char('w')));
    state.refresh_session_browser();
    let browser = state.session_browser().unwrap();
    assert_eq!(browser.error(), None);
    let damaged = browser
        .rows()
        .iter()
        .find(|row| row.summary().id().as_str() == "corrupt-session")
        .expect("the damaged session is listed");
    assert!(!damaged.summary().is_readable());
    assert!(
        browser.rows().len() >= 2,
        "healthy sessions are still listed beside it: {}",
        browser.rows().len()
    );
}

#[test]
fn unreadable_row_is_visibly_damaged_in_full_and_flat_modes() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let corrupt = root.path().join("corrupt-session");
    std::fs::create_dir(&corrupt).unwrap();
    std::fs::write(corrupt.join("session.jsonl"), b"not json\n").unwrap();

    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service,
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.open_session_browser();
    state.handle_terminal_event(&key(KeyCode::Char('w')));
    let browser = state.session_browser().unwrap();
    assert!(
        browser
            .rows()
            .iter()
            .any(|row| !row.summary().is_readable()),
        "the damaged session is listed"
    );

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, &mut state))
        .unwrap();
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("unreadable"),
        "full mode must not draw a damaged log as an ordinary empty session: {text}"
    );

    let flat = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(
        flat.contains("unreadable"),
        "flat mode must name the damaged row too: {flat}"
    );
}

#[test]
fn delete_confirmation_defaults_to_cancel_and_explicit_confirm_uses_safe_trash() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let saved_id = create_named(&service, "Delete me", "/work/project");
    let current = service
        .create(
            &SessionCreateRequest::new(metadata("/work/project"))
                .with_title(SessionTitle::new("Current").unwrap()),
        )
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.open_session_browser();
    while state
        .session_browser()
        .unwrap()
        .selected()
        .unwrap()
        .summary()
        .id()
        != &saved_id
    {
        state.handle_terminal_event(&key(KeyCode::Down));
    }

    state.handle_terminal_event(&key(KeyCode::Char('d')));
    assert_eq!(
        state
            .session_browser()
            .unwrap()
            .delete_confirmation()
            .unwrap()
            .choice(),
        SessionDeleteChoice::Cancel
    );
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(service.resume(&saved_id).is_ok(), "default Enter cancels");

    state.handle_terminal_event(&key(KeyCode::Char('d')));
    state.handle_terminal_event(&key(KeyCode::Right));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(matches!(
        service.resume(&saved_id),
        Err(heycode_session::SessionQueryError::SessionNotFound)
    ));
    assert!(
        state
            .session_browser()
            .unwrap()
            .notice()
            .unwrap()
            .contains("recovery")
    );
}

#[test]
fn lifecycle_requests_return_typed_recomposition_only_after_durable_creation() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let current_id = current.id().clone();
    let saved_id = create_named(&service, "Resume target", "/work/project");
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );

    state.handle_session_command(SessionCommandRequest::Resume(saved_id.clone()));
    assert_eq!(
        state.take_run_outcome(),
        Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Resume {
            session_id: saved_id.clone(),
        }))
    );

    state.handle_session_command(SessionCommandRequest::Fork(Some(current_id.clone())));
    let Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Forked {
        parent_session_id,
        session_id,
        ..
    })) = state.take_run_outcome()
    else {
        panic!("fork did not return a typed session outcome")
    };
    assert_eq!(parent_session_id, current_id);
    assert_eq!(
        service
            .resume(&session_id)
            .unwrap()
            .lineage()
            .unwrap()
            .parent_session_id(),
        &parent_session_id
    );

    state.handle_session_command(SessionCommandRequest::New(Some(
        SessionTitle::new("Brand new").unwrap(),
    )));
    let Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Created { session_id, .. })) =
        state.take_run_outcome()
    else {
        panic!("new did not return a typed session outcome")
    };
    assert_eq!(
        service.resume(&session_id).unwrap().events()[1].kind,
        SessionEventKind::SessionTitle {
            title: "Brand new".to_owned()
        }
    );
}

#[test]
fn clearing_a_named_session_retains_its_title_and_local_command_receipt() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let original = create_named(&service, "Keep this title", "/work/project");
    let current = service.resume(&original).unwrap();
    let original_events = current.events().len();
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state
        .items
        .push(heycode_tui::Item::Command("/clear".to_owned()));
    state.handle_session_command(SessionCommandRequest::New(None));
    let Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Created {
        session_id,
        command,
    })) = state.take_run_outcome()
    else {
        panic!("clear did not create a conversation");
    };
    assert_ne!(session_id, original);
    assert_eq!(command.as_deref(), Some("/clear"));
    let cleared = service.resume(&session_id).unwrap();
    assert!(cleared.events().iter().any(|event| matches!(&event.kind, SessionEventKind::SessionTitle { title } if title == "Keep this title")));
    assert_eq!(
        service.resume(&original).unwrap().events().len(),
        original_events
    );
}

#[test]
fn direct_delete_request_still_opens_the_same_cancel_default_confirmation() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let saved_id = create_named(&service, "Saved", "/work/project");
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service,
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.handle_session_command(SessionCommandRequest::Delete(Some(saved_id.clone())));
    let confirmation = state
        .session_browser()
        .unwrap()
        .delete_confirmation()
        .unwrap();
    assert_eq!(confirmation.session_id(), &saved_id);
    assert_eq!(confirmation.choice(), SessionDeleteChoice::Cancel);
}

#[test]
fn direct_rename_archive_and_export_requests_cross_the_lower_service() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let saved_id = create_named(&service, "Saved", "/work/project");
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let current_id = current.id().clone();
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );

    state.handle_session_command(SessionCommandRequest::Rename(Some(
        SessionTitle::new("Renamed current").unwrap(),
    )));
    assert!(matches!(
        service.resume(&current_id).unwrap().events().last().unwrap().kind,
        SessionEventKind::SessionTitle { ref title } if title == "Renamed current"
    ));

    state.handle_session_command(SessionCommandRequest::Archive(Some(saved_id.clone())));
    let archived = service
        .query(
            &heycode_session::SessionQuery::new(
                heycode_session::SessionFilter::new()
                    .with_storage(heycode_session::SessionStorageFilter::Archived),
                10,
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(archived.items()[0].id(), &saved_id);

    state.handle_session_command(SessionCommandRequest::Export {
        session_id: Some(saved_id),
        format: heycode_session::SessionExportFormat::Markdown,
    });
    let exports = std::fs::read_dir(root.path().join(".exports"))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(exports.len(), 1);
    assert!(
        exports[0]
            .path()
            .extension()
            .is_some_and(|value| value == "md")
    );
}

#[test]
fn resume_search_starts_focused_filters_conversation_text_and_enter_selects() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let mut saved = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    saved
        .append(SessionEventKind::UserMessage {
            text: "Repair login redirect loops".into(),
        })
        .unwrap();
    let saved_id = saved.id().clone();
    drop(saved);
    create_named(&service, "Other folder login", "/work/other");
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(
        service,
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.handle_session_command(SessionCommandRequest::Browse);
    for character in "redirect".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character)));
    }
    let browser = state.session_browser().unwrap();
    assert_eq!(browser.total_matches(), 1);
    assert_eq!(
        browser.rows()[0].summary().title(),
        Some("Repair login redirect loops")
    );
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(
        matches!(state.take_run_outcome(), Some(TuiRunOutcome::RecomposeSession(
        SessionRecompose::Resume { session_id })) if session_id == saved_id)
    );
}

#[test]
fn no_argument_rename_uses_conversation_and_reports_resolved_duplicate_title() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    create_named(&service, "Fix login bug", "/work/project");
    let mut current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    current
        .append(SessionEventKind::UserMessage {
            text: "Fix login bug".into(),
        })
        .unwrap();
    let current_id = current.id().clone();
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.handle_session_command(SessionCommandRequest::Rename(None));
    assert!(
        matches!(service.resume(&current_id).unwrap().events().last().unwrap().kind,
        SessionEventKind::SessionTitle { ref title } if title == "Fix login bug (2)")
    );
    assert!(state.items.iter().any(|item| matches!(item,
        heycode_tui::app::Item::Info(text) if text == "Renamed to Fix login bug (2)")));
}

#[test]
fn named_branch_is_durable_before_switch_and_preserves_the_source() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let mut current = service
        .create(
            &SessionCreateRequest::new(metadata("/work/project"))
                .with_title(SessionTitle::new("Original named conversation").unwrap()),
        )
        .unwrap();
    current
        .append(SessionEventKind::UserMessage {
            text: "Original conversation".into(),
        })
        .unwrap();
    let current_id = current.id().clone();
    let original_events = current.events().to_vec();
    let current = Arc::new(Mutex::new(current));
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(service.clone(), current.clone(), metadata("/work/project"));
    state.handle_session_command(SessionCommandRequest::Branch {
        session_id: None,
        title: Some(SessionTitle::from_input("Login  alternatives").unwrap()),
    });
    let Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Forked {
        parent_session_id,
        parent_title,
        session_id,
        title,
        command,
    })) = state.take_run_outcome()
    else {
        panic!("named branch did not return a durable child");
    };
    assert_eq!(parent_session_id, current_id);
    assert_eq!(command, "/branch Login  alternatives");
    assert_eq!(title.as_deref(), Some("Login  alternatives"));
    assert_eq!(parent_title.as_deref(), Some("Original named conversation"));
    let receipt = heycode_tui::branch_receipt(
        &session_id,
        &parent_session_id,
        title.as_deref(),
        parent_title.as_deref(),
    );
    assert!(receipt.contains(&format!(
        "/resume {parent_session_id} (\"Original named conversation\")"
    )));
    assert!(receipt.contains(&format!("heycode --resume {parent_session_id}")));
    let child = service.resume(&session_id).unwrap();
    assert_ne!(child.id(), &current_id);
    assert_eq!(child.lineage().unwrap().parent_session_id(), &current_id);
    assert!(
        matches!(&child.events().last().unwrap().kind, SessionEventKind::SessionTitle { title } if title == "Login  alternatives")
    );
    assert_eq!(current.lock().unwrap().events(), original_events);
}

#[test]
fn inline_resume_requires_exact_title_while_bare_picker_keeps_substring_search() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let id = create_named(&service, "Fix login redirect", "/work/project");
    create_named(&service, "Login redirect in other folder", "/work/other");
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(
        service,
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.handle_session_command(SessionCommandRequest::ResumeSearch("login redirect".into()));
    assert!(state.session_browser().is_none());
    assert!(state.take_run_outcome().is_none());
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .into_text()
            .contains("Session login redirect was not found.")
    );
    state.handle_session_command(SessionCommandRequest::Browse);
    state
        .session_browser_mut()
        .unwrap()
        .set_search("login redirect")
        .unwrap();
    assert_eq!(state.session_browser().unwrap().total_matches(), 1);
    assert!(
        state.take_run_outcome().is_none(),
        "picker search requires selection"
    );
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(
        matches!(state.take_run_outcome(), Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Resume { session_id })) if session_id == id)
    );
    state.handle_session_command(SessionCommandRequest::Browse);
    state
        .session_browser_mut()
        .unwrap()
        .set_search("absent term")
        .unwrap();
    assert_eq!(state.session_browser().unwrap().total_matches(), 0);
    assert!(
        state
            .session_browser_mut()
            .unwrap()
            .set_search(&"x".repeat(129))
            .is_err()
    );
    assert_eq!(state.session_browser().unwrap().total_matches(), 0);
}

#[test]
fn resume_exact_title_uses_current_title_and_rejects_ambiguity_across_pages() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let id = create_named(&service, "Exact saved title", "/work/project");
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    for index in 0..11 {
        create_named(
            &service,
            &format!("Exact saved title extra {index}"),
            "/work/project",
        );
    }
    let mut renamed = service
        .resume(&create_named(
            &service,
            "Exact saved title",
            "/work/project",
        ))
        .unwrap();
    service
        .rename_open(
            &mut renamed,
            &SessionTitle::new("Renamed unrelated").unwrap(),
        )
        .unwrap();
    drop(renamed);
    state.handle_session_command(SessionCommandRequest::ResumeSearch(
        "Exact saved title".into(),
    ));
    assert!(
        matches!(state.take_run_outcome(), Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Resume { session_id })) if session_id == id)
    );
    create_named(&service, "Exact saved title", "/work/project");
    state.handle_session_command(SessionCommandRequest::ResumeSearch(
        "Exact saved title".into(),
    ));
    assert!(
        state.take_run_outcome().is_none(),
        "duplicate titles require an explicit selection"
    );
    assert_eq!(state.session_browser().unwrap().total_matches(), 2);
    for _ in 0..11 {
        create_named(&service, "Exact saved title", "/work/project");
    }
    state.handle_session_command(SessionCommandRequest::ResumeSearch(
        "Exact saved title".into(),
    ));
    assert!(
        state.take_run_outcome().is_none(),
        "hidden duplicate pages cannot establish unique identity"
    );
    assert_eq!(state.session_browser().unwrap().total_matches(), 13);
    assert!(
        state.session_browser().unwrap().total_matches()
            > state.session_browser().unwrap().rows().len()
    );
    let unicode_title = "é".repeat(200);
    let unicode_id = create_named(&service, &unicode_title, "/work/project");
    state.handle_session_command(SessionCommandRequest::ResumeSearch(unicode_title));
    assert!(
        matches!(state.take_run_outcome(), Some(TuiRunOutcome::RecomposeSession(SessionRecompose::Resume { session_id })) if session_id == unicode_id)
    );
}

#[test]
fn rewind_picker_owns_input_and_only_queues_a_confirmed_current_session_choice() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let current = service
        .create(&SessionCreateRequest::new(metadata("/work/project")))
        .unwrap();
    let id = current.id().clone();
    let current = Arc::new(Mutex::new(current));
    let original_events = current.lock().unwrap().events().to_vec();
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(service.clone(), current.clone(), metadata("/work/project"));
    let request = heycode_tui::rewind_picker::RewindPickerRequest::new(
        id.clone(),
        vec![heycode_agent::RewindPoint {
            turn: 4,
            event_count: 0,
            prompt: "Fix the parser".into(),
            at_ms: 1_800_000_000_000,
        }],
    );
    state.handle_session_command(SessionCommandRequest::RewindPicker(request.clone()));
    let snapshot = ScreenReaderSnapshot::from_state(&state).into_text();
    assert!(snapshot.contains("Fix the parser"));
    state.handle_terminal_event(&Event::Paste("/quit\n".into()));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(state.pending_send.is_none());
    assert_eq!(state.queued_command_count(), 0);
    state.handle_session_command(SessionCommandRequest::RewindPicker(request.clone()));
    state.handle_terminal_event(&key(KeyCode::Up));
    state.handle_terminal_event(&key(KeyCode::Enter));
    state.handle_terminal_event(&key(KeyCode::Down)); // Explicit Never mind.
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert_eq!(state.queued_command_count(), 0);
    state.handle_session_command(SessionCommandRequest::RewindPicker(request));
    state.handle_terminal_event(&key(KeyCode::Up));
    state.handle_terminal_event(&key(KeyCode::Enter));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert_eq!(state.queued_command_count(), 1);
    assert!(state.pending_send.is_none());
    state.promote_next_queued_command();
    assert_eq!(state.pending_send.as_deref(), Some("/rewind 4"));
    assert_eq!(
        current.lock().unwrap().events(),
        original_events,
        "selection must not execute a rewind"
    );

    state.pending_send = None;
    let stale = heycode_tui::rewind_picker::RewindPickerRequest::new(
        heycode_core::SessionId::from_raw("different-session"),
        vec![],
    );
    state.handle_session_command(SessionCommandRequest::RewindPicker(stale));
    assert_eq!(state.queued_command_count(), 0);
    assert!(state.items.iter().any(|item| matches!(item, heycode_tui::app::Item::Error(text) if text == "Rewind checkpoint belongs to another session")));
}

#[test]
fn resume_picker_frames_rows_search_and_selection_like_the_source() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    create_named(&service, "Older saved conversation", "/work/project");
    create_named(&service, "Newer saved conversation", "/work/project");
    let current = service
        .create(
            &SessionCreateRequest::new(metadata("/work/project"))
                .with_title(SessionTitle::new("Current conversation").unwrap()),
        )
        .unwrap();
    let mut state = AppState::new("model", std::path::PathBuf::from("/work/project"));
    state.set_session_service(
        service,
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.open_session_browser();

    let rows_of = |state: &mut AppState| {
        let mut terminal = Terminal::new(TestBackend::new(110, 42)).unwrap();
        terminal
            .draw(|frame| heycode_tui::render::draw(frame, state))
            .unwrap();
        let buffer = terminal.backend().buffer().clone();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_owned())
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect::<Vec<_>>()
    };

    let frame = rows_of(&mut state);
    let heading = frame
        .iter()
        .position(|line| line.trim() == "Resume session")
        .expect("the panel names the resume surface like the source");
    assert!(
        frame[heading - 1].starts_with('▔'),
        "a full-width separator opens the panel: {:?}",
        frame[heading - 1]
    );
    assert!(frame[heading + 1].starts_with("   ╭"), "{frame:#?}");
    assert!(
        frame[heading + 2].starts_with("   │ ⌕ Search…"),
        "{frame:#?}"
    );
    assert!(frame[heading + 3].starts_with("   ╰"), "{frame:#?}");
    let selected = frame
        .iter()
        .find(|line| line.starts_with("   ❯ "))
        .expect("exactly one row carries the selection marker");
    assert!(!selected.contains("Current conversation"), "{selected:?}");
    assert!(selected.contains("saved conversation"), "{selected:?}");
    assert!(
        frame.iter().any(|line| line == "     project"),
        "{frame:#?}"
    );
    assert!(
        !frame.iter().any(|line| line.contains("filter  active")),
        "{frame:#?}"
    );
    assert!(
        frame
            .iter()
            .any(|line| line.contains("Ctrl+A to show all projects")),
        "{frame:#?}"
    );
    let age = frame
        .iter()
        .position(|line| line.contains("seconds ago") || line.contains("second ago"))
        .expect("each row states its age and size like the source");
    assert!(
        frame[age].contains("events") && frame[age].starts_with("     "),
        "the metadata column sits under the title: {:?}",
        frame[age]
    );
    assert!(
        frame[age + 1].is_empty(),
        "a blank row separates saved conversations: {:?}",
        frame[age + 1]
    );

    // Typing filters the list and is echoed inside the search field.
    state.handle_terminal_event(&key(KeyCode::Char('/')));
    for character in "Older".chars() {
        state.handle_terminal_event(&key(KeyCode::Char(character)));
    }
    let searched = rows_of(&mut state);
    assert!(
        searched.iter().any(|line| line.contains("│ ⌕ Older")),
        "{searched:#?}"
    );
    assert!(
        searched
            .iter()
            .any(|line| line.contains("Older saved conversation")),
        "{searched:#?}"
    );
    assert!(
        !searched
            .iter()
            .any(|line| line.contains("Newer saved conversation")),
        "a non-matching conversation is filtered out: {searched:#?}"
    );
}

#[test]
fn resume_workspace_shortcuts_preserve_search_and_target_saved_session() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let local = create_named(&service, "Saved local", "/work/project");
    create_named(&service, "Saved elsewhere", "/elsewhere/project");
    let current = service
        .create(
            &SessionCreateRequest::new(metadata("/work/project"))
                .with_title(SessionTitle::new("Saved current").unwrap()),
        )
        .unwrap();
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(
        service.clone(),
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.open_session_browser();
    state
        .session_browser_mut()
        .unwrap()
        .set_search("Saved")
        .unwrap();
    assert_eq!(state.session_browser().unwrap().total_matches(), 1);
    assert_eq!(
        state
            .session_browser()
            .unwrap()
            .workspace_group(0)
            .as_deref(),
        Some("project")
    );
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
    )));
    let browser = state.session_browser().unwrap();
    assert_eq!(browser.total_matches(), 2);
    let groups = (0..browser.rows().len())
        .filter_map(|index| browser.workspace_group(index))
        .collect::<Vec<_>>();
    assert!(groups.contains(&"/work/project".to_owned()));
    assert!(groups.contains(&"/elsewhere/project".to_owned()));
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('a'),
        KeyModifiers::CONTROL,
    )));
    assert_eq!(
        state
            .session_browser()
            .unwrap()
            .selected()
            .unwrap()
            .summary()
            .id(),
        &local
    );
    state.handle_terminal_event(&Event::Key(KeyEvent::new(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    state.handle_terminal_event(&key(KeyCode::Char('!')));
    state.handle_terminal_event(&key(KeyCode::Enter));
    assert!(service.resume(&local).unwrap().events().iter().any(|event|
        matches!(&event.kind, SessionEventKind::SessionTitle { title } if title == "Saved local!")));
    assert_eq!(
        state.session_browser().unwrap().total_matches(),
        1,
        "shortcut keys must not enter search text"
    );
}

#[test]
fn excluded_session_filter_binds_cursor_and_counts_before_pagination() {
    use heycode_session::{SessionFilter, SessionQuery, SessionQueryError};
    let root = tempfile::tempdir().unwrap();
    let service = SessionQueryService::local(root.path().to_path_buf());
    let excluded = create_named(&service, "Excluded", "/work/project");
    for index in 0..3 {
        create_named(&service, &format!("Saved {index}"), "/work/project");
    }
    let filter = SessionFilter::new().excluding_session(excluded.clone());
    let first = service
        .query(&SessionQuery::new(filter.clone(), 2).unwrap())
        .unwrap();
    assert_eq!(first.total_matches(), 3);
    assert_eq!(first.items().len(), 2);
    assert!(first.items().iter().all(|row| row.id() != &excluded));
    let cursor = first.next_cursor().unwrap().clone();
    let second = service
        .query(
            &SessionQuery::new(filter, 2)
                .unwrap()
                .with_cursor(cursor.clone()),
        )
        .unwrap();
    assert_eq!(second.items().len(), 1);
    assert_eq!(second.total_matches(), 3);
    assert!(second.items().iter().all(|row| row.id() != &excluded));
    assert!(matches!(
        service.query(
            &SessionQuery::new(SessionFilter::new(), 2)
                .unwrap()
                .with_cursor(cursor)
        ),
        Err(SessionQueryError::InvalidCursor)
    ));
}

#[test]
fn explicit_current_title_still_resolves_without_exposing_it_in_browse_search() {
    let root = tempfile::tempdir().unwrap();
    let service = Arc::new(SessionQueryService::local(root.path().to_path_buf()));
    let current = service
        .create(
            &SessionCreateRequest::new(metadata("/work/project"))
                .with_title(SessionTitle::new("Current named conversation").unwrap()),
        )
        .unwrap();
    let mut state = AppState::new("model", "/work/project".into());
    state.set_session_service(
        service,
        Arc::new(Mutex::new(current)),
        metadata("/work/project"),
    );
    state.handle_session_command(SessionCommandRequest::ResumeSearch(
        "Current named conversation".into(),
    ));
    assert!(state.session_browser().is_none());
    assert!(state.take_run_outcome().is_none());
    assert!(state.items.iter().any(|item| matches!(item, heycode_tui::app::Item::Info(text) if text == "session is already current")));
    state.open_session_browser();
    state
        .session_browser_mut()
        .unwrap()
        .set_search("Current")
        .unwrap();
    assert_eq!(state.session_browser().unwrap().total_matches(), 0);
}
