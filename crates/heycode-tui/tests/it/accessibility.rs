//! U19 flat-output, terminal-control, semantic, and keyboard contracts.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_tui::app::accessibility::{FlatOutput, ScreenReaderSnapshot};
use heycode_tui::app::{AppState, Item};
use heycode_tui::terminal::{Chrome, TuiDisplayMode, enter_terminal_screen, leave_terminal_screen};
use heycode_ui::terminal::{RenderMode, TerminalEnvironment};

use crate::support::journey::key;

fn screen_reader_state() -> AppState {
    let mut state = AppState::new("model-a", std::path::PathBuf::from("/workspace"));
    state.runtime = "native".to_owned();
    state.provider = "provider-a".to_owned();
    state.permission = "ask".to_owned();
    let environment = TerminalEnvironment::new()
        .with_term(Some("xterm-256color"))
        .with_colorterm(Some("truecolor"))
        .with_columns(Some(120));
    state.apply_terminal(
        TuiDisplayMode::ScreenReader.resolve(&environment),
        &heycode_ui::theme::default_theme().unwrap(),
    );
    state
}

#[test]
fn finding_reports_are_color_independent_and_name_their_local_scope() {
    let report = heycode_session::FindingReport::new(
        heycode_session::FindingReportId::new("report-accessible").unwrap(),
        heycode_session::FindingReportSource::workspace(
            heycode_core::SessionId::from_raw("session-accessible"),
            4,
            0,
            None,
        )
        .unwrap(),
        vec![
            heycode_session::ReportedFinding::new(
                "finding-accessible",
                heycode_session::ReviewSeverity::Critical,
                "src/main.rs",
                7,
                7,
                "a".repeat(64),
                "Unsafe boundary",
                "Send one crafted request",
                "The boundary is crossed",
                "Another tenant can be affected",
            )
            .unwrap(),
        ],
    )
    .unwrap();
    let mut state = screen_reader_state();
    state.items.push(Item::FindingsReport {
        report: Box::new(report),
        expanded: true,
        focused: true,
    });

    let text = ScreenReaderSnapshot::from_state(&state);
    let text = text.as_text();
    assert!(text.contains(
        "expanded findings report: 1 finding; highest critical; workspace revision 4; local, not externally published"
    ));
    assert!(text.contains("finding 1: critical — src/main.rs:7 — Unsafe boundary"));
    assert!(text.contains("trigger: Send one crafted request"));
    assert!(text.contains("failure: The boundary is crossed"));
    assert!(text.contains("impact: Another tenant can be affected"));
}

#[test]
fn screen_reader_mode_forces_flat_colorless_motionless_rendering() {
    let environment = TerminalEnvironment::new()
        .with_term(Some("xterm-256color"))
        .with_colorterm(Some("truecolor"))
        .with_columns(Some(120));
    let automatic = TuiDisplayMode::Automatic.resolve(&environment);
    let accessible = TuiDisplayMode::ScreenReader.resolve(&environment);
    assert_eq!(automatic.render(), RenderMode::Full);
    assert_eq!(accessible.render(), RenderMode::Flat);
    assert!(!Chrome::new(accessible.render()).alternate_screen());
    assert!(!Chrome::new(accessible.render()).animation());

    let mut state = screen_reader_state();
    state.verb = Some("Thinking…".to_owned());
    let before = ScreenReaderSnapshot::from_state(&state);
    state.tick_spinner();
    assert_eq!(ScreenReaderSnapshot::from_state(&state), before);
}

#[test]
fn flat_terminal_lifecycle_emits_no_alternate_screen_or_cursor_control_bytes() {
    let flat = Chrome::new(RenderMode::Flat);
    let full = Chrome::new(RenderMode::Full);
    let mut control = Vec::new();
    enter_terminal_screen(&mut control, full).unwrap();
    leave_terminal_screen(&mut control, full).unwrap();
    assert!(
        control.contains(&0x1b),
        "control proves the detector can see escapes"
    );

    let mut bytes = Vec::new();
    enter_terminal_screen(&mut bytes, flat).unwrap();
    leave_terminal_screen(&mut bytes, flat).unwrap();
    assert_eq!(bytes, Vec::<u8>::new());
}

/// `--screen-reader` on an ordinary terminal draws flat because the user asked
/// for flat — that terminal can still report a bracketed paste, and without
/// one a pasted block arrives as keystrokes whose first newline sends line one
/// as a prompt. A terminal that really is `dumb` is still asked nothing.
#[test]
fn a_requested_flat_frame_still_enables_bracketed_paste_on_a_capable_terminal() {
    use heycode_tui::terminal::InputProtocol;
    let dumb = Chrome::new(RenderMode::Flat);
    assert!(!dumb.bracketed_paste(), "a dumb terminal is asked nothing");

    let requested = Chrome::new(RenderMode::Flat).with_input(InputProtocol::Escapes);
    assert!(!requested.borders(), "still a flat frame");
    assert!(!requested.alternate_screen());
    assert!(!requested.animation());
    assert!(requested.bracketed_paste());

    let mut bytes = Vec::new();
    enter_terminal_screen(&mut bytes, requested).unwrap();
    leave_terminal_screen(&mut bytes, requested).unwrap();
    let written = String::from_utf8(bytes).unwrap();
    assert_eq!(
        written, "\u{1b}[?2004h\u{1b}[?2004l",
        "exactly the paste mode, on and off: no alternate screen, no cursor control"
    );
}

#[test]
fn flat_snapshot_names_transcript_status_dialog_focus_and_keyboard_path() {
    let mut state = screen_reader_state();
    state.items.push(Item::User("check the build".to_owned()));
    state
        .items
        .push(Item::Assistant("I found one issue.".to_owned()));
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 7,
        name: "bash".to_owned(),
        args_preview: "cargo test".to_owned(),
    });

    assert_eq!(
        ScreenReaderSnapshot::from_state(&state).as_text(),
        "== heycode ==\nversion: 0.1.0\nroute: provider-a/model-a\nworkspace: /workspace\n== Transcript ==\nuser: check the build\nassistant: I found one issue.\n== Status ==\napproval policy: ask\ncontext: unavailable\ntokens: in 0 out 0\nactivity: waiting for approval\n== Composer ==\ninput: empty\nattachments: 0\nkeys: Enter sends; Alt+Enter or backslash then Enter inserts a new line; Control+P opens commands; Escape interrupts; Page Up and Page Down scroll.\n== Permission requested ==\ntool: bash\ncargo test\n[selected] Accept\n[ ] Reject\nkeys: Up or Down chooses; Enter confirms; y accepts; n or Escape rejects."
    );

    state.handle_terminal_event(&key(crossterm::event::KeyCode::Down));
    let denied = ScreenReaderSnapshot::from_state(&state);
    assert!(denied.as_text().contains("[ ] Accept\n[selected] Reject"));
    state.handle_terminal_event(&key(crossterm::event::KeyCode::Enter));
    assert!(state.pending_ask.is_none());
    assert!(
        ScreenReaderSnapshot::from_state(&state)
            .as_text()
            .contains("assistant: I found one issue.")
    );
    assert!(!state.items.iter().any(|item| matches!(item, Item::Info(_))));
}

#[test]
fn flat_output_is_plain_deduplicated_and_sanitizes_control_payloads() {
    let mut state = screen_reader_state();
    state
        .items
        .push(Item::Info("before\u{1b}[2Jafter\u{9b}1;1H".to_owned()));
    let mut output = FlatOutput::new(Vec::new());
    assert!(output.render(&state).unwrap());
    assert!(
        !output.render(&state).unwrap(),
        "unchanged state is not repeated"
    );
    let bytes = output.into_inner();
    assert!(!bytes.contains(&0x1b));
    assert!(!bytes.contains(&0x9b));
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.matches("== Transcript ==").count(), 1);
    assert!(text.contains("info: before[2Jafter1;1H"), "{text}");
}

#[test]
fn flat_audio_output_contains_only_safe_metadata() {
    let mut state = screen_reader_state();
    let attachment = heycode_core::AttachmentMetadata::new_audio(
        heycode_core::AttachmentContentId::from_sha256([0x75; 32]),
        heycode_core::AttachmentMediaType::new("audio/wav").unwrap(),
        16_044,
        Some("answer.wav".to_owned()),
        heycode_core::AttachmentAudioMetadata::new(1_000, 8_000, 1, 16).unwrap(),
    )
    .unwrap();
    state.apply(&heycode_agent::UiEvent::AssistantAudio {
        attachments: vec![attachment.clone()],
    });
    let snapshot = ScreenReaderSnapshot::from_state(&state);
    let text = snapshot.as_text();
    assert!(text.contains("assistant audio: answer.wav"), "{text}");
    assert!(text.contains("1.00 seconds"), "{text}");
    assert!(!text.contains(attachment.content_id().as_str()), "{text}");
    assert!(!text.contains("base64"), "{text}");
}

/// A screen reader must hear the reason editor too: the choices are gone
/// while it is open, and the key line says what the keyboard now does.
#[test]
fn the_flat_frame_names_the_denial_reason_editor_instead_of_the_choices() {
    let mut state =
        heycode_tui::app::AppState::new("model-a", std::path::PathBuf::from("/workspace"));
    state.apply(&heycode_agent::UiEvent::ApprovalRequested {
        owner_session: None,
        id: 3,
        name: "bash".to_owned(),
        args_preview: "command: rm -rf /data".to_owned(),
    });
    state.handle_terminal_event(&crossterm::event::Event::Key(
        crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('r'),
            crossterm::event::KeyModifiers::NONE,
        ),
    ));
    let frame = ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(frame.contains("reason: empty"), "{frame}");
    assert!(
        frame.contains(
            "keys: type the reason; Enter denies with it; Escape returns to the choices."
        ),
        "{frame}"
    );
    assert!(!frame.contains("[selected] Allow"), "{frame}");
}

#[test]
fn accessible_orchestration_hides_routine_metadata_but_retains_expansion_and_errors() {
    let mut state = screen_reader_state();
    state.items.push(Item::Tool {
        call_id: None,
        name: "agent_control".into(),
        args: serde_json::json!({"action":"list"}),
        result: Some((
            true,
            serde_json::json!({"agents":[],"budget":{"remaining":173}}),
        )),
        untrusted_content: None,
        view: Default::default(),
    });
    let text = ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(
        !text.contains("agent_control") && !text.contains("173"),
        "{text}"
    );
    if let Item::Tool { view, .. } = &mut state.items[0] {
        view.expanded = true;
    }
    let text = ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(
        text.contains("agent_control") && text.contains("173"),
        "{text}"
    );
    if let Item::Tool { view, result, .. } = &mut state.items[0] {
        view.expanded = false;
        *result = Some((false, serde_json::json!({"message":"inspection failed"})));
    }
    let text = ScreenReaderSnapshot::from_state(&state)
        .as_text()
        .to_owned();
    assert!(text.contains("inspection failed"), "{text}");
}

#[test]
fn terminal_cleanup_attempts_remaining_modes_after_a_write_failure() {
    use std::io::{self, Write};
    struct FailOnce {
        failed: bool,
        bytes: Vec<u8>,
    }
    impl Write for FailOnce {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if !self.failed {
                self.failed = true;
                return Err(io::ErrorKind::WouldBlock.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut writer = FailOnce {
        failed: false,
        bytes: Vec::new(),
    };
    assert!(leave_terminal_screen(&mut writer, Chrome::new(RenderMode::Full)).is_err());
    let written = String::from_utf8(writer.bytes).unwrap();
    assert!(written.contains("\x1b[?1006l"));
    assert!(written.contains("\x1b[?1004l"));
    assert!(written.contains("\x1b[<1u"));
    assert!(written.contains("\x1b[?1049l"));
    assert!(written.contains("\x1b[?25h"));
}
