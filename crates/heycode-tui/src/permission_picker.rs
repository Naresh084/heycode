//! Conversation permission choices. Sandbox details live under `/sandbox`.
use heycode_agent::ApprovalPolicyKind;
use heycode_exec::SandboxMode;

/// One plain-language conversation permission choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPickerRow {
    /// Shared approval mode identity.
    pub mode: ApprovalPolicyKind,
    /// Short product name.
    pub label: &'static str,
    /// Plain-language behavior.
    pub description: &'static str,
    /// Committed mode when the picker opened.
    pub current: bool,
    /// Whether the active conversation supports this choice.
    pub selectable: bool,
    /// Visible explanation for an unavailable choice.
    pub unavailable_reason: Option<&'static str>,
}

/// Build the four available conversation permission choices.
#[must_use]
pub fn build_permission_rows(current: &str, switchable: bool) -> Vec<PermissionPickerRow> {
    [
        ApprovalPolicyKind::FullAccess,
        ApprovalPolicyKind::AcceptedEdits,
        ApprovalPolicyKind::Ask,
        ApprovalPolicyKind::Plan,
    ]
    .into_iter()
    .map(|mode| {
        let selectable = switchable;
        PermissionPickerRow {
            mode,
            label: mode.label(),
            description: mode.description(),
            current: mode.as_str() == current,
            selectable,
            unavailable_reason: if !switchable {
                Some("Managed by this connection")
            } else {
                None
            },
        }
    })
    .collect()
}

/// Short label for an effective permission ID.
#[must_use]
pub fn permission_label(mode: &str) -> &str {
    match mode {
        "plan" => "Plan",
        "full_access" => "Full access",
        "accepted_edits" => "Accepted edits",
        "ask" | "default" => "Default",
        "auto" => "Auto",
        "deny" => "Blocked",
        "" => "Permissions unavailable",
        other => other,
    }
}

/// Idle-footer phrasing for an effective permission ID.
///
/// The pause glyph marks a mode that stops and asks; the double fast-forward
/// marks one that proceeds on its own. Both the glyphs and the lowercase
/// phrasing were read from the pinned Claude Code 2.1.269 footer captures
/// under `tmp/terminal-checks/…-footer-reference*`, one capture per mode.
/// `full_access` and `deny` keep heycode's own product names because the source
/// has no captured counterpart for them.
#[must_use]
pub fn permission_footer_label(mode: &str) -> String {
    match mode {
        "" => String::new(),
        "plan" => "⏸ plan mode on".to_owned(),
        "ask" | "default" => "⏸ manual mode on".to_owned(),
        "accepted_edits" => "⏵⏵ accept edits on".to_owned(),
        "auto" => "⏵⏵ auto mode on".to_owned(),
        "full_access" => "⏵⏵ full access on".to_owned(),
        "deny" => "⏸ tools blocked".to_owned(),
        other => format!("⏸ {} on", permission_label(other).to_lowercase()),
    }
}

/// Theme role that carries an approval mode's colour.
///
/// Each mode owns a distinct role so the controls row reads by colour at a
/// glance, not by word: permissive modes warn, plan is accent, accepted edits
/// reads as success, and the default stays quiet.
#[must_use]
pub fn permission_role(mode: &str) -> heycode_ui::theme::ThemeRole {
    use heycode_ui::theme::ThemeRole;
    match mode {
        "plan" => ThemeRole::Accent,
        "accepted_edits" => ThemeRole::Success,
        "full_access" | "auto" => ThemeRole::Warn,
        "deny" | "" => ThemeRole::Error,
        _ => ThemeRole::Dim,
    }
}

/// Legacy sandbox label for restart diagnostics.
#[must_use]
pub const fn mode_label(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::Off => "Full Access",
        SandboxMode::ReadOnly => "Read Only",
        SandboxMode::WorkspaceWrite => "Workspace Write",
    }
}

/// Root-config value that selects one exact effective sandbox mode on restart.
#[must_use]
pub const fn sandbox_config_value(mode: SandboxMode) -> &'static str {
    match mode {
        SandboxMode::Off => "off",
        SandboxMode::ReadOnly => "readonly",
        SandboxMode::WorkspaceWrite => "workspace",
    }
}

/// Actionable explanation used until one Settings-backed owner can commit and
/// hot-apply a sandbox generation.
#[must_use]
pub fn unavailable_selection_message(mode: SandboxMode) -> String {
    format!(
        "sandbox mode {} was not changed: runtime policy hot reload is unavailable; restart with `--set sandbox.mode={}`",
        mode_label(mode),
        sandbox_config_value(mode),
    )
}
