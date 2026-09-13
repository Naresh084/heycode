//! U20: which terminal capability tier each documented signal decides, and
//! what happens to a signal that was never found.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_ui::terminal::{
    ColorLevel, ColorReason, NARROW_COLUMNS, RenderMode, Support, TerminalCapabilities,
    TerminalEnvironment, WidthClass, no_color_suppresses,
};

/// A terminal that says nothing at all, so each test states only its own signal.
fn env() -> TerminalEnvironment {
    TerminalEnvironment::new()
        .with_term(Some("xterm"))
        .with_columns(Some(120))
}

#[test]
fn colorterm_truecolor_is_the_only_evidence_that_selects_the_24_bit_tier() {
    for value in ["truecolor", "24bit"] {
        let caps = TerminalCapabilities::detect(&env().with_colorterm(Some(value)));
        assert_eq!(caps.truecolor(), Support::Supported, "COLORTERM={value}");
        assert_eq!(caps.color(), ColorLevel::TrueColor, "COLORTERM={value}");
        assert_eq!(caps.color_reason(), ColorReason::Detected);
    }
    let without = TerminalCapabilities::detect(&env());
    assert_eq!(without.truecolor(), Support::Unknown);
    assert_ne!(without.color(), ColorLevel::TrueColor);
}

#[test]
fn a_colorterm_that_merely_contains_truecolor_is_not_evidence() {
    // A substring inside an arbitrary value is a coincidence, not a terminal
    // advertising itself, so it must stay Unknown rather than promote.
    let caps = TerminalCapabilities::detect(&env().with_colorterm(Some("nottruecolor")));
    assert_eq!(caps.truecolor(), Support::Unknown);
    assert_ne!(caps.color(), ColorLevel::TrueColor);
}

#[test]
fn the_term_256color_suffix_selects_the_256_tier() {
    for term in ["xterm-256color", "screen-256color", "tmux-256color"] {
        let caps = TerminalCapabilities::detect(&env().with_term(Some(term)));
        assert_eq!(caps.ansi256(), Support::Supported, "TERM={term}");
        assert_eq!(caps.color(), ColorLevel::Ansi256, "TERM={term}");
        assert_eq!(caps.color_reason(), ColorReason::Detected, "TERM={term}");
    }
}

#[test]
fn truecolor_evidence_outranks_256_evidence() {
    let caps = TerminalCapabilities::detect(
        &env()
            .with_term(Some("xterm-256color"))
            .with_colorterm(Some("truecolor")),
    );
    assert_eq!(caps.color(), ColorLevel::TrueColor);
}

#[test]
fn an_undetermined_capability_renders_at_basic_and_never_above() {
    // `xterm` advertises neither signal. Both capabilities must read Unknown,
    // and the rendered tier must be below both tiers whose misuse garbles.
    let caps = TerminalCapabilities::detect(&env());
    assert_eq!(caps.truecolor(), Support::Unknown);
    assert_eq!(caps.ansi256(), Support::Unknown);
    assert_eq!(caps.color(), ColorLevel::Basic);
    assert_eq!(caps.color_reason(), ColorReason::Fallback);
    assert!(caps.color() < ColorLevel::Ansi256);
}

#[test]
fn term_dumb_renders_flat_and_colourless() {
    let caps = TerminalCapabilities::detect(&env().with_term(Some("dumb")));
    assert_eq!(caps.render(), RenderMode::Flat);
    assert_eq!(caps.color(), ColorLevel::None);
    assert_eq!(caps.color_reason(), ColorReason::Dumb);
    assert_eq!(caps.truecolor(), Support::Unsupported);
    assert_eq!(caps.ansi256(), Support::Unsupported);
}

#[test]
fn term_dumb_outranks_a_colorterm_truecolor_claim() {
    // The `dumb` terminfo entry declares no `colors#` at all. Contradictory
    // evidence resolves to the safe reading, not the flattering one.
    let caps = TerminalCapabilities::detect(
        &env()
            .with_term(Some("dumb"))
            .with_colorterm(Some("truecolor")),
    );
    assert_eq!(caps.truecolor(), Support::Unsupported);
    assert_eq!(caps.color(), ColorLevel::None);
    assert_eq!(caps.color_reason(), ColorReason::Dumb);
}

#[test]
fn an_absent_or_empty_term_is_flat_because_nothing_is_known() {
    for term in [None, Some(String::new())] {
        let caps = TerminalCapabilities::detect(&env().with_term(term.clone()));
        assert_eq!(caps.render(), RenderMode::Flat, "TERM={term:?}");
        assert_eq!(caps.color(), ColorLevel::None, "TERM={term:?}");
    }
    assert_eq!(
        TerminalCapabilities::detect(&env()).render(),
        RenderMode::Full,
        "a named terminal must still get the full renderer"
    );
}

#[test]
fn no_color_suppresses_only_when_it_is_not_an_empty_string() {
    // <https://no-color.org/>: "when present and not an empty string
    // (regardless of its value)". An empty value is deliberately not a
    // suppression, and this is the rule most implementations get wrong.
    assert!(!no_color_suppresses(&env().with_no_color(Some(""))));
    assert!(!no_color_suppresses(&env().with_no_color(None::<String>)));
    assert!(no_color_suppresses(&env().with_no_color(Some("1"))));
    assert!(no_color_suppresses(&env().with_no_color(Some("0"))));

    let empty = TerminalCapabilities::detect(&env().with_no_color(Some("")));
    assert_eq!(empty.color(), ColorLevel::Basic);
    assert_eq!(empty.color_reason(), ColorReason::Fallback);
}

#[test]
fn no_color_removes_the_colour_without_denying_the_capability() {
    // A user preference is not a terminal fact. The tier goes to None, but the
    // evidence still says this terminal can do 24-bit.
    let caps = TerminalCapabilities::detect(
        &env()
            .with_no_color(Some("1"))
            .with_colorterm(Some("truecolor")),
    );
    assert_eq!(caps.color(), ColorLevel::None);
    assert_eq!(caps.color_reason(), ColorReason::Suppressed);
    assert_eq!(caps.truecolor(), Support::Supported);
    assert_eq!(
        caps.render(),
        RenderMode::Full,
        "suppressing colour is not a reason to stop painting frames"
    );
}

#[test]
fn narrow_is_decided_at_the_eighty_column_boundary() {
    let narrow = TerminalCapabilities::detect(&env().with_columns(Some(NARROW_COLUMNS - 1)));
    assert_eq!(narrow.width(), WidthClass::Narrow);
    let normal = TerminalCapabilities::detect(&env().with_columns(Some(NARROW_COLUMNS)));
    assert_eq!(normal.width(), WidthClass::Normal);
}

#[test]
fn an_unmeasured_width_degrades_to_narrow() {
    // The wide layout is the claim, so it is the one that needs evidence.
    let caps = TerminalCapabilities::detect(&env().with_columns(None));
    assert_eq!(caps.width(), WidthClass::Narrow);
    assert_eq!(caps.columns(), None);
}
