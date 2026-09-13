//! U20 terminal adapter: the only place that turns UI-neutral capability and
//! theme values into ratatui styles, crossterm key events into chords, and a
//! width budget into a status line that degrades instead of wrapping.
//!
//! Everything that decides anything lives in `heycode-ui`. This module translates.

use base64::Engine as _;
use heycode_ui::keymap::{KeyChord, KeyName, Modifiers};
use heycode_ui::terminal::{ColorLevel, RenderMode, TerminalCapabilities, TerminalEnvironment};
use heycode_ui::theme::{ResolvedTheme, TerminalColor, ThemeRole};
use ratatui::style::Color;

/// Read the live environment and terminal size into a capability snapshot.
///
/// The one impure function in the U20 path; every rule it feeds is pure.
#[must_use]
pub fn detect_environment() -> TerminalEnvironment {
    TerminalEnvironment::new()
        .with_no_color(std::env::var("NO_COLOR").ok())
        .with_colorterm(std::env::var("COLORTERM").ok())
        .with_term(std::env::var("TERM").ok())
        .with_columns(crossterm::terminal::size().ok().map(|(columns, _)| columns))
}

/// Product-neutral choice of terminal presentation.
///
/// [`Self::ScreenReader`] is explicit input from the shell owner. It is not an
/// environment test hook and it does not change command/dialog state; it only
/// chooses the flat, colorless, motionless projection of that state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TuiDisplayMode {
    /// Resolve ordinary capability evidence from the supplied environment.
    #[default]
    Automatic,
    /// Force flat output with no alternate screen, cursor addressing, color,
    /// or animation.
    ScreenReader,
}

impl TuiDisplayMode {
    /// Resolve one immutable capability snapshot.
    #[must_use]
    pub fn resolve(self, environment: &TerminalEnvironment) -> TerminalCapabilities {
        match self {
            Self::Automatic => TerminalCapabilities::detect(environment),
            Self::ScreenReader => TerminalCapabilities::detect(
                &TerminalEnvironment::new()
                    .with_no_color(Some("1"))
                    .with_term(Some("dumb"))
                    .with_columns(environment.columns()),
            ),
        }
    }
}

/// Render one resolved colour.
///
/// [`TerminalColor::Rgb`] is only ever produced at [`ColorLevel::TrueColor`],
/// so this function cannot emit a 24-bit colour for a terminal that did not
/// advertise one.
#[must_use]
pub const fn color(resolved: TerminalColor) -> Color {
    match resolved {
        TerminalColor::Rgb(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
        TerminalColor::Indexed(index) => Color::Indexed(index),
        TerminalColor::Default => Color::Reset,
    }
}

/// Infer a light terminal surface from a concrete dark text colour.
pub(crate) fn foreground_is_dark(color: Color) -> bool {
    let rgb = match color {
        Color::Rgb(red, green, blue) => Some((red, green, blue)),
        Color::Indexed(index @ 16..=231) => {
            let index = index - 16;
            let component = |slot: u8| [0, 95, 135, 175, 215, 255][usize::from(slot)];
            Some((
                component(index / 36),
                component((index % 36) / 6),
                component(index % 6),
            ))
        }
        Color::Indexed(index @ 232..=255) => {
            let value = 8_u8.saturating_add((index - 232).saturating_mul(10));
            Some((value, value, value))
        }
        Color::Black | Color::DarkGray => Some((0, 0, 0)),
        _ => None,
    };
    rgb.is_some_and(|(red, green, blue)| {
        (u32::from(red) * 299 + u32::from(green) * 587 + u32::from(blue) * 114) / 1000 < 128
    })
}

/// Every role as a ratatui colour, for one theme at one tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Styles {
    accent: Color,
    code: Color,
    level: ColorLevel,
    success: Color,
    error: Color,
    warn: Color,
    text: Color,
    dim: Color,
    border: Color,
    prompt_background: Color,
    prompt_glyph: Color,
    panel_title: Color,
}

impl Styles {
    /// Project a resolved theme.
    #[must_use]
    pub fn new(theme: &ResolvedTheme) -> Self {
        Self {
            accent: color(theme.color(ThemeRole::Accent)),
            code: color(theme.color(ThemeRole::Code)),
            level: theme.level(),
            success: color(theme.color(ThemeRole::Success)),
            error: color(theme.color(ThemeRole::Error)),
            warn: color(theme.color(ThemeRole::Warn)),
            text: color(theme.color(ThemeRole::Text)),
            dim: color(theme.color(ThemeRole::Dim)),
            border: color(theme.color(ThemeRole::Border)),
            prompt_background: color(theme.color(ThemeRole::PromptBackground)),
            prompt_glyph: color(theme.color(ThemeRole::PromptGlyph)),
            panel_title: color(theme.color(ThemeRole::PanelTitle)),
        }
    }

    /// Primary accent.
    #[must_use]
    pub const fn accent(&self) -> Color {
        self.accent
    }

    /// Task and workflow panel heading.
    #[must_use]
    pub const fn panel_title(&self) -> Color {
        self.panel_title
    }

    /// Inline code and references.
    #[must_use]
    pub const fn code(&self) -> Color {
        self.code
    }

    /// Terminal color capability used by this palette.
    #[must_use]
    pub const fn level(&self) -> ColorLevel {
        self.level
    }

    /// Successful outcome.
    #[must_use]
    pub const fn success(&self) -> Color {
        self.success
    }

    /// Failed outcome.
    #[must_use]
    pub const fn error(&self) -> Color {
        self.error
    }

    /// Foreground for the marker and number on a changed diff row.
    #[must_use]
    pub fn diff_marker(&self, added: bool) -> Color {
        let light = foreground_is_dark(self.text);
        match self.level {
            ColorLevel::None => Color::Reset,
            ColorLevel::Basic => {
                if added {
                    self.success
                } else {
                    self.error
                }
            }
            ColorLevel::Ansi256 | ColorLevel::TrueColor if light => {
                // Claude 2.1.269 light Edit approval, captured 2026-09-12:
                // added #248a3d, removed #cf222e. Lower tiers are quantized.
                let rgb = if added {
                    heycode_ui::theme::Rgb::new(0x24, 0x8a, 0x3d)
                } else {
                    heycode_ui::theme::Rgb::new(0xcf, 0x22, 0x2e)
                };
                if self.level == ColorLevel::Ansi256 {
                    Color::Indexed(heycode_ui::theme::quantize_256(rgb))
                } else {
                    Color::Rgb(rgb.r, rgb.g, rgb.b)
                }
            }
            ColorLevel::Ansi256 | ColorLevel::TrueColor => {
                Color::Indexed(if added { 77 } else { 167 })
            }
        }
    }

    /// Background used for removed and inserted diff rows.
    #[must_use]
    pub fn diff_background(&self, added: bool) -> Color {
        let light = foreground_is_dark(self.text);
        match self.level {
            // Basic text uses the terminal's own foreground. Its unknown RGB
            // cannot be paired safely with a fixed red or green background.
            ColorLevel::None | ColorLevel::Basic => Color::Reset,
            ColorLevel::Ansi256 | ColorLevel::TrueColor if light => {
                // Same source capture: added #dcffdc, removed #ffdcdc.
                let rgb = if added {
                    heycode_ui::theme::Rgb::new(0xdc, 0xff, 0xdc)
                } else {
                    heycode_ui::theme::Rgb::new(0xff, 0xdc, 0xdc)
                };
                if self.level == ColorLevel::Ansi256 {
                    Color::Indexed(heycode_ui::theme::quantize_256(rgb))
                } else {
                    Color::Rgb(rgb.r, rgb.g, rgb.b)
                }
            }
            ColorLevel::Ansi256 | ColorLevel::TrueColor => {
                Color::Indexed(if added { 22 } else { 52 })
            }
        }
    }

    /// Companion ink follows the selected theme and terminal color tier.
    #[must_use]
    pub const fn companion(&self) -> Color {
        self.accent
    }

    /// Cautionary state.
    #[must_use]
    pub const fn warn(&self) -> Color {
        self.warn
    }

    /// Body text.
    #[must_use]
    pub const fn text(&self) -> Color {
        self.text
    }

    /// Secondary text.
    #[must_use]
    pub const fn dim(&self) -> Color {
        self.dim
    }

    /// Borders and chrome.
    #[must_use]
    pub const fn border(&self) -> Color {
        self.border
    }

    /// Background for a submitted prompt or focused transcript row.
    #[must_use]
    pub const fn prompt_background(&self) -> Color {
        self.prompt_background
    }

    /// The `❯` marker inside a prompt band.
    #[must_use]
    pub const fn prompt_glyph(&self) -> Color {
        self.prompt_glyph
    }
}

impl Default for Styles {
    /// The built-in theme at the 24-bit tier, matching [`crate::palette`].
    fn default() -> Self {
        heycode_ui::theme::default_theme().map_or(
            Self {
                accent: Color::Reset,
                code: Color::Reset,
                level: ColorLevel::None,
                success: Color::Reset,
                error: Color::Reset,
                warn: Color::Reset,
                text: Color::Reset,
                dim: Color::Reset,
                border: Color::Reset,
                prompt_background: Color::Reset,
                prompt_glyph: Color::Reset,
                panel_title: Color::Reset,
            },
            |theme| Self::new(&theme.resolve(ColorLevel::TrueColor)),
        )
    }
}

/// What decoration a terminal can carry.
///
/// A `dumb` terminal has no `cup`, so anything that assumes cursor addressing
/// is not merely ugly there — it is a garbled frame. Every such decision reads
/// this one value.
///
/// One stored fact, three named questions. Three independent flags would be
/// three fields that can never disagree, which is a rule no test could
/// distinguish (GOTCHAS #160); the accessors exist so each call site reads as
/// the question it is actually asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chrome {
    mode: RenderMode,
    input: InputProtocol,
}

/// What the terminal can be *asked* about input, which is a different
/// question from what may be drawn.
///
/// `--screen-reader` on an ordinary terminal draws flat because the user
/// asked for flat, not because the terminal cannot address a cursor: it can
/// still report a bracketed paste, and without that a pasted block arrives as
/// keystrokes whose first newline sends line one as a prompt. A genuinely
/// `dumb` terminal is asked nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputProtocol {
    /// The terminal understands input-mode escapes (bracketed paste).
    Escapes,
    /// Nothing may be written to this terminal, including input-mode requests.
    PlainKeys,
}

impl Chrome {
    /// Decide chrome from the render mode.
    ///
    /// A flat render mode here means the terminal itself is flat, so no input
    /// escapes are sent either; [`Self::with_input`] states otherwise.
    #[must_use]
    pub const fn new(mode: RenderMode) -> Self {
        Self {
            mode,
            input: match mode {
                RenderMode::Full => InputProtocol::Escapes,
                RenderMode::Flat => InputProtocol::PlainKeys,
            },
        }
    }

    /// The same decoration with an explicit input protocol.
    #[must_use]
    pub const fn with_input(self, input: InputProtocol) -> Self {
        Self {
            mode: self.mode,
            input,
        }
    }

    /// What may be asked of this terminal's input.
    #[must_use]
    pub const fn input(&self) -> InputProtocol {
        self.input
    }

    /// Whether bracketed paste may be enabled, so a pasted block arrives as
    /// one event instead of a burst of keystrokes.
    #[must_use]
    pub const fn bracketed_paste(&self) -> bool {
        matches!(self.input, InputProtocol::Escapes)
    }

    const fn full(self) -> bool {
        matches!(self.mode, RenderMode::Full)
    }

    /// Whether boxes may be drawn.
    #[must_use]
    pub const fn borders(&self) -> bool {
        self.full()
    }

    /// Whether the alternate screen may be entered.
    #[must_use]
    pub const fn alternate_screen(&self) -> bool {
        self.full()
    }

    /// Whether the spinner may animate.
    #[must_use]
    pub const fn animation(&self) -> bool {
        self.full()
    }
}

impl Default for Chrome {
    fn default() -> Self {
        Self::new(RenderMode::Full)
    }
}

/// Outcome of one explicit `/copy` terminal clipboard request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardOutcome {
    /// The native clipboard accepted text or OSC 52 was emitted to a full terminal.
    Emitted,
    /// Flat/screen-reader presentation forbids terminal control bytes.
    UnsupportedPresentation,
    /// The completed answer exceeded the bounded clipboard payload.
    TooLarge,
}

/// Maximum answer bytes admitted to one OSC 52 clipboard operation.
pub const MAX_CLIPBOARD_BYTES: usize = 64 * 1024;

/// Emit an explicit terminal clipboard request without invoking a host process.
///
/// Flat/screen-reader output writes zero bytes. The input is base64 encoded,
/// so assistant-authored terminal controls cannot escape the OSC payload.
///
/// # Errors
/// Writer failures are returned after no success is reported.
pub fn write_clipboard(
    writer: &mut impl std::io::Write,
    chrome: Chrome,
    text: &str,
) -> std::io::Result<ClipboardOutcome> {
    if !chrome.alternate_screen() {
        return Ok(ClipboardOutcome::UnsupportedPresentation);
    }
    if text.len() > MAX_CLIPBOARD_BYTES {
        return Ok(ClipboardOutcome::TooLarge);
    }
    let encoded = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    writer.write_all(b"\x1b]52;c;")?;
    writer.write_all(encoded.as_bytes())?;
    writer.write_all(b"\x07")?;
    writer.flush()?;
    Ok(ClipboardOutcome::Emitted)
}

/// Copy through the local macOS pasteboard, or the terminal for remote/other hosts.
/// The pure OSC writer remains separate so rendering/tests never mutate a host clipboard.
pub(crate) async fn copy_to_clipboard(
    subprocess: &heycode_exec::SubprocessService,
    writer: &mut impl std::io::Write,
    chrome: Chrome,
    text: &str,
) -> std::io::Result<ClipboardOutcome> {
    if !chrome.alternate_screen() || text.len() > MAX_CLIPBOARD_BYTES {
        return write_clipboard(writer, chrome, text);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = subprocess;
    #[cfg(target_os = "macos")]
    if std::env::var_os("SSH_CONNECTION").is_none() && std::env::var_os("SSH_TTY").is_none() {
        let copy = async {
            let executable = subprocess.resolve_program(std::ffi::OsStr::new("/usr/bin/pbcopy"))?;
            let spec = heycode_exec::ProcessSpec::new(executable, "/")?
                .with_interactive_stdio()
                .with_environment(heycode_exec::safe_environment_snapshot())?
                .with_timeout(Some(std::time::Duration::from_secs(2)))?;
            let process = subprocess
                .spawn_interactive(spec, tokio_util::sync::CancellationToken::new())
                .await?;
            let (process, mut input, _output) = process.into_parts();
            input.write(text.as_bytes()).await?;
            input.finish().await?;
            process.wait().await.map(|status| status.is_success())
        };
        if matches!(
            tokio::time::timeout(std::time::Duration::from_secs(2), copy).await,
            Ok(Ok(true))
        ) {
            return Ok(ClipboardOutcome::Emitted);
        }
    }
    write_clipboard(writer, chrome, text)
}

/// Enter the terminal presentation selected by `chrome`.
///
/// Flat mode deliberately writes zero bytes. Raw-mode configuration is an OS
/// terminal ioctl owned by the caller and is not part of this byte stream.
///
/// # Errors
/// Returns an output error while entering the alternate screen.
pub fn enter_terminal_screen(
    writer: &mut impl std::io::Write,
    chrome: Chrome,
) -> std::io::Result<()> {
    if chrome.alternate_screen() {
        crossterm::execute!(
            writer,
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture,
            crossterm::event::EnableFocusChange,
            // Lets a terminal that speaks the kitty keyboard protocol report
            // Shift+Enter distinctly from Enter; others ignore the request.
            crossterm::event::PushKeyboardEnhancementFlags(
                crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
            )
        )?;
    }
    if chrome.bracketed_paste() {
        // Bracketed paste makes a pasted block one `Event::Paste` instead of a
        // burst of keystrokes whose first newline would send the draft. It is
        // an input-mode request, not decoration, so a flat screen-reader frame
        // on a capable terminal gets it too.
        crossterm::execute!(writer, crossterm::event::EnableBracketedPaste)?;
    }
    Ok(())
}

/// Restore the terminal presentation selected by `chrome`.
///
/// Flat mode deliberately writes zero bytes, including no cursor-show escape,
/// because the flat renderer never hid or addressed the cursor.
///
/// # Errors
/// Returns an output error while leaving the alternate screen.
pub fn leave_terminal_screen(
    writer: &mut impl std::io::Write,
    chrome: Chrome,
) -> std::io::Result<()> {
    // Independent attempts ensure a transient failure disabling one protocol
    // does not skip the others and leave the shell receiving mouse reports.
    let mut first_error = None;
    macro_rules! restore {
        ($command:expr) => {
            if let Err(error) = crossterm::execute!(writer, $command) {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        };
    }
    if chrome.bracketed_paste() {
        restore!(crossterm::event::DisableBracketedPaste);
    }
    if chrome.alternate_screen() {
        restore!(crossterm::event::DisableMouseCapture);
        restore!(crossterm::event::DisableFocusChange);
        restore!(crossterm::event::PopKeyboardEnhancementFlags);
        restore!(crossterm::terminal::LeaveAlternateScreen);
        restore!(crossterm::cursor::Show);
    }
    first_error.map_or(Ok(()), Err)
}

/// Restore input protocols through an independent nonblocking terminal handle.
///
/// Cleanup must not block OS raw-mode restoration when a terminal stops reading.
/// Empty output (the flat/plain-key contract) does not open the terminal.
///
/// # Errors
/// Returns a terminal open/write error or a bounded drain timeout.
pub fn restore_terminal_output(bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if bytes.is_empty() {
        return Ok(());
    }
    let mut terminal = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(nix::libc::O_NONBLOCK)
        .open("/dev/tty")?;
    write_cleanup_output(&mut terminal, bytes, std::time::Duration::from_millis(250))
}

fn write_cleanup_output(
    writer: &mut impl std::io::Write,
    mut bytes: &[u8],
    budget: std::time::Duration,
) -> std::io::Result<()> {
    let started = std::time::Instant::now();
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(count) => bytes = &bytes[count..],
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        if bytes.is_empty() {
            break;
        }
        let remaining = budget
            .checked_sub(started.elapsed())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "terminal cleanup output did not drain",
                )
            })?;
        std::thread::sleep(remaining.min(std::time::Duration::from_millis(2)));
    }
    Ok(())
}

/// The chord a key event represents, when this build can express it.
///
/// A key carrying a modifier the chord grammar has no name for returns `None`
/// rather than a chord with that modifier dropped: silently ignoring `super`
/// would make Super+P fire the plain Ctrl+P binding.
#[must_use]
pub fn chord(event: crossterm::event::KeyEvent) -> Option<KeyChord> {
    use crossterm::event::{KeyCode, KeyModifiers};
    let unnameable = KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META;
    if event.modifiers.intersects(unnameable) {
        return None;
    }
    let key = match event.code {
        KeyCode::Char(character) => KeyName::Char(character),
        KeyCode::Enter => KeyName::Enter,
        KeyCode::Esc => KeyName::Esc,
        KeyCode::Tab => KeyName::Tab,
        KeyCode::BackTab => KeyName::BackTab,
        KeyCode::Backspace => KeyName::Backspace,
        KeyCode::Delete => KeyName::Delete,
        KeyCode::Insert => KeyName::Insert,
        KeyCode::Up => KeyName::Up,
        KeyCode::Down => KeyName::Down,
        KeyCode::Left => KeyName::Left,
        KeyCode::Right => KeyName::Right,
        KeyCode::Home => KeyName::Home,
        KeyCode::End => KeyName::End,
        KeyCode::PageUp => KeyName::PageUp,
        KeyCode::PageDown => KeyName::PageDown,
        KeyCode::F(number) if (1..=heycode_ui::keymap::MAX_FUNCTION_KEY).contains(&number) => {
            KeyName::Function(number)
        }
        _ => return None,
    };
    Some(KeyChord::new(
        key,
        Modifiers {
            ctrl: event.modifiers.contains(KeyModifiers::CONTROL),
            alt: event.modifiers.contains(KeyModifiers::ALT),
            shift: event.modifiers.contains(KeyModifiers::SHIFT),
        },
    ))
}

/// One status-line field with the priority that decides what survives a narrow
/// terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusField {
    /// Rendered text, already including any leading separator.
    pub text: String,
    /// Which role colours it.
    pub role: ThemeRole,
    /// Higher survives longer. Ties keep the earlier field.
    pub priority: u8,
}

impl StatusField {
    /// One field.
    #[must_use]
    pub fn new(text: impl Into<String>, role: ThemeRole, priority: u8) -> Self {
        Self {
            text: text.into(),
            role,
            priority,
        }
    }
}

/// Display width of text, in terminal columns.
#[must_use]
pub fn width_of(text: &str) -> usize {
    unicode_width::UnicodeWidthStr::width(text)
}

/// Cut text to `columns`, marking the cut with `…`.
///
/// Cuts on character boundaries and accounts for wide characters, so the
/// result never overflows its budget and never splits a character.
#[must_use]
pub fn truncate_to_width(text: &str, columns: usize) -> String {
    if width_of(text) <= columns {
        return text.to_owned();
    }
    if columns == 0 {
        return String::new();
    }
    // The marker occupies one column of the budget.
    let budget = columns - 1;
    let mut kept = String::new();
    let mut used = 0;
    for character in text.chars() {
        let character_width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > budget {
            break;
        }
        used += character_width;
        kept.push(character);
    }
    kept.push('…');
    kept
}

/// Drop the lowest-priority fields until the line fits, then truncate.
///
/// Never wraps: a wrapped status line pushes the composer off screen, which is
/// exactly the "narrow terminal renders nonsense" failure. Fields are returned
/// in their original order.
#[must_use]
pub fn fit_status(fields: &[StatusField], columns: u16) -> Vec<StatusField> {
    let columns = usize::from(columns);
    let mut kept: Vec<usize> = (0..fields.len()).collect();
    // Bounded by the field count: each pass removes exactly one field, so a
    // mutation that stops the loop shrinking fails rather than hangs.
    for _ in 0..fields.len() {
        let total: usize = kept
            .iter()
            .map(|index| width_of(&fields[*index].text))
            .sum();
        if total <= columns || kept.len() <= 1 {
            break;
        }
        let Some((position, _)) = kept
            .iter()
            .enumerate()
            .min_by_key(|(position, index)| (fields[**index].priority, usize::MAX - position))
        else {
            break;
        };
        kept.remove(position);
    }
    let mut remaining = columns;
    kept.into_iter()
        .map(|index| {
            let field = &fields[index];
            let text = truncate_to_width(&field.text, remaining);
            remaining = remaining.saturating_sub(width_of(&text));
            StatusField {
                text,
                role: field.role,
                priority: field.priority,
            }
        })
        .filter(|field| !field.text.is_empty())
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod diff_palette_tests {
    #[test]
    fn terminal_cleanup_backpressure_has_a_deadline() {
        struct Blocked;
        impl std::io::Write for Blocked {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::WouldBlock.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let result = super::write_cleanup_output(&mut Blocked, b"reset", std::time::Duration::ZERO);
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
    }

    #[test]
    fn terminal_cleanup_keeps_partial_write_offsets() {
        struct Partial {
            bytes: Vec<u8>,
            pause: bool,
        }
        impl std::io::Write for Partial {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.pause = !self.pause;
                if self.pause {
                    return Err(std::io::ErrorKind::WouldBlock.into());
                }
                self.bytes.push(bytes[0]);
                Ok(1)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let mut writer = Partial {
            bytes: Vec::new(),
            pause: false,
        };
        super::write_cleanup_output(&mut writer, b"reset", std::time::Duration::from_secs(1))
            .unwrap();
        assert_eq!(writer.bytes, b"reset");
    }

    use super::*;

    fn styles(id: &str, tier: ColorLevel) -> Styles {
        let theme = heycode_ui::theme::builtin_themes()
            .unwrap()
            .into_iter()
            .find(|theme| theme.id().as_str() == id)
            .unwrap();
        Styles::new(&theme.resolve(tier))
    }

    #[test]
    fn light_diff_matches_captured_source_colors_and_quantizes_at_256() {
        let light = styles("heycode-light", ColorLevel::TrueColor);
        assert_eq!(light.diff_background(true), Color::Rgb(0xdc, 0xff, 0xdc));
        assert_eq!(light.diff_background(false), Color::Rgb(0xff, 0xdc, 0xdc));
        assert_eq!(light.diff_marker(true), Color::Rgb(0x24, 0x8a, 0x3d));
        assert_eq!(light.diff_marker(false), Color::Rgb(0xcf, 0x22, 0x2e));
        let ansi = styles("heycode-light", ColorLevel::Ansi256);
        assert_eq!(ansi.diff_background(true), Color::Indexed(194));
        assert_eq!(ansi.diff_background(false), Color::Indexed(224));
        assert_eq!(ansi.diff_marker(true), Color::Indexed(29));
        assert_eq!(ansi.diff_marker(false), Color::Indexed(160));
    }

    #[test]
    fn dark_diff_retains_source_indices_and_basic_preserves_terminal_background() {
        for tier in [ColorLevel::Ansi256, ColorLevel::TrueColor] {
            let dark = styles("heycode-dark", tier);
            assert_eq!(dark.diff_background(true), Color::Indexed(22));
            assert_eq!(dark.diff_background(false), Color::Indexed(52));
            assert_eq!(dark.diff_marker(true), Color::Indexed(77));
            assert_eq!(dark.diff_marker(false), Color::Indexed(167));
        }
        for id in ["heycode-dark", "heycode-light"] {
            let basic = styles(id, ColorLevel::Basic);
            assert_eq!(basic.diff_background(true), Color::Reset);
            assert_eq!(basic.diff_background(false), Color::Reset);
            assert_eq!(basic.diff_marker(true), basic.success());
            assert_eq!(basic.diff_marker(false), basic.error());
            let none = styles(id, ColorLevel::None);
            assert_eq!(none.diff_background(true), Color::Reset);
            assert_eq!(none.diff_marker(false), Color::Reset);
        }
    }
}
