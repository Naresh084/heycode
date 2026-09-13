//! U20: what a theme resolves to at each tier, and the lifecycle of a theme a
//! plugin contributed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use heycode_core::{Context, Plugin, compose};
use heycode_ui::terminal::ColorLevel;
use heycode_ui::theme::{
    DEFAULT_THEME_ID, HEYCODE_DARK, HEYCODE_LIGHT, Rgb, TerminalColor, Theme, ThemeRole,
    basic_slot, builtin_themes, default_theme, quantize_256,
};
use heycode_ui::{SERVICE_UI, UiRegistry, UiRegistryError, ui_registry_plugin};

#[test]
fn truecolor_resolution_hands_back_exactly_the_authored_rgb() {
    let theme = default_theme().unwrap();
    let resolved = theme.resolve(ColorLevel::TrueColor);
    for role in ThemeRole::ALL {
        assert_eq!(
            resolved.color(role),
            TerminalColor::Rgb(HEYCODE_DARK[role.index()]),
            "role {}",
            role.as_str()
        );
    }
}

#[test]
fn a_terminal_without_24_bit_is_never_handed_a_24_bit_value() {
    // The single safety property of the whole module: below TrueColor, no
    // resolved role may carry an Rgb.
    let theme = default_theme().unwrap();
    for level in [ColorLevel::Ansi256, ColorLevel::Basic, ColorLevel::None] {
        let resolved = theme.resolve(level);
        for role in ThemeRole::ALL {
            assert!(
                !matches!(resolved.color(role), TerminalColor::Rgb(_)),
                "{} leaked a 24-bit value at {}",
                role.as_str(),
                level.as_str()
            );
        }
    }
}

#[test]
fn the_256_tier_uses_only_indices_the_palette_actually_defines() {
    // 0-15 are terminal-defined, so quantizing toward them would measure
    // against values this process invented.
    let theme = default_theme().unwrap();
    let resolved = theme.resolve(ColorLevel::Ansi256);
    for role in ThemeRole::ALL {
        match resolved.color(role) {
            TerminalColor::Indexed(index) => assert!(
                index >= 16,
                "{} quantized to terminal-defined index {index}",
                role.as_str()
            ),
            other => panic!("{} resolved to {other:?}", role.as_str()),
        }
    }
}

#[test]
fn the_quantizer_reproduces_the_published_cube_and_grey_ramp() {
    // xterm's 6x6x6 cube spans indices 16 (#000000) to 231 (#ffffff) over the
    // component levels 0/95/135/175/215/255; the 24-step grey ramp spans 232
    // (#080808) to 255 (#eeeeee).
    assert_eq!(quantize_256(Rgb::new(0x00, 0x00, 0x00)), 16);
    assert_eq!(quantize_256(Rgb::new(0xFF, 0xFF, 0xFF)), 231);
    assert_eq!(quantize_256(Rgb::new(0x08, 0x08, 0x08)), 232);
    assert_eq!(quantize_256(Rgb::new(0xEE, 0xEE, 0xEE)), 255);
    // 16 + 36*5 + 6*0 + 0 == 196.
    assert_eq!(quantize_256(Rgb::new(0xFF, 0x00, 0x00)), 196);
    // A grey the ramp reproduces exactly beats the nearest cube level.
    assert_eq!(quantize_256(Rgb::new(0x12, 0x12, 0x12)), 233);
}

#[test]
fn the_basic_tier_uses_semantic_slots_rather_than_quantized_colour() {
    let theme = default_theme().unwrap();
    let resolved = theme.resolve(ColorLevel::Basic);
    for role in ThemeRole::ALL {
        assert_eq!(resolved.color(role), basic_slot(role), "{}", role.as_str());
    }
    // Body text follows the terminal's own foreground so a user's 16-colour
    // scheme keeps working; the outcome roles keep their conventional slots.
    assert_eq!(resolved.color(ThemeRole::Text), TerminalColor::Default);
    assert_eq!(resolved.color(ThemeRole::Error), TerminalColor::Indexed(1));
    assert_eq!(
        resolved.color(ThemeRole::Success),
        TerminalColor::Indexed(2)
    );
    for role in ThemeRole::ALL {
        if let TerminalColor::Indexed(index) = resolved.color(role) {
            assert!(index < 16, "{} used index {index} at basic", role.as_str());
        }
    }
}

#[test]
fn the_no_colour_tier_defaults_every_role() {
    let resolved = default_theme().unwrap().resolve(ColorLevel::None);
    for role in ThemeRole::ALL {
        assert_eq!(
            resolved.color(role),
            TerminalColor::Default,
            "{}",
            role.as_str()
        );
    }
    assert_eq!(resolved.level(), ColorLevel::None);
    assert_eq!(resolved.id().as_str(), DEFAULT_THEME_ID);
}

#[test]
fn a_different_theme_resolves_to_different_colour_at_every_visible_tier() {
    // Guards the case where `resolve` reads a fixed palette instead of the
    // theme it was called on.
    let other = builtin_themes()
        .unwrap()
        .into_iter()
        .find(|theme| theme.id().as_str() != DEFAULT_THEME_ID)
        .expect("a second built-in theme");
    let default = default_theme().unwrap();
    for level in [ColorLevel::TrueColor, ColorLevel::Ansi256] {
        assert_ne!(
            default.resolve(level).color(ThemeRole::Accent),
            other.resolve(level).color(ThemeRole::Accent),
            "accent is identical across themes at {}",
            level.as_str()
        );
    }
}

fn relative_luminance(color: Rgb) -> f64 {
    let linear = |component: u8| {
        let component = f64::from(component) / 255.0;
        if component <= 0.04045 {
            component / 12.92
        } else {
            ((component + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

fn contrast(first: Rgb, second: Rgb) -> f64 {
    let (lighter, darker) = {
        let first = relative_luminance(first);
        let second = relative_luminance(second);
        if first >= second {
            (first, second)
        } else {
            (second, first)
        }
    };
    (lighter + 0.05) / (darker + 0.05)
}

#[test]
fn built_in_truecolor_roles_meet_the_captured_terminal_contrast_floors() {
    // Captured heycode/Claude dark terminal surface and the light probe surface.
    // Preserve the measured Claude 2.1.269 light palette. Its accent, warning
    // and border do not meet the stricter floors used by the dark palette.
    // These are regression bounds for source fidelity, not a WCAG claim.
    let surfaces = [
        ("dark", HEYCODE_DARK, Rgb::new(0x10, 0x10, 0x14)),
        ("light", HEYCODE_LIGHT, Rgb::new(0xF8, 0xF9, 0xFB)),
    ];
    for (name, palette, background) in surfaces {
        for role in [
            ThemeRole::Accent,
            ThemeRole::Success,
            ThemeRole::Error,
            ThemeRole::Warn,
            ThemeRole::Text,
            ThemeRole::Dim,
            ThemeRole::Code,
        ] {
            let ratio = contrast(palette[role.index()], background);
            let floor = match (name, role) {
                ("light", ThemeRole::Accent) => 4.1,
                ("light", ThemeRole::Warn) => 4.4,
                _ => 4.5,
            };
            assert!(
                ratio >= floor,
                "{name} {} contrast {ratio:.3} is below {floor}:1",
                role.as_str()
            );
        }
        let ratio = contrast(palette[ThemeRole::Border.index()], background);
        let floor = if name == "light" { 2.7 } else { 3.0 };
        assert!(
            ratio >= floor,
            "{name} border contrast {ratio:.3} is below {floor}:1"
        );
    }
}

#[test]
fn a_theme_needs_a_valid_id_and_title() {
    assert!(matches!(
        Theme::new("Not Kebab", "ok", HEYCODE_DARK),
        Err(UiRegistryError::InvalidId)
    ));
    assert!(matches!(
        Theme::new("fine", " untrimmed ", HEYCODE_DARK),
        Err(UiRegistryError::InvalidTitle)
    ));
}

struct ThemePlugin;

impl Plugin for ThemePlugin {
    fn name(&self) -> &'static str {
        "test-theme"
    }

    fn descriptor(&self) -> heycode_core::PluginDescriptor {
        heycode_core::PluginDescriptor::built_in(
            "test-theme",
            "1.0.0",
            &[heycode_core::PluginContributionKind::UserInterface],
        )
    }

    fn inject(&self) -> &'static [heycode_core::ServiceKey] {
        &[SERVICE_UI]
    }

    fn apply(&self, context: &mut Context) -> heycode_core::CoreResult<()> {
        let registry = context
            .get::<UiRegistry>(SERVICE_UI)
            .ok_or_else(|| heycode_core::CoreError::other("ui registry missing"))?;
        let theme = Theme::new("vendor.sunset", "Vendor sunset", HEYCODE_DARK)
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
        registry
            .register_theme(context, theme)
            .map_err(|error| heycode_core::CoreError::other(error.to_string()))
    }
}

#[test]
fn a_contributed_theme_is_visible_while_composed_and_gone_after_shutdown() {
    let plugins: Vec<Box<dyn Plugin>> = vec![ui_registry_plugin(), Box::new(ThemePlugin)];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<UiRegistry>(SERVICE_UI).unwrap();
    assert!(
        registry
            .theme("vendor.sunset")
            .unwrap()
            .is_some_and(|theme| theme.title() == "Vendor sunset")
    );
    context.shutdown();
    assert!(registry.theme("vendor.sunset").unwrap().is_none());
    assert!(
        registry
            .themes()
            .unwrap()
            .iter()
            .all(|theme| theme.id().as_str() != "vendor.sunset"),
        "a disposed theme must not survive in the listing"
    );
}

#[test]
fn a_contributed_theme_may_not_take_a_builtin_id() {
    let plugins: Vec<Box<dyn Plugin>> = vec![ui_registry_plugin()];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<UiRegistry>(SERVICE_UI).unwrap();
    let clash = Theme::new(DEFAULT_THEME_ID, "impostor", HEYCODE_DARK).unwrap();
    let error = registry.register_theme(&context, clash).unwrap_err();
    assert!(
        matches!(&error, UiRegistryError::Duplicate { identity } if identity == "theme:heycode-dark"),
        "expected a named duplicate, got {error}"
    );
    assert_eq!(
        registry.theme(DEFAULT_THEME_ID).unwrap().unwrap().title(),
        "heycode dark",
        "the built-in must be untouched by the refused registration"
    );
    context.shutdown();
}

#[test]
fn the_theme_listing_holds_the_builtins_ordered_by_id() {
    let plugins: Vec<Box<dyn Plugin>> = vec![ui_registry_plugin(), Box::new(ThemePlugin)];
    let mut context = compose(&plugins).unwrap();
    let registry = context.get::<UiRegistry>(SERVICE_UI).unwrap();
    let listed: Vec<String> = registry
        .themes()
        .unwrap()
        .iter()
        .map(|theme| theme.id().as_str().to_owned())
        .collect();
    assert_eq!(
        listed,
        vec![
            "heycode-dark".to_owned(),
            "heycode-high-contrast".to_owned(),
            "heycode-light".to_owned(),
            "vendor.sunset".to_owned(),
        ]
    );
    context.shutdown();
}
