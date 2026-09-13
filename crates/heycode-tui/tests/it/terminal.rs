//! U20 terminal adapter: what the renderer is handed at each capability tier,
//! what a key event becomes, and how a narrow status line degrades.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use heycode_tui::app::AppState;
use heycode_tui::palette;
use heycode_tui::terminal::{
    Chrome, ClipboardOutcome, StatusField, Styles, chord, detect_environment, fit_status,
    truncate_to_width, width_of, write_clipboard,
};
use heycode_ui::keymap::{KeyChord, KeyName, Keymap, KeymapAction, Modifiers};
use heycode_ui::terminal::{ColorLevel, RenderMode, TerminalCapabilities, TerminalEnvironment};
use heycode_ui::theme::{ThemeRole, default_theme};
use ratatui::style::Color;
use ratatui::{Terminal, backend::TestBackend};

fn key_event(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
    KeyEvent {
        code,
        modifiers,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn frame_text(state: &mut AppState, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| heycode_tui::render::draw(frame, state))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect::<Vec<_>>()
        .join("")
}

fn capabilities(term: &str, colorterm: Option<&str>) -> TerminalCapabilities {
    TerminalCapabilities::detect(
        &TerminalEnvironment::new()
            .with_term(Some(term))
            .with_colorterm(colorterm)
            .with_columns(Some(120)),
    )
}

#[test]
fn the_palette_constants_are_the_built_in_theme_at_the_24_bit_tier() {
    // One colour table in the workspace: the legacy constants must be exactly
    // what resolving the built-in theme at TrueColor produces.
    let styles = Styles::new(&default_theme().unwrap().resolve(ColorLevel::TrueColor));
    assert_eq!(styles.accent(), palette::ACCENT);
    assert_eq!(styles.success(), palette::SUCCESS);
    assert_eq!(styles.error(), palette::ERROR);
    assert_eq!(styles.warn(), palette::WARN);
    assert_eq!(styles.text(), palette::TEXT);
    assert_eq!(styles.dim(), palette::DIM);
    assert_eq!(styles.border(), palette::BORDER);
    assert_eq!(Styles::default(), styles);
}

#[test]
fn a_terminal_below_the_24_bit_tier_is_never_handed_an_rgb_colour() {
    let theme = default_theme().unwrap();
    for (level, expect_reset) in [
        (ColorLevel::Ansi256, false),
        (ColorLevel::Basic, false),
        (ColorLevel::None, true),
    ] {
        let styles = Styles::new(&theme.resolve(level));
        for (name, color) in [
            ("accent", styles.accent()),
            ("success", styles.success()),
            ("error", styles.error()),
            ("warn", styles.warn()),
            ("text", styles.text()),
            ("dim", styles.dim()),
            ("border", styles.border()),
            ("prompt-background", styles.prompt_background()),
            ("panel-title", styles.panel_title()),
        ] {
            assert!(
                !matches!(color, Color::Rgb(..)),
                "{name} emitted 24-bit at {}",
                level.as_str()
            );
            if expect_reset {
                assert_eq!(color, Color::Reset, "{name} at {}", level.as_str());
            }
        }
    }
    assert_eq!(
        Styles::new(&theme.resolve(ColorLevel::Ansi256)).accent(),
        Color::Indexed(147),
        "the accent must be the quantized index, not a fresh colour"
    );
}

#[test]
fn applying_a_detected_tier_resolves_the_state_styles_for_that_tier() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    let theme = default_theme().unwrap();
    assert_eq!(state.styles().accent(), palette::ACCENT);

    state.apply_terminal(capabilities("xterm-256color", None), &theme);
    assert!(matches!(state.styles().accent(), Color::Indexed(_)));

    state.apply_terminal(capabilities("xterm", Some("truecolor")), &theme);
    assert_eq!(state.styles().accent(), palette::ACCENT);
}

#[test]
fn a_dumb_terminal_loses_its_borders_animation_and_alternate_screen() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.apply_terminal(capabilities("dumb", None), &default_theme().unwrap());
    assert!(!state.chrome().borders());
    assert!(!state.chrome().alternate_screen());
    assert!(!state.chrome().animation());
    assert_eq!(state.styles().accent(), Color::Reset);

    assert_eq!(Chrome::new(RenderMode::Full), Chrome::default());
    assert!(Chrome::new(RenderMode::Full).borders());
}

#[test]
fn a_dumb_terminal_does_not_animate_the_spinner() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.verb = Some("Thinking…".to_owned());
    let before = state.spinner_glyph();
    let pet_before = state.pet_frame;
    state.tick_spinner();
    state.tick_pet();
    assert_ne!(state.spinner_glyph(), before, "a full terminal animates");
    assert_ne!(state.pet_frame, pet_before, "the companion animates");

    state.apply_terminal(capabilities("dumb", None), &default_theme().unwrap());
    let frozen = state.spinner_glyph();
    let pet_frozen = state.pet_frame;
    state.tick_spinner();
    state.tick_spinner();
    state.tick_pet();
    assert_eq!(state.spinner_glyph(), frozen);
    assert_eq!(state.pet_frame, pet_frozen);
}

#[test]
fn a_key_event_becomes_the_chord_the_user_pressed() {
    assert_eq!(
        chord(key_event(KeyCode::Char('p'), KeyModifiers::CONTROL)),
        Some(KeyChord::new(KeyName::Char('p'), Modifiers::ctrl()))
    );
    assert_eq!(
        chord(key_event(KeyCode::PageUp, KeyModifiers::NONE))
            .map(|chord| chord.to_string())
            .as_deref(),
        Some("pageup")
    );
    assert_eq!(
        chord(key_event(KeyCode::F(5), KeyModifiers::ALT))
            .map(|chord| chord.to_string())
            .as_deref(),
        Some("alt+f5")
    );
}

#[test]
fn a_modifier_the_grammar_cannot_name_produces_no_chord_at_all() {
    // Dropping `super` instead would make Super+P fire the plain ctrl+p
    // binding, which is a keystroke the user never asked for.
    for modifier in [
        KeyModifiers::SUPER,
        KeyModifiers::HYPER,
        KeyModifiers::META,
        KeyModifiers::CONTROL | KeyModifiers::SUPER,
    ] {
        assert_eq!(
            chord(key_event(KeyCode::Char('p'), modifier)),
            None,
            "{modifier:?} must not collapse into a nameable chord"
        );
    }
    assert_eq!(chord(key_event(KeyCode::F(13), KeyModifiers::NONE)), None);
    assert_eq!(
        chord(key_event(KeyCode::CapsLock, KeyModifiers::NONE)),
        None
    );
}

#[test]
fn a_rebound_action_fires_on_its_new_key_and_the_old_key_becomes_input() {
    // The control comes first: with the shipped bindings, ctrl+p opens the
    // palette in exactly this setup. Without it, "ctrl+p did nothing" after a
    // rebinding could just mean the palette can never open here.
    let mut control = AppState::new("m", std::path::PathBuf::from("/p"));
    control.set_commands(std::sync::Arc::new(heycode_agent::CommandRegistry::new()));
    control.handle_terminal_event(&Event::Key(key_event(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL,
    )));
    assert!(
        control.command_palette().is_some(),
        "the default binding must open the palette here"
    );

    let mut overrides = std::collections::BTreeMap::new();
    overrides.insert(
        KeymapAction::CommandPalette,
        KeyChord::parse("ctrl+k").unwrap(),
    );
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    state.set_commands(std::sync::Arc::new(heycode_agent::CommandRegistry::new()));
    state.set_keymap(Keymap::resolve(&overrides).unwrap());

    state.handle_terminal_event(&Event::Key(key_event(
        KeyCode::Char('p'),
        KeyModifiers::CONTROL,
    )));
    assert!(
        state.command_palette().is_none(),
        "the vacated key must no longer open the palette"
    );

    state.handle_terminal_event(&Event::Key(key_event(
        KeyCode::Char('k'),
        KeyModifiers::CONTROL,
    )));
    assert!(state.command_palette().is_some());
}

#[test]
fn a_default_binding_still_fires_when_nothing_was_rebound() {
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    assert!(!state.show_reasoning);
    state.handle_terminal_event(&Event::Key(key_event(
        KeyCode::Char('r'),
        KeyModifiers::CONTROL,
    )));
    assert!(state.show_reasoning);
}

#[test]
fn the_status_line_drops_the_lowest_priority_field_first() {
    let fields = vec![
        StatusField::new("route", ThemeRole::Text, 100),
        StatusField::new(" · cwd", ThemeRole::Dim, 50),
        StatusField::new(" · hint", ThemeRole::Border, 10),
    ];
    let full = fit_status(&fields, 40);
    assert_eq!(full.len(), 3, "everything fits at 40 columns");

    let tight = fit_status(&fields, 12);
    assert_eq!(
        tight
            .iter()
            .map(|field| field.text.as_str())
            .collect::<Vec<_>>(),
        vec!["route", " · cwd"],
        "the hint is the first thing to go"
    );

    let tighter = fit_status(&fields, 5);
    assert_eq!(
        tighter
            .iter()
            .map(|field| field.text.as_str())
            .collect::<Vec<_>>(),
        vec!["route"]
    );
}

#[test]
fn the_status_line_truncates_the_survivor_rather_than_overflowing() {
    let fields = vec![StatusField::new(
        "a-very-long-provider/model-identifier",
        ThemeRole::Text,
        100,
    )];
    let fitted = fit_status(&fields, 10);
    assert_eq!(fitted.len(), 1);
    assert_eq!(width_of(&fitted[0].text), 10);
    assert!(fitted[0].text.ends_with('…'));
}

#[test]
fn a_fitted_status_line_never_exceeds_its_column_budget() {
    let fields = vec![
        StatusField::new("⏺ ", ThemeRole::Accent, 100),
        StatusField::new("openai/gpt-9-turbo", ThemeRole::Text, 99),
        StatusField::new(" · workspace-write", ThemeRole::Dim, 70),
        StatusField::new(" · heycode", ThemeRole::Dim, 50),
        StatusField::new(" ctrl+p palette", ThemeRole::Border, 10),
    ];
    for columns in 0_u16..60 {
        let total: usize = fit_status(&fields, columns)
            .iter()
            .map(|field| width_of(&field.text))
            .sum();
        assert!(
            total <= usize::from(columns),
            "{total} columns used of {columns}"
        );
    }
}

#[test]
fn truncation_measures_display_columns_not_characters() {
    // A CJK character occupies two columns; counting characters would overflow
    // the line by one column per wide glyph.
    assert_eq!(width_of("日本語"), 6);
    let cut = truncate_to_width("日本語テスト", 5);
    assert!(width_of(&cut) <= 5, "`{cut}` overflowed 5 columns");
    assert!(cut.ends_with('…'));
    assert_eq!(truncate_to_width("short", 10), "short");
    assert_eq!(truncate_to_width("short", 0), "");
}

#[test]
fn the_environment_snapshot_reads_the_documented_variables() {
    // The one impure function: it must at least agree with the process it is
    // reading, whatever this host happens to have set.
    let env = detect_environment();
    assert_eq!(
        env.columns().is_some(),
        crossterm::terminal::size().is_ok(),
        "column measurement must reflect whether a terminal answered"
    );
}

#[test]
fn a_dumb_terminal_renders_the_composer_without_a_box() {
    // The visible half of the flat tier: no rounded border, because a terminal
    // with no cursor addressing cannot keep one in place.
    let mut state = AppState::new("m", std::path::PathBuf::from("/p"));
    let full = frame_text(&mut state, 60, 12);
    assert!(full.contains('─'), "a full terminal separates the composer");

    state.apply_terminal(capabilities("dumb", None), &default_theme().unwrap());
    let flat = frame_text(&mut state, 60, 12);
    assert!(
        !flat.contains('\u{256d}'),
        "a dumb terminal must not draw a box: {flat}"
    );
    assert!(
        flat.contains('❯'),
        "the composer must still render its prompt"
    );
}

#[test]
fn a_narrow_status_line_drops_the_key_hint_and_keeps_the_route() {
    // The visible half of the narrow tier: the line degrades by losing its
    // lowest-priority field, never by wrapping the composer off screen.
    let mut state = AppState::new("gpt-9-turbo", std::path::PathBuf::from("/w/project"));
    state.provider = "openai".to_owned();
    state.permission = "workspace-write".to_owned();

    let wide = frame_text(&mut state, 100, 13);
    assert!(wide.contains("gpt-9-turbo"), "{wide}");
    assert!(
        !wide.contains("/ commands"),
        "the reference footer omits the redundant command hint"
    );

    let narrow = frame_text(&mut state, 34, 13);
    assert!(narrow.contains("gpt-9-turbo"), "{narrow}");
    assert!(
        !narrow.contains("/ commands"),
        "the hint must be dropped, not wrapped: {narrow}"
    );
}

#[test]
fn clipboard_uses_bounded_base64_only_in_full_terminal_mode() {
    let mut full = Vec::new();
    assert_eq!(
        write_clipboard(
            &mut full,
            Chrome::new(RenderMode::Full),
            "answer\u{1b}]spoof"
        )
        .unwrap(),
        ClipboardOutcome::Emitted
    );
    let bytes = String::from_utf8(full).unwrap();
    assert!(bytes.starts_with("\u{1b}]52;c;"));
    assert!(
        !bytes.contains("spoof"),
        "payload must be encoded: {bytes:?}"
    );

    let mut flat = Vec::new();
    assert_eq!(
        write_clipboard(&mut flat, Chrome::new(RenderMode::Flat), "answer").unwrap(),
        ClipboardOutcome::UnsupportedPresentation
    );
    assert!(flat.is_empty());
}
