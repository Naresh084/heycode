//! U20 terminal capability tiers, decided from evidence.
//!
//! Detection is a pure function of an environment snapshot, so the four tiers
//! the acceptance criterion names — truecolor, 256, dumb, narrow — are values a
//! test asserts rather than frames a test squints at.
//!
//! The governing rule is that **a capability we could not determine is
//! [`Support::Unknown`], and `Unknown` never becomes `Supported`**. A terminal
//! that cannot do 24-bit must not be handed 24-bit escapes, so the effective
//! tier is the highest one with positive evidence, never the highest one that
//! has not been ruled out.
//!
//! ## Signals, and where each comes from
//!
//! - `NO_COLOR`: <https://no-color.org/> — "Command-line software which adds
//!   ANSI color to its output by default should check for a `NO_COLOR`
//!   environment variable that, **when present and not an empty string**
//!   (regardless of its value), prevents the addition of ANSI color."
//!   An **empty** `NO_COLOR` therefore does **not** suppress colour; see
//!   [`no_color_suppresses`].
//! - `COLORTERM`: the de-facto convention documented at
//!   <https://github.com/termstandard/colors> — "VTE, Konsole and iTerm2 all
//!   advertise truecolor support by placing `COLORTERM=truecolor` in the
//!   environment", and the S-Lang library additionally recognises `24bit`.
//! - `TERM=*-256color`: the terminfo database's own naming convention rather
//!   than a specification. Verified against the entries on this host:
//!   `xterm-256color`, `screen-256color` and `tmux-256color` each declare
//!   `colors#256`, while plain `xterm` declares `colors#8`.
//! - `TERM=dumb`: the terminfo `dumb` entry is `dumb|80-column dumb tty` and
//!   declares only `am`, `cols#80`, `bel`, `cr`, `cud1` and `ind`. It has no
//!   `colors#` **and no `cup`**, so such a terminal can neither colour nor
//!   address the cursor — which is why it gets [`RenderMode::Flat`] and not
//!   merely a colourless frame.
//!
//! Anything outside that list is `Unknown`. `Unknown` renders at
//! [`ColorLevel::Basic`], deliberately: `Basic` is the SGR 30–37/40–47 set from
//! ECMA-48, it is *below* both tiers whose misuse garbles a frame, and it is
//! not a claim that any particular capability was detected — see
//! [`ColorReason::Fallback`].

/// Three-state evidence about one terminal capability.
///
/// `Unknown` is not a synonym for `Unsupported`: one says nothing was found,
/// the other says absence was established. They differ in what a future probe
/// is allowed to change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Support {
    /// A documented signal positively advertised this capability.
    Supported,
    /// A documented signal established that the capability is absent.
    Unsupported,
    /// Nothing was found either way. Never rendered as `Supported`.
    Unknown,
}

impl Support {
    /// Whether this evidence permits using the capability.
    ///
    /// Exactly one variant does. `Unknown` does not, which is the whole point.
    #[must_use]
    pub const fn permits(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Effective colour depth, lowest tier first.
///
/// Ordered so "never render above the detected tier" is an ordering fact rather
/// than a convention someone has to remember.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ColorLevel {
    /// No colour at all; every role renders as the terminal's own default.
    None,
    /// The ECMA-48 SGR 30–37/40–47 set. Also the fallback for `Unknown`.
    Basic,
    /// The 256-entry indexed palette.
    Ansi256,
    /// 24-bit direct colour.
    TrueColor,
}

impl ColorLevel {
    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Basic => "basic",
            Self::Ansi256 => "256",
            Self::TrueColor => "truecolor",
        }
    }
}

/// Why the effective [`ColorLevel`] is what it is.
///
/// Kept separate from the capability evidence because "the user asked for no
/// colour" and "this terminal cannot colour" are different statements, and a
/// boundary that reports one as the other lies about the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorReason {
    /// A documented signal advertised this exact tier.
    Detected,
    /// `NO_COLOR` is set to a non-empty value. A user preference, not a
    /// capability: the terminal's own evidence is left untouched.
    Suppressed,
    /// `TERM` names a terminal that declares no colour capability.
    Dumb,
    /// Nothing was determined, so the tier below both garbling tiers is used.
    Fallback,
}

/// Whether a frame can be painted, or only lines emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RenderMode {
    /// Cursor addressing is available: borders, overlays and animation are safe.
    Full,
    /// No cursor addressing. Flat line output, no chrome, no animation.
    Flat,
}

/// Whether the terminal is wide enough for the full status/chrome layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WidthClass {
    /// Below [`NARROW_COLUMNS`], or unknown. Lower-priority content is dropped.
    Narrow,
    /// At least [`NARROW_COLUMNS`] columns.
    Normal,
}

/// The width at and above which the full layout is used.
///
/// 80 is the width the terminfo `dumb` entry itself declares (`cols#80`) and
/// the historical terminal standard, so it is the one threshold in this module
/// that is not arbitrary.
pub const NARROW_COLUMNS: u16 = 80;

/// The `COLORTERM` values that advertise 24-bit colour.
///
/// Exact values, not a substring search: an exact value is evidence that some
/// terminal set it deliberately, whereas a substring inside an arbitrary string
/// is a coincidence that would promote `Unknown` to `Supported`.
const TRUECOLOR_COLORTERM: [&str; 2] = ["truecolor", "24bit"];

/// The terminfo entry name for a terminal with no capabilities.
const DUMB_TERM: &str = "dumb";

/// The terminfo database's suffix for 256-colour entries.
const ANSI256_TERM_SUFFIX: &str = "-256color";

/// An environment snapshot to decide capabilities from.
///
/// Taking the values rather than reading the process environment is what makes
/// [`TerminalCapabilities::detect`] pure and every tier assertable without a
/// terminal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalEnvironment {
    no_color: Option<String>,
    colorterm: Option<String>,
    term: Option<String>,
    columns: Option<u16>,
}

impl TerminalEnvironment {
    /// An environment in which nothing is set and no size is known.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set `NO_COLOR`. `Some("")` models the variable being present but empty.
    #[must_use]
    pub fn with_no_color(mut self, value: Option<impl Into<String>>) -> Self {
        self.no_color = value.map(Into::into);
        self
    }

    /// Set `COLORTERM`.
    #[must_use]
    pub fn with_colorterm(mut self, value: Option<impl Into<String>>) -> Self {
        self.colorterm = value.map(Into::into);
        self
    }

    /// Set `TERM`.
    #[must_use]
    pub fn with_term(mut self, value: Option<impl Into<String>>) -> Self {
        self.term = value.map(Into::into);
        self
    }

    /// Set the measured column count.
    #[must_use]
    pub const fn with_columns(mut self, columns: Option<u16>) -> Self {
        self.columns = columns;
        self
    }

    /// Measured column count, when one was supplied.
    #[must_use]
    pub const fn columns(&self) -> Option<u16> {
        self.columns
    }
}

/// Whether `NO_COLOR` suppresses colour for this environment.
///
/// The published convention is "present **and not an empty string**", so an
/// empty `NO_COLOR` is deliberately *not* a suppression. This is one function
/// and one test so the rule has exactly one home.
#[must_use]
pub fn no_color_suppresses(env: &TerminalEnvironment) -> bool {
    env.no_color
        .as_deref()
        .is_some_and(|value| !value.is_empty())
}

/// The resolved capability tiers for one terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalCapabilities {
    truecolor: Support,
    ansi256: Support,
    color: ColorLevel,
    color_reason: ColorReason,
    render: RenderMode,
    width: WidthClass,
    columns: Option<u16>,
}

impl TerminalCapabilities {
    /// Decide every tier from one environment snapshot.
    #[must_use]
    pub fn detect(env: &TerminalEnvironment) -> Self {
        let term = env.term.as_deref().unwrap_or_default();
        // An absent or empty `TERM` names no terminfo entry, so nothing about
        // this terminal is known — including whether the cursor can be moved.
        // The flat tier is the only one that stays correct under that.
        let unnamed = term.is_empty();
        let dumb = term == DUMB_TERM;
        let render = if unnamed || dumb {
            RenderMode::Flat
        } else {
            RenderMode::Full
        };

        let (truecolor, ansi256) = if unnamed || dumb {
            // The `dumb` entry declares no `colors#` at all, and an unnamed
            // terminal declares nothing. Absence is established, not merely
            // unfound, so a `COLORTERM` claim does not override it.
            (Support::Unsupported, Support::Unsupported)
        } else {
            (
                if env
                    .colorterm
                    .as_deref()
                    .is_some_and(|value| TRUECOLOR_COLORTERM.contains(&value))
                {
                    Support::Supported
                } else {
                    Support::Unknown
                },
                if term.ends_with(ANSI256_TERM_SUFFIX) {
                    Support::Supported
                } else {
                    Support::Unknown
                },
            )
        };

        let (color, color_reason) = if no_color_suppresses(env) {
            // A user preference, so the capability evidence above is left
            // exactly as detected: this terminal may well do 24-bit.
            (ColorLevel::None, ColorReason::Suppressed)
        } else if unnamed || dumb {
            (ColorLevel::None, ColorReason::Dumb)
        } else if truecolor.permits() {
            (ColorLevel::TrueColor, ColorReason::Detected)
        } else if ansi256.permits() {
            (ColorLevel::Ansi256, ColorReason::Detected)
        } else {
            (ColorLevel::Basic, ColorReason::Fallback)
        };

        let width = match env.columns {
            Some(columns) if columns >= NARROW_COLUMNS => WidthClass::Normal,
            // An unknown width degrades for the same reason an unknown colour
            // capability does: the wide layout is the claim, so it needs
            // evidence.
            _ => WidthClass::Narrow,
        };

        Self {
            truecolor,
            ansi256,
            color,
            color_reason,
            render,
            width,
            columns: env.columns,
        }
    }

    /// Evidence about 24-bit colour, independent of user preference.
    #[must_use]
    pub const fn truecolor(&self) -> Support {
        self.truecolor
    }

    /// Evidence about the 256-entry indexed palette.
    #[must_use]
    pub const fn ansi256(&self) -> Support {
        self.ansi256
    }

    /// The tier a theme must resolve to.
    #[must_use]
    pub const fn color(&self) -> ColorLevel {
        self.color
    }

    /// Why [`Self::color`] is what it is.
    #[must_use]
    pub const fn color_reason(&self) -> ColorReason {
        self.color_reason
    }

    /// Whether a frame can be painted at all.
    #[must_use]
    pub const fn render(&self) -> RenderMode {
        self.render
    }

    /// Whether the full-width layout fits.
    #[must_use]
    pub const fn width(&self) -> WidthClass {
        self.width
    }

    /// Measured column count, when one was supplied.
    #[must_use]
    pub const fn columns(&self) -> Option<u16> {
        self.columns
    }
}

impl Default for TerminalCapabilities {
    /// The capabilities of a terminal nothing is known about.
    fn default() -> Self {
        Self::detect(&TerminalEnvironment::new())
    }
}
