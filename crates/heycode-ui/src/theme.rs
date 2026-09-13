//! U20 themes: a role palette, and the rule that resolves it for a terminal.
//!
//! A theme is authored once in 24-bit colour and *resolved* against a
//! [`ColorLevel`] before anything is drawn. That is the whole safety property:
//! a terminal that cannot do 24-bit is never handed 24-bit, because the only
//! way to obtain a drawable colour is [`Theme::resolve`], which takes the tier.
//!
//! Roles, not colours, are what a renderer names. `palette::ACCENT` in
//! `heycode-tui` is derived from [`HEYCODE_DARK`] rather than repeated, so the
//! workspace holds exactly one colour table.
//!
//! ## Tier behaviour
//!
//! | Tier | Result |
//! |---|---|
//! | [`ColorLevel::TrueColor`] | the authored [`Rgb`] |
//! | [`ColorLevel::Ansi256`] | [`quantize_256`] into indices 16–255 |
//! | [`ColorLevel::Basic`] | the role's semantic ANSI slot ([`basic_slot`]) |
//! | [`ColorLevel::None`] | [`TerminalColor::Default`] for every role |
//!
//! At [`ColorLevel::Basic`] the authored colours are deliberately **not**
//! quantized. ANSI indices 0–15 have no defined RGB values — every terminal
//! and every user theme assigns them differently — so "nearest colour" has no
//! meaning there. The role's semantic slot is the honest answer, and it also
//! means a user's own 16-colour scheme keeps working.
//!
//! For the same reason [`quantize_256`] targets only indices 16–255: the cube
//! and the grey ramp have published RGB values, while 0–15 do not.

use crate::terminal::ColorLevel;
use crate::{UiContributionId, UiRegistryError};

/// A 24-bit colour as authored by a theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Rgb {
    /// Red component.
    pub r: u8,
    /// Green component.
    pub g: u8,
    /// Blue component.
    pub b: u8,
}

impl Rgb {
    /// A colour from its three components.
    #[must_use]
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// What a colour is *for*, which is the only thing a renderer names.
///
/// Closed and exhaustively matched: adding a role must stop
/// [`basic_slot`] and every palette literal from compiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ThemeRole {
    /// Primary brand accent.
    Accent,
    /// Successful outcome.
    Success,
    /// Failed outcome.
    Error,
    /// Cautionary state.
    Warn,
    /// Body text.
    Text,
    /// Secondary text.
    Dim,
    /// Borders and chrome.
    Border,
    /// Inline code and source references.
    Code,
    /// Background behind submitted prompts and focused transcript rows.
    PromptBackground,
    /// The `❯` marker inside a prompt band, quieter than secondary text.
    PromptGlyph,
    /// Heading for task and workflow panels, distinct from picker focus.
    PanelTitle,
}

/// Number of [`ThemeRole`] variants; the width of every palette.
pub const ROLE_COUNT: usize = 11;

impl ThemeRole {
    /// Every role, in palette order.
    pub const ALL: [Self; ROLE_COUNT] = [
        Self::Accent,
        Self::Success,
        Self::Error,
        Self::Warn,
        Self::Text,
        Self::Dim,
        Self::Border,
        Self::Code,
        Self::PromptBackground,
        Self::PromptGlyph,
        Self::PanelTitle,
    ];

    /// Palette index for this role.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Accent => 0,
            Self::Success => 1,
            Self::Error => 2,
            Self::Warn => 3,
            Self::Text => 4,
            Self::Dim => 5,
            Self::Border => 6,
            Self::Code => 7,
            Self::PromptBackground => 8,
            Self::PromptGlyph => 9,
            Self::PanelTitle => 10,
        }
    }

    /// Stable diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Accent => "accent",
            Self::Success => "success",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Text => "text",
            Self::Dim => "dim",
            Self::Border => "border",
            Self::Code => "code",
            Self::PromptBackground => "prompt-background",
            Self::PromptGlyph => "prompt-glyph",
            Self::PanelTitle => "panel-title",
        }
    }
}

/// The built-in semantic dark palette, in [`ThemeRole::index`] order.
///
/// This array is the single source of the product's colours; nothing else in
/// the workspace may repeat these values.
pub const HEYCODE_DARK: [Rgb; ROLE_COUNT] = [
    Rgb::new(0xB1, 0xB9, 0xF9),
    Rgb::new(0x4E, 0xBA, 0x65),
    Rgb::new(0xFF, 0x6B, 0x80),
    Rgb::new(0xFF, 0xC1, 0x07),
    Rgb::new(0xDD, 0xDD, 0xDD),
    Rgb::new(0x99, 0x99, 0x99),
    Rgb::new(0x88, 0x88, 0x88),
    Rgb::new(0xEB, 0x9F, 0x7F),
    Rgb::new(0x37, 0x37, 0x37),
    Rgb::new(0x50, 0x50, 0x50),
    Rgb::new(0x00, 0xCC, 0xCC),
];

/// Semantic foregrounds for a light terminal background.
pub const HEYCODE_LIGHT: [Rgb; ROLE_COUNT] = [
    Rgb::new(0x57, 0x69, 0xF7),
    Rgb::new(0x2C, 0x7A, 0x39),
    Rgb::new(0xAB, 0x2B, 0x3F),
    Rgb::new(0x96, 0x6C, 0x1E),
    Rgb::new(0x20, 0x24, 0x2C),
    Rgb::new(0x66, 0x66, 0x66),
    Rgb::new(0x99, 0x99, 0x99),
    Rgb::new(0x70, 0x36, 0x9C),
    Rgb::new(0xF0, 0xF0, 0xF0),
    Rgb::new(0xAF, 0xAF, 0xAF),
    Rgb::new(0x00, 0x99, 0x99),
];

/// A high-contrast palette for terminals or eyes the default does not serve.
///
/// Every role is a saturated primary or a pure grey, so it survives
/// quantization at 256 with visibly distinct results.
pub const HEYCODE_HIGH_CONTRAST: [Rgb; ROLE_COUNT] = [
    Rgb::new(0xFF, 0xAF, 0x00),
    Rgb::new(0x00, 0xD7, 0x00),
    Rgb::new(0xFF, 0x00, 0x00),
    Rgb::new(0xFF, 0xFF, 0x00),
    Rgb::new(0xFF, 0xFF, 0xFF),
    Rgb::new(0xBC, 0xBC, 0xBC),
    Rgb::new(0x6C, 0x6C, 0x6C),
    Rgb::new(0xD7, 0xAF, 0xFF),
    Rgb::new(0x30, 0x30, 0x30),
    Rgb::new(0x80, 0x80, 0x80),
    Rgb::new(0x00, 0xFF, 0xFF),
];

/// A colour in the form a terminal can actually be told to use.
///
/// There is no variant carrying a 24-bit value alongside an index: a resolved
/// colour is exactly one thing, decided by the tier, so a renderer cannot pick
/// the wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TerminalColor {
    /// 24-bit direct colour. Only ever produced at [`ColorLevel::TrueColor`].
    Rgb(Rgb),
    /// An indexed palette entry. 16–255 at [`ColorLevel::Ansi256`], 0–15 at
    /// [`ColorLevel::Basic`].
    Indexed(u8),
    /// The terminal's own default, which is also what "no colour" means.
    Default,
}

/// A named role palette, authored in 24-bit colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Theme {
    id: UiContributionId,
    title: String,
    palette: [Rgb; ROLE_COUNT],
}

impl Theme {
    /// Validate and construct a theme.
    ///
    /// Every role must be supplied, because the alternative is a missing
    /// colour discovered at draw time with nothing sensible to substitute.
    ///
    /// # Errors
    /// An invalid id or title fails before the theme can be registered.
    pub fn new(
        id: impl Into<String>,
        title: impl Into<String>,
        palette: [Rgb; ROLE_COUNT],
    ) -> Result<Self, UiRegistryError> {
        let title = title.into();
        if title.is_empty()
            || title.trim() != title
            || title.len() > 256
            || title.chars().any(char::is_control)
        {
            return Err(UiRegistryError::InvalidTitle);
        }
        Ok(Self {
            id: UiContributionId::new(id)?,
            title,
            palette,
        })
    }

    /// Stable lookup id.
    #[must_use]
    pub const fn id(&self) -> &UiContributionId {
        &self.id
    }

    /// Human label.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The authored 24-bit colour for one role.
    #[must_use]
    pub const fn rgb(&self, role: ThemeRole) -> Rgb {
        self.palette[role.index()]
    }

    /// Resolve every role for one colour tier.
    ///
    /// This is the only way to obtain a drawable colour, which is what stops a
    /// 24-bit value reaching a terminal that cannot render it.
    #[must_use]
    pub fn resolve(&self, level: ColorLevel) -> ResolvedTheme {
        let mut colors = [TerminalColor::Default; ROLE_COUNT];
        for role in ThemeRole::ALL {
            colors[role.index()] = match level {
                ColorLevel::TrueColor => TerminalColor::Rgb(self.rgb(role)),
                ColorLevel::Ansi256 => TerminalColor::Indexed(quantize_256(self.rgb(role))),
                ColorLevel::Basic => basic_slot(role),
                ColorLevel::None => TerminalColor::Default,
            };
        }
        ResolvedTheme {
            id: self.id.clone(),
            level,
            colors,
        }
    }
}

/// A theme after the tier has been applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTheme {
    id: UiContributionId,
    level: ColorLevel,
    colors: [TerminalColor; ROLE_COUNT],
}

impl ResolvedTheme {
    /// Which theme this came from.
    #[must_use]
    pub const fn id(&self) -> &UiContributionId {
        &self.id
    }

    /// The tier it was resolved for.
    #[must_use]
    pub const fn level(&self) -> ColorLevel {
        self.level
    }

    /// The drawable colour for one role.
    #[must_use]
    pub const fn color(&self, role: ThemeRole) -> TerminalColor {
        self.colors[role.index()]
    }
}

/// The role's slot in the 16-colour ANSI set.
///
/// A semantic mapping rather than a quantization, because ANSI 0–15 carry no
/// defined RGB values to measure distance against. `Text` resolves to the
/// terminal's own foreground so a user's colour scheme keeps working, and
/// `Dim`/`Border` share bright-black because the 16-colour set holds exactly
/// one grey.
#[must_use]
pub const fn basic_slot(role: ThemeRole) -> TerminalColor {
    match role {
        // Blue focus remains distinct from red errors in the basic palette.
        ThemeRole::Accent => TerminalColor::Indexed(12),
        ThemeRole::PanelTitle => TerminalColor::Indexed(6),
        ThemeRole::Code => TerminalColor::Indexed(13),
        ThemeRole::Success => TerminalColor::Indexed(2),
        ThemeRole::Error => TerminalColor::Indexed(1),
        ThemeRole::Warn => TerminalColor::Indexed(3),
        ThemeRole::Text => TerminalColor::Default,
        ThemeRole::PromptBackground => TerminalColor::Default,
        ThemeRole::Dim | ThemeRole::Border | ThemeRole::PromptGlyph => TerminalColor::Indexed(8),
    }
}

/// The six component levels of the xterm 6×6×6 colour cube.
///
/// Indices 16–231 are `16 + 36*r + 6*g + b` over these levels; index 16 is
/// `#000000` and index 231 is `#ffffff`.
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// First index of the 24-step grey ramp; index 232 is `#080808`.
const GREY_FIRST_INDEX: u8 = 232;

/// Value of the darkest grey-ramp entry.
const GREY_FIRST_VALUE: u8 = 8;

/// Step between grey-ramp entries; index 255 is `#eeeeee` (238).
const GREY_STEP: u8 = 10;

/// Number of grey-ramp entries.
const GREY_STEPS: u8 = 24;

/// Nearest xterm palette index in 16–255 for a 24-bit colour.
///
/// Indices 0–15 are excluded: they are terminal-defined, so choosing one by
/// RGB distance would be measuring against values this process invented.
#[must_use]
pub fn quantize_256(rgb: Rgb) -> u8 {
    let cube_index = |value: u8| -> usize {
        let mut best = 0;
        let mut best_distance = u32::MAX;
        for (index, level) in CUBE_LEVELS.iter().enumerate() {
            let distance = value.abs_diff(*level) as u32;
            if distance < best_distance {
                best_distance = distance;
                best = index;
            }
        }
        best
    };
    let (r, g, b) = (cube_index(rgb.r), cube_index(rgb.g), cube_index(rgb.b));
    let cube_rgb = Rgb::new(CUBE_LEVELS[r], CUBE_LEVELS[g], CUBE_LEVELS[b]);
    let cube_slot = 16 + 36 * r + 6 * g + b;

    let average = (u32::from(rgb.r) + u32::from(rgb.g) + u32::from(rgb.b)) / 3;
    let mut grey_step = 0_u8;
    let mut grey_distance = u32::MAX;
    for step in 0..GREY_STEPS {
        let value = u32::from(GREY_FIRST_VALUE) + u32::from(GREY_STEP) * u32::from(step);
        let distance = value.abs_diff(average);
        if distance < grey_distance {
            grey_distance = distance;
            grey_step = step;
        }
    }
    let grey_value = GREY_FIRST_VALUE.saturating_add(GREY_STEP.saturating_mul(grey_step));
    let grey_rgb = Rgb::new(grey_value, grey_value, grey_value);

    if squared_distance(rgb, grey_rgb) < squared_distance(rgb, cube_rgb) {
        GREY_FIRST_INDEX.saturating_add(grey_step)
    } else {
        // 16 + 36*5 + 6*5 + 5 == 231, so the cast cannot truncate.
        cube_slot as u8
    }
}

fn squared_distance(left: Rgb, right: Rgb) -> u32 {
    let component = |a: u8, b: u8| {
        let delta = u32::from(a.abs_diff(b));
        delta * delta
    };
    component(left.r, right.r) + component(left.g, right.g) + component(left.b, right.b)
}

/// The themes that ship with heycode, in listing order.
///
/// # Errors
/// A malformed built-in id or title, which is a bug in this module rather than
/// a runtime condition.
pub fn builtin_themes() -> Result<Vec<Theme>, UiRegistryError> {
    Ok(vec![
        Theme::new("heycode-dark", "heycode dark", HEYCODE_DARK)?,
        Theme::new("heycode-light", "heycode light", HEYCODE_LIGHT)?,
        Theme::new(
            "heycode-high-contrast",
            "heycode high contrast",
            HEYCODE_HIGH_CONTRAST,
        )?,
    ])
}

/// Id of the theme used when nothing has been selected.
pub const DEFAULT_THEME_ID: &str = "heycode-dark";

/// The default theme.
///
/// # Errors
/// A malformed built-in id or title.
pub fn default_theme() -> Result<Theme, UiRegistryError> {
    Theme::new(DEFAULT_THEME_ID, "heycode dark", HEYCODE_DARK)
}
