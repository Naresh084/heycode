//! heycode-tui — the Claude Code-inspired terminal UI.
//!
//! Renders the transcript from live [`UiEvent`]s (deltas, tool cards,
//! spinner) inside a three-region layout: transcript, rounded input box,
//! status line. Idle renders zero wakeups; streaming redraws are capped.
//! Markdown renders with syntect-highlighted fenced code.

pub mod add_directory;
pub mod advisor_panel;
mod agent_readiness;
pub mod app;
pub mod approval_preview;
mod autocompact_panel;
pub mod command_palette;
mod command_panel_frame;
pub mod command_scheduling;
mod composer_shortcuts;
mod copy_panel;
mod effort_render;
mod endpoint_connection;
mod export_panel;
mod file_delivery;
mod help;
pub mod human_commands;
mod insights;
mod markdown;
pub mod mascot;
pub mod mcp_panel;
pub mod memory_commands;
pub mod memory_panel;
pub mod model_picker;
pub mod panel_commands;
pub mod panel_frame;
pub mod permission_picker;
pub mod plan_review;
mod plugin;
pub mod plugin_panel;
pub mod product_attachments;
pub mod prompt_history;
pub mod recomposition;
pub mod release_notes;
pub mod render;
pub mod rewind_picker;
pub mod route_picker;
mod sandbox_panel;
mod screen_selection;
pub mod session_browser;
mod session_control;
pub mod settings_panel;
pub mod side_panel;
mod skill_doctor_panel;
mod skills_panel;
mod stats_render;
pub mod stats_view;
pub mod task_console;
mod task_observation;
mod task_render;
pub mod task_source;
pub mod terminal;
mod tool_family_cards;
pub mod transcript;
mod voice;
pub mod workflow_console;
mod workflow_render;
mod workflow_source;
pub mod workspace_context;

pub use app::accessibility::{FlatOutput, ScreenReaderSnapshot};
pub use app::{
    AppEvent, AppState, Item, SessionRecompose, TuiRunOutcome, WelcomeHealth, WelcomeStatusView,
};
pub use panel_commands::{CapabilityPanel, PanelCommandBridge, panel_commands_plugin};
pub use plugin::{TuiHandle, tui_plugin, tui_plugin_with_mcp_bridge};
pub use product_attachments::{
    McpProductAttachmentError, McpProductSession, McpTuiBridge, ProductHookAdapter,
    product_hook_attachments_plugin,
};
pub use session_browser::branch_receipt;
pub use terminal::TuiDisplayMode;

/// Interactive terminal UI handle service.
pub const SERVICE_TUI: heycode_core::ServiceKey = heycode_core::ServiceKey::new("tui");

/// Accent color tokens (AGENTS.md §8 palette).
///
/// Derived from [`heycode_ui::theme::HEYCODE_DARK`] resolved at the 24-bit tier, so
/// the workspace holds exactly one colour table. A renderer that must respect
/// the detected capability tier uses [`terminal::Styles`] instead; these
/// constants are the truecolor projection of the same theme.
pub mod palette {
    use heycode_ui::theme::{HEYCODE_DARK, ThemeRole};
    use ratatui::style::Color;

    const fn token(role: ThemeRole) -> Color {
        let rgb = HEYCODE_DARK[role.index()];
        Color::Rgb(rgb.r, rgb.g, rgb.b)
    }

    /// Active navigation accent.
    pub const ACCENT: Color = token(ThemeRole::Accent);
    /// Inline code and source references.
    pub const CODE: Color = token(ThemeRole::Code);
    /// Success green.
    pub const SUCCESS: Color = token(ThemeRole::Success);
    /// Error red.
    pub const ERROR: Color = token(ThemeRole::Error);
    /// Body text.
    pub const TEXT: Color = token(ThemeRole::Text);
    /// Secondary text.
    pub const DIM: Color = token(ThemeRole::Dim);
    /// Warn amber.
    pub const WARN: Color = token(ThemeRole::Warn);
    /// Borders and chrome.
    pub const BORDER: Color = token(ThemeRole::Border);
}
