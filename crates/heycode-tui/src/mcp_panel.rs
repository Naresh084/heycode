//! U12 MCP panel: one terminal surface over the MCP10 operation set.
//!
//! The panel owns no management logic. Every operation it offers is one of the
//! seven [`McpOperation`] variants and is executed by [`McpManagement`], which
//! implements each exactly once. `McpOperation` is deliberately not
//! `#[non_exhaustive]`, so the matches in [`build_actions`] and
//! [`McpPanelAction::label`] stop compiling the moment an eighth operation is
//! added — parity with the CLI is structural rather than remembered.
//!
//! Everything here is a pure function of its inputs: rows, sections and even
//! rendered lines are produced without a terminal, so the contracts below are
//! testable directly instead of through a frame buffer.

use std::cell::{Cell, RefCell};

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use heycode_mcp::management::{
    McpHealth, McpManagement, McpManagementError, McpOperation, McpStatusRow, StoredServer,
};
use heycode_mcp::{
    McpAuthenticationState, McpCapabilitySet, McpConnectionState, McpContributionCounts,
    McpDefinitionScope, McpFailureCode, McpServerSnapshot, McpSnapshot, McpTransportKind,
};

/// The six capabilities the acceptance criterion names, in tab order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpPanelSection {
    /// Persisted definition plus live connection lifecycle.
    Status,
    /// Whether the server needs authorization, and of what kind.
    Auth,
    /// Tools discovered by the last successful generation.
    Tools,
    /// Resources published by the server.
    Resources,
    /// Prompts published by the server.
    Prompts,
    /// The seven management operations.
    Actions,
}

impl McpPanelSection {
    /// Every section, in the order the acceptance criterion names them.
    pub const ALL: [Self; 6] = [
        Self::Status,
        Self::Auth,
        Self::Tools,
        Self::Resources,
        Self::Prompts,
        Self::Actions,
    ];

    /// Stable tab label.
    #[must_use]
    pub const fn title(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Auth => "auth",
            Self::Tools => "tools",
            Self::Resources => "resources",
            Self::Prompts => "prompts",
            Self::Actions => "actions",
        }
    }

    /// Next section in tab order, wrapping.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Status => Self::Auth,
            Self::Auth => Self::Tools,
            Self::Tools => Self::Resources,
            Self::Resources => Self::Prompts,
            Self::Prompts => Self::Actions,
            Self::Actions => Self::Status,
        }
    }

    /// Previous section in tab order, wrapping.
    #[must_use]
    pub const fn previous(self) -> Self {
        match self {
            Self::Status => Self::Actions,
            Self::Auth => Self::Status,
            Self::Tools => Self::Auth,
            Self::Resources => Self::Tools,
            Self::Prompts => Self::Resources,
            Self::Actions => Self::Prompts,
        }
    }
}

/// Which MCP listing capabilities this build actually implements.
///
/// Not a user feature flag: it is the panel's honest description of the code
/// that exists. A section whose field is `false` carries counts that are
/// structurally zero, because nothing ever lists that family — rendering a
/// count there would state "this server has none" when the truth is "heycode
/// never asked". Flipping a field is what makes the matching section go live.
///
/// All three families are live as of MCP09. The type stays because the
/// distinction it draws — "walked to zero" versus "never asked" — is the one
/// the panel exists to keep, and the next family to arrive (completions,
/// roots) will land here `false` before it lands `true`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpListingSupport {
    /// `tools/list` is implemented (MCP06).
    pub tools: bool,
    /// `resources/list` is implemented (MCP08).
    pub resources: bool,
    /// `prompts/list` is implemented (MCP09).
    pub prompts: bool,
}

impl McpListingSupport {
    /// What this build implements: all three listing families.
    ///
    /// MCP06 landed `tools`, MCP08 `resources`, MCP09 `prompts`. Every zero
    /// this panel renders is now a zero heycode actually walked to.
    pub const CURRENT: Self = Self {
        tools: true,
        resources: true,
        prompts: true,
    };
}

/// Live facts about one server, taken from the MCP registry snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpLiveFacts {
    /// Stable lifecycle word.
    pub state: &'static str,
    /// Safe one-line lifecycle detail.
    pub state_detail: String,
    /// Stable authorization word.
    pub authentication: &'static str,
    /// Capabilities the server advertised during `initialize`.
    pub capabilities: McpCapabilitySet,
    /// Counts discovered by the last successful generation, when there is one.
    pub contributions: Option<McpContributionCounts>,
    /// Negotiated server identity, when a generation was committed.
    pub identity: Option<String>,
}

/// One projected server row: persisted definition plus what is known live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPanelRow {
    /// Server name, unique within the store.
    pub name: String,
    /// Which transport this server speaks.
    pub transport: McpTransportKind,
    /// Command or URL, already stripped of anything that could carry a secret.
    pub target: String,
    /// Whether the definition participates in a session.
    pub enabled: bool,
    /// Where the definition came from.
    pub scope: &'static str,
    /// Administrator-locked definitions refuse every mutating operation.
    pub managed: bool,
    /// Probe result from the management layer.
    pub health: McpHealth,
    /// Live registry facts, absent when no MCP registry is composed or the
    /// registry has no row for this definition.
    pub live: Option<McpLiveFacts>,
}

/// Whether one section can state live facts, and if not, why not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpSectionState {
    /// The lines below are live facts.
    Live,
    /// The server did not advertise this capability during `initialize`.
    NotAdvertised,
    /// Nothing has been observed, so nothing is claimed.
    NoEvidence,
    /// This build does not implement the capability yet.
    Unsupported,
}

/// One section's availability plus its safe rendered lines.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpSectionView {
    /// Which capability this describes.
    pub section: McpPanelSection,
    /// Whether the lines are live facts or an explicit absence.
    pub state: McpSectionState,
    /// Safe display lines; never empty.
    pub lines: Vec<String>,
}

/// One offered management action, derived from the closed operation set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPanelAction {
    /// Which of the seven operations this action runs.
    pub operation: McpOperation,
    /// Human label; `enable` reads as its effect, not its name.
    pub label: &'static str,
    /// Present when the panel must not offer this action. A managed server and
    /// an empty selection are both refusals the operator sees before choosing,
    /// not after being denied.
    pub unavailable_reason: Option<String>,
}

impl McpPanelAction {
    /// Whether this action can be run now.
    #[must_use]
    pub const fn is_available(&self) -> bool {
        self.unavailable_reason.is_none()
    }
}

/// A fully specified panel intent, ready for [`dispatch`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpPanelIntent {
    /// Register a new definition.
    Add {
        /// Server name.
        name: String,
        /// Chosen transport.
        transport: McpTransportKind,
        /// Command or URL.
        target: String,
    },
    /// Re-read every definition without touching a server.
    List,
    /// Re-read every definition and probe each one live.
    ListProbed,
    /// Report one server's authorization state.
    Auth {
        /// Server name.
        name: String,
    },
    /// Probe one server.
    Test {
        /// Server name.
        name: String,
    },
    /// Change one definition's transport and target.
    Edit {
        /// Server name.
        name: String,
        /// Chosen transport.
        transport: McpTransportKind,
        /// Command or URL.
        target: String,
    },
    /// Turn one server on or off.
    Enable {
        /// Server name.
        name: String,
        /// Whether to turn the server on.
        on: bool,
    },
    /// Delete one definition.
    Remove {
        /// Server name.
        name: String,
    },
}

impl McpPanelIntent {
    /// Which operation this intent invokes.
    #[must_use]
    pub const fn operation(&self) -> McpOperation {
        match self {
            Self::Add { .. } => McpOperation::Add,
            Self::List | Self::ListProbed => McpOperation::List,
            Self::Auth { .. } => McpOperation::Auth,
            Self::Test { .. } => McpOperation::Test,
            Self::Edit { .. } => McpOperation::Edit,
            Self::Enable { .. } => McpOperation::Enable,
            Self::Remove { .. } => McpOperation::Remove,
        }
    }
}

/// What one dispatched operation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpPanelOutcome {
    /// `list` re-read the store.
    Listed(Vec<McpStatusRow>),
    /// `auth` or `test` reported live health without changing anything.
    Health {
        /// Server the probe named.
        name: String,
        /// What the probe concluded.
        health: McpHealth,
    },
    /// A mutating operation committed. The panel re-lists before showing rows,
    /// so no row is published from an operation that might still roll back.
    Committed(String),
}

/// What one key press asked the shell to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum McpPanelKeyOutcome {
    /// The view updated itself; nothing else to do.
    Handled,
    /// Run this intent against the management layer.
    Run(McpPanelIntent),
    /// Close the panel.
    Close,
}

/// Semantic emphasis for one rendered line.
///
/// The model stays terminal-free; the renderer maps these onto the AGENTS.md
/// §8 palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpPanelTone {
    /// Section or column heading.
    Heading,
    /// Ordinary content.
    Body,
    /// Secondary detail and key hints.
    Dim,
    /// A confirmed-good fact.
    Good,
    /// An absence, a lock, or a state needing operator action.
    Warn,
    /// A failure.
    Bad,
    /// The row or action under the cursor.
    Selected,
}

/// One rendered panel line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpPanelLine {
    /// Safe display text.
    pub text: String,
    /// Semantic emphasis.
    pub tone: McpPanelTone,
    hits: Vec<McpPanelLineHit>,
}

impl McpPanelLine {
    fn new(tone: McpPanelTone, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            tone,
            hits: Vec::new(),
        }
    }

    fn with_full_hit(mut self, target: McpPanelHitTarget) -> Self {
        self.hits.push(McpPanelLineHit {
            start: 0,
            end: usize::MAX,
            target,
        });
        self
    }

    fn with_hits(mut self, hits: Vec<McpPanelLineHit>) -> Self {
        self.hits = hits;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpPanelHitTarget {
    Server(usize),
    Section(McpPanelSection),
    Action(usize),
    FormField(McpFormField),
    Confirm(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct McpPanelLineHit {
    start: usize,
    end: usize,
    target: McpPanelHitTarget,
}

const NO_SELECTION: &str = "select a server first";

/// Server rows kept on the card at once; the rest are windowed, not clipped.
const VISIBLE_SERVER_ROWS: usize = 8;
const MAX_MCP_NAME_CHARS: usize = 64;
const MAX_MCP_TARGET_CHARS: usize = 512;

/// Stable health word. `needs-auth` and `unreachable` stay distinct because
/// they send the operator to different next steps.
#[must_use]
pub const fn health_word(health: McpHealth) -> &'static str {
    match health {
        McpHealth::Reachable => "reachable",
        McpHealth::Unreachable => "unreachable",
        McpHealth::AuthorizationRequired => "needs-auth",
        // `McpHealth` is `#[non_exhaustive]` on purpose: a new state is data
        // about the world, not a breaking change. A state this build does not
        // recognize reads as unknown, never as reachable — the safe direction
        // is the one that does not claim access we cannot see.
        _ => "unknown",
    }
}

/// The operator's next step for one health state.
#[must_use]
pub const fn health_next_step(health: McpHealth) -> &'static str {
    match health {
        McpHealth::Reachable => "the server answered initialize",
        McpHealth::Unreachable => "check the command or URL, then run test again",
        McpHealth::AuthorizationRequired => "run auth to authorize this server",
        _ => "not probed yet; run test, or refresh and probe, to check",
    }
}

/// Render one target with no secret material.
///
/// A stdio target is the exact command the operator typed and is shown whole:
/// credentials for a stdio server live in its environment, which this panel
/// never renders, and eliding argv would hide what actually runs. An HTTP
/// target is a URL, where userinfo, query and fragment are exactly where a
/// bearer token ends up, so those are removed — visibly, because hiding that
/// something was hidden is its own dishonesty.
#[must_use]
pub fn display_target(transport: McpTransportKind, target: &str) -> String {
    match transport {
        McpTransportKind::Stdio => target.to_owned(),
        McpTransportKind::StreamableHttp => elide_url(target),
    }
}

fn elide_url(target: &str) -> String {
    let (scheme, rest) = target
        .split_once("://")
        .map_or((None, target), |(scheme, rest)| (Some(scheme), rest));
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let (userinfo, host) = authority
        .rsplit_once('@')
        .map_or((None, authority), |(userinfo, host)| (Some(userinfo), host));
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = &tail[..path_end];
    let elided = userinfo.is_some() || path_end < tail.len();
    let mut rendered = String::new();
    if let Some(scheme) = scheme {
        rendered.push_str(scheme);
        rendered.push_str("://");
    }
    rendered.push_str(host);
    rendered.push_str(path);
    if elided {
        rendered.push_str(" · query and credentials elided");
    }
    rendered
}

const fn scope_word(scope: McpDefinitionScope) -> &'static str {
    match scope {
        McpDefinitionScope::User => "user",
        McpDefinitionScope::Project => "project",
        McpDefinitionScope::Local => "local",
        McpDefinitionScope::Managed => "managed",
        McpDefinitionScope::Plugin => "plugin",
    }
}

const fn transport_word(transport: McpTransportKind) -> &'static str {
    match transport {
        McpTransportKind::Stdio => "stdio",
        McpTransportKind::StreamableHttp => "streamable-http",
    }
}

const fn failure_word(code: McpFailureCode) -> &'static str {
    match code {
        McpFailureCode::Transport => "transport unavailable",
        McpFailureCode::Protocol => "protocol contract failed",
        McpFailureCode::Unauthorized => "authorization rejected",
        McpFailureCode::TimedOut => "operation budget expired",
        McpFailureCode::Conflict => "contribution identity conflict",
        McpFailureCode::InvalidDefinition => "definition rejected",
        McpFailureCode::ReconnectExhausted => "reconnect budget exhausted",
        McpFailureCode::Internal => "internal failure",
    }
}

const fn authentication_word(state: McpAuthenticationState) -> &'static str {
    match state {
        McpAuthenticationState::NotRequired => "not required",
        McpAuthenticationState::Unknown => "unknown",
        McpAuthenticationState::Required => "required",
        McpAuthenticationState::Connected => "connected",
        McpAuthenticationState::Expired => "expired",
        McpAuthenticationState::ReauthenticationRequired => "reauthentication required",
    }
}

fn live_facts(snapshot: &McpServerSnapshot) -> McpLiveFacts {
    let (state, state_detail) = match snapshot.state() {
        McpConnectionState::Disabled => ("disabled", "this definition is turned off".to_owned()),
        McpConnectionState::Inactive => (
            "inactive",
            "enabled, but no connection provider has claimed it".to_owned(),
        ),
        McpConnectionState::Starting { .. } => {
            ("starting", "connecting and negotiating".to_owned())
        }
        McpConnectionState::Ready { .. } => (
            "ready",
            "the latest successful generation is active".to_owned(),
        ),
        McpConnectionState::AuthenticationRequired { .. } => (
            "needs-auth",
            "authorize this server before it can connect".to_owned(),
        ),
        McpConnectionState::Reconnecting {
            attempt,
            max_attempts,
            ..
        } => (
            "reconnecting",
            format!("attempt {attempt} of {max_attempts}"),
        ),
        McpConnectionState::Degraded { code, .. } => (
            "degraded",
            format!(
                "{}; the last good generation is retained",
                failure_word(*code)
            ),
        ),
        McpConnectionState::Failed { code, .. } => (
            "failed",
            format!("{}; no generation is retained", failure_word(*code)),
        ),
    };
    let generation = snapshot.last_good_generation();
    McpLiveFacts {
        state,
        state_detail,
        authentication: authentication_word(snapshot.authentication()),
        capabilities: generation.map_or_else(McpCapabilitySet::default, |generation| {
            generation.capabilities()
        }),
        contributions: generation.map(heycode_mcp::McpConnectionGeneration::contributions),
        identity: generation.map(|generation| {
            format!(
                "{} {} · MCP {} · generation {}",
                generation.server_name(),
                generation.server_version(),
                generation.protocol_version(),
                generation.number()
            )
        }),
    }
}

/// Project stored definitions and the live registry into panel rows.
///
/// `live` is absent in a world that composes management without the MCP
/// registry; a row then carries no live facts rather than pretending the server
/// is idle.
#[must_use]
pub fn build_rows(status: &[McpStatusRow], live: Option<&McpSnapshot>) -> Vec<McpPanelRow> {
    status
        .iter()
        .map(|row| McpPanelRow {
            name: row.server.name.clone(),
            transport: row.server.transport,
            target: display_target(row.server.transport, &row.server.target),
            enabled: row.server.enabled,
            scope: scope_word(row.server.scope),
            managed: row.server.managed,
            health: row.health,
            live: live.and_then(|snapshot| {
                snapshot
                    .servers()
                    .iter()
                    .find(|server| server.definition().id().as_str() == row.server.name)
                    .map(live_facts)
            }),
        })
        .collect()
}

/// Build the action list for one selection.
///
/// The match on [`McpOperation`] carries no wildcard arm: an eighth operation
/// fails to compile here until this panel handles it.
#[must_use]
pub fn build_actions(row: Option<&McpPanelRow>) -> Vec<McpPanelAction> {
    McpOperation::ALL
        .into_iter()
        .map(|operation| {
            let (label, unavailable_reason) = match operation {
                // `add` creates a new definition, so a managed *selection* does
                // not refuse it: the administrator lock is per-definition.
                McpOperation::Add => ("Add server…", None),
                // Opening the panel is a read; this action is the user
                // asking heycode to go and connect.
                McpOperation::List => ("Refresh and probe", None),
                McpOperation::Auth => (
                    "Authorize",
                    row.map_or(Some(NO_SELECTION.to_owned()), |_| None),
                ),
                McpOperation::Test => (
                    "Test connection",
                    row.map_or(Some(NO_SELECTION.to_owned()), |_| None),
                ),
                McpOperation::Edit => ("Edit target…", mutation_refusal(row)),
                McpOperation::Enable => (
                    if row.is_some_and(|row| row.enabled) {
                        "Disable"
                    } else {
                        "Enable"
                    },
                    mutation_refusal(row),
                ),
                McpOperation::Remove => ("Remove", mutation_refusal(row)),
            };
            McpPanelAction {
                operation,
                label,
                unavailable_reason,
            }
        })
        .collect()
}

/// Why a mutating action is not offered for this selection.
///
/// A managed definition is refused by the store itself; discovering that lock
/// by being denied is a poor experience, so the panel states it up front.
fn mutation_refusal(row: Option<&McpPanelRow>) -> Option<String> {
    match row {
        None => Some(NO_SELECTION.to_owned()),
        Some(row) if row.managed => Some(format!(
            "`{}` is managed by an administrator and cannot be changed here",
            row.name
        )),
        Some(_) => None,
    }
}

fn listing_section(
    section: McpPanelSection,
    row: Option<&McpPanelRow>,
    supported: bool,
    pending_row: &str,
    read: fn(McpCapabilitySet) -> bool,
    count: fn(McpContributionCounts) -> u32,
) -> McpSectionView {
    let noun = section.title();
    let (state, lines) = if let Some(row) = row {
        let advertised = row.live.as_ref().map(|live| read(live.capabilities));
        match (supported, row.live.as_ref(), advertised) {
            (false, _, Some(true)) => (
                McpSectionState::Unsupported,
                vec![
                    format!("this server advertises {noun}"),
                    format!("heycode does not list {noun} yet ({pending_row})"),
                ],
            ),
            (false, _, _) => (
                McpSectionState::Unsupported,
                vec![
                    format!("heycode does not list {noun} yet ({pending_row})"),
                    format!("nothing here means unasked, not that the server has no {noun}"),
                ],
            ),
            (true, None, _) => (
                McpSectionState::NoEvidence,
                vec![
                    format!("no live connection, so no {noun} have been observed"),
                    "connect this server to list them".to_owned(),
                ],
            ),
            (true, Some(_), Some(false) | None) => (
                McpSectionState::NotAdvertised,
                vec![format!(
                    "this server did not advertise {noun} during initialize"
                )],
            ),
            (true, Some(live), Some(true)) => (
                McpSectionState::Live,
                vec![match live.contributions {
                    Some(contributions) => {
                        format!("{} {noun} in the current generation", count(contributions))
                    }
                    None => format!("advertised; no committed generation lists {noun} yet"),
                }],
            ),
        }
    } else {
        (
            McpSectionState::NoEvidence,
            vec![format!("select a server to inspect its {noun}")],
        )
    };
    McpSectionView {
        section,
        state,
        lines,
    }
}

/// Render one section for one selection.
///
/// Sections this build cannot answer report themselves unsupported and name the
/// row that would make them live. An empty list would say the server has none,
/// which is a different and false statement.
#[must_use]
pub fn section_view(
    section: McpPanelSection,
    row: Option<&McpPanelRow>,
    support: McpListingSupport,
) -> McpSectionView {
    match section {
        McpPanelSection::Status => status_section(row),
        McpPanelSection::Auth => auth_section(row),
        McpPanelSection::Tools => listing_section(
            section,
            row,
            support.tools,
            "MCP06",
            |capabilities| capabilities.tools,
            |contributions| contributions.tools,
        ),
        McpPanelSection::Resources => listing_section(
            section,
            row,
            support.resources,
            "MCP08",
            |capabilities| capabilities.resources,
            |contributions| contributions.resources,
        ),
        McpPanelSection::Prompts => listing_section(
            section,
            row,
            support.prompts,
            "MCP09",
            |capabilities| capabilities.prompts,
            |contributions| contributions.prompts,
        ),
        McpPanelSection::Actions => actions_section(row),
    }
}

fn status_section(row: Option<&McpPanelRow>) -> McpSectionView {
    let Some(row) = row else {
        return McpSectionView {
            section: McpPanelSection::Status,
            state: McpSectionState::NoEvidence,
            lines: vec!["no MCP servers are configured".to_owned()],
        };
    };
    let mut lines = vec![
        format!(
            "{} · {} scope · {}",
            if row.enabled { "enabled" } else { "disabled" },
            row.scope,
            transport_word(row.transport)
        ),
        format!("target: {}", row.target),
    ];
    if row.managed {
        lines.push("managed by an administrator; mutating operations refuse".to_owned());
    }
    match row.live.as_ref() {
        Some(live) => {
            lines.push(format!(
                "connection: {} — {}",
                live.state, live.state_detail
            ));
            if let Some(identity) = live.identity.as_ref() {
                lines.push(format!("server: {identity}"));
            }
        }
        None => lines.push("connection: no live registry evidence in this world".to_owned()),
    }
    McpSectionView {
        section: McpPanelSection::Status,
        state: if row.live.is_some() {
            McpSectionState::Live
        } else {
            McpSectionState::NoEvidence
        },
        lines,
    }
}

fn auth_section(row: Option<&McpPanelRow>) -> McpSectionView {
    let Some(row) = row else {
        return McpSectionView {
            section: McpPanelSection::Auth,
            state: McpSectionState::NoEvidence,
            lines: vec!["select a server to inspect its authorization".to_owned()],
        };
    };
    let mut lines = vec![
        format!("probe: {}", health_word(row.health)),
        health_next_step(row.health).to_owned(),
    ];
    if let Some(live) = row.live.as_ref() {
        lines.push(format!("credential state: {}", live.authentication));
    }
    McpSectionView {
        section: McpPanelSection::Auth,
        state: match row.health {
            McpHealth::Reachable | McpHealth::Unreachable | McpHealth::AuthorizationRequired => {
                McpSectionState::Live
            }
            _ => McpSectionState::NoEvidence,
        },
        lines,
    }
}

fn actions_section(row: Option<&McpPanelRow>) -> McpSectionView {
    McpSectionView {
        section: McpPanelSection::Actions,
        state: McpSectionState::Live,
        lines: build_actions(row)
            .into_iter()
            .map(|action| match action.unavailable_reason {
                Some(reason) => format!("{} — unavailable: {reason}", action.label),
                None => action.label.to_owned(),
            })
            .collect(),
    }
}

/// Run one intent against the shared MCP10 operations layer.
///
/// Nothing is reimplemented here: every arm delegates to [`McpManagement`],
/// which owns the single implementation of each operation.
///
/// This call is synchronous. With a probe installed, `auth`, `test` and `list`
/// reach the network, so a caller on a render thread must run it elsewhere.
///
/// # Errors
/// The management error rendered for a terminal. The text names servers and
/// fixed reasons only — never a typed target, which is the one field an
/// operator could have pasted a secret into.
pub fn dispatch(
    management: &McpManagement,
    intent: &McpPanelIntent,
) -> Result<McpPanelOutcome, String> {
    match intent {
        McpPanelIntent::Add {
            name,
            transport,
            target,
        } => {
            let server = StoredServer::new(name, *transport, target).map_err(render_error)?;
            management.add(server).map_err(render_error)?;
            Ok(McpPanelOutcome::Committed(format!("added `{name}`")))
        }
        McpPanelIntent::List => management
            .list()
            .map(McpPanelOutcome::Listed)
            .map_err(render_error),
        McpPanelIntent::ListProbed => management
            .list_probed()
            .map(McpPanelOutcome::Listed)
            .map_err(render_error),
        McpPanelIntent::Auth { name } => management
            .auth(name)
            .map(|health| McpPanelOutcome::Health {
                name: name.clone(),
                health,
            })
            .map_err(render_error),
        McpPanelIntent::Test { name } => management
            .test(name)
            .map(|health| McpPanelOutcome::Health {
                name: name.clone(),
                health,
            })
            .map_err(render_error),
        McpPanelIntent::Edit {
            name,
            transport,
            target,
        } => {
            management
                .edit(name, *transport, target)
                .map_err(render_error)?;
            Ok(McpPanelOutcome::Committed(format!("updated `{name}`")))
        }
        McpPanelIntent::Enable { name, on } => {
            management.enable(name, *on).map_err(render_error)?;
            Ok(McpPanelOutcome::Committed(format!(
                "{} `{name}`",
                if *on { "enabled" } else { "disabled" }
            )))
        }
        McpPanelIntent::Remove { name } => {
            management.remove(name).map_err(render_error)?;
            Ok(McpPanelOutcome::Committed(format!("removed `{name}`")))
        }
    }
}

fn render_error(error: McpManagementError) -> String {
    error.to_string()
}

/// Which list the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpPanelFocus {
    Servers,
    Actions,
}

/// Which field of an add/edit form the cursor is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum McpFormField {
    Name,
    Transport,
    Target,
}

struct McpPanelForm {
    operation: McpOperation,
    name: String,
    transport: McpTransportKind,
    target: String,
    field: McpFormField,
}

struct McpPanelConfirm {
    name: String,
    remove: bool,
}

struct McpPanelNotice {
    text: String,
    ok: bool,
}

/// Panel state: selection, section, in-flight form and last notice.
pub struct McpPanelView {
    rows: Vec<McpPanelRow>,
    selected: usize,
    section: McpPanelSection,
    focus: McpPanelFocus,
    action_selected: usize,
    section_offset: usize,
    support: McpListingSupport,
    form: Option<McpPanelForm>,
    confirm: Option<McpPanelConfirm>,
    notice: Option<McpPanelNotice>,
    mouse_area: Cell<Option<Rect>>,
    mouse_rows: RefCell<Vec<(Rect, McpPanelHitTarget)>>,
}

impl McpPanelView {
    /// Open the panel over one projection.
    #[must_use]
    pub fn new(rows: Vec<McpPanelRow>, support: McpListingSupport) -> Self {
        Self {
            rows,
            selected: 0,
            section: McpPanelSection::Status,
            focus: McpPanelFocus::Servers,
            action_selected: 0,
            section_offset: 0,
            support,
            form: None,
            confirm: None,
            notice: None,
            mouse_area: Cell::new(None),
            mouse_rows: RefCell::new(Vec::new()),
        }
    }

    /// Replace the rows, keeping the cursor on the same server when it survives.
    pub fn set_rows(&mut self, rows: Vec<McpPanelRow>) {
        let previous = self.selected_row().map(|row| row.name.clone());
        self.rows = rows;
        self.selected = previous
            .and_then(|name| self.rows.iter().position(|row| row.name == name))
            .unwrap_or(0);
        self.section_offset = 0;
        self.clamp_action();
    }

    /// Record a successful operation.
    pub fn note_success(&mut self, text: impl Into<String>) {
        self.notice = Some(McpPanelNotice {
            text: text.into(),
            ok: true,
        });
    }

    /// Record a failed operation.
    pub fn note_failure(&mut self, text: impl Into<String>) {
        self.notice = Some(McpPanelNotice {
            text: text.into(),
            ok: false,
        });
    }

    /// Projected rows.
    #[must_use]
    pub fn rows(&self) -> &[McpPanelRow] {
        &self.rows
    }

    /// Highlighted server row index.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    /// Highlighted server row.
    #[must_use]
    pub fn selected_row(&self) -> Option<&McpPanelRow> {
        self.rows.get(self.selected)
    }

    /// Active capability section.
    #[must_use]
    pub const fn section(&self) -> McpPanelSection {
        self.section
    }

    /// Which listing capabilities this build implements.
    #[must_use]
    pub const fn support(&self) -> McpListingSupport {
        self.support
    }

    /// Actions offered for the current selection.
    #[must_use]
    pub fn actions(&self) -> Vec<McpPanelAction> {
        build_actions(self.selected_row())
    }

    /// Highlighted action index.
    #[must_use]
    pub const fn action_selected(&self) -> usize {
        self.action_selected
    }

    /// Whether an add/edit form or a remove confirmation is open.
    #[must_use]
    pub const fn is_prompting(&self) -> bool {
        self.form.is_some() || self.confirm.is_some()
    }

    fn clamp_action(&mut self) {
        let count = McpOperation::ALL.len();
        if self.action_selected >= count {
            self.action_selected = count.saturating_sub(1);
        }
    }

    /// Rendered lines. Pure: no terminal, no styling beyond a tone token.
    #[must_use]
    pub fn lines(&self) -> Vec<McpPanelLine> {
        let mut lines = vec![McpPanelLine::new(
            McpPanelTone::Heading,
            format!("servers ({})", self.rows.len()),
        )];
        if self.rows.is_empty() {
            lines.push(McpPanelLine::new(
                McpPanelTone::Warn,
                "  none configured — use Add server…",
            ));
        }
        // The card is a fixed-height modal, so a long server list is windowed
        // here rather than clipped by the renderer: silently losing the key
        // hints and the last notice off the bottom is not an acceptable way to
        // run out of room.
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(VISIBLE_SERVER_ROWS);
        let end = start
            .saturating_add(VISIBLE_SERVER_ROWS)
            .min(self.rows.len());
        for (offset, row) in self.rows[start..end].iter().enumerate() {
            let index = start + offset;
            let selected = index == self.selected;
            lines.push(
                McpPanelLine::new(
                    if selected {
                        McpPanelTone::Selected
                    } else {
                        McpPanelTone::Body
                    },
                    format!(
                        "{}{:<20} {:<15} {:<9} {}{}",
                        if selected { "● " } else { "  " },
                        row.name,
                        transport_word(row.transport),
                        if row.enabled { "enabled" } else { "disabled" },
                        health_word(row.health),
                        if row.managed { "  [managed]" } else { "" }
                    ),
                )
                .with_full_hit(McpPanelHitTarget::Server(index)),
            );
        }
        if self.rows.len() > VISIBLE_SERVER_ROWS {
            lines.push(McpPanelLine::new(
                McpPanelTone::Dim,
                format!(
                    "  showing {}-{} of {}",
                    start.saturating_add(1),
                    end,
                    self.rows.len()
                ),
            ));
        }
        lines.push(McpPanelLine::new(McpPanelTone::Dim, ""));
        lines.push(self.section_tabs_line());
        lines.extend(self.section_lines());
        if let Some(form) = self.form.as_ref() {
            lines.push(McpPanelLine::new(McpPanelTone::Dim, ""));
            lines.extend(form_lines(form));
        }
        if let Some(confirm) = self.confirm.as_ref() {
            lines.push(McpPanelLine::new(McpPanelTone::Dim, ""));
            lines.push(McpPanelLine::new(
                McpPanelTone::Warn,
                format!("Remove `{}`? The definition is deleted.", confirm.name),
            ));
            lines.push(confirm_choice_line(confirm));
        }
        if let Some(notice) = self.notice.as_ref() {
            lines.push(McpPanelLine::new(McpPanelTone::Dim, ""));
            lines.push(McpPanelLine::new(
                if notice.ok {
                    McpPanelTone::Good
                } else {
                    McpPanelTone::Bad
                },
                notice.text.clone(),
            ));
        }
        lines.push(McpPanelLine::new(McpPanelTone::Dim, self.hint()));
        lines
    }

    /// Render the visual panel within an exact content-row budget.
    ///
    /// [`Self::lines`] remains the complete accessibility projection. This
    /// visual projection windows servers and section details while always
    /// retaining the active prompt or notice and the key hint. At very short
    /// heights a form follows its selected field, so Tab makes every required
    /// field reachable instead of letting the renderer clip it invisibly.
    #[must_use]
    pub fn lines_for_height(&self, height: u16) -> Vec<McpPanelLine> {
        let capacity = usize::from(height);
        if capacity == 0 {
            return Vec::new();
        }
        let hint = McpPanelLine::new(McpPanelTone::Dim, self.hint());
        if capacity == 1 {
            return vec![hint];
        }

        if self.is_prompting() {
            let tail = self.visual_prompt_lines();
            if tail.len().saturating_add(1) >= capacity {
                let mut compact = self.compact_prompt_lines(capacity - 1);
                compact.push(hint);
                return compact;
            }
            let prefix_budget = capacity.saturating_sub(tail.len().saturating_add(1));
            let mut lines = Vec::new();
            if prefix_budget >= 1 {
                lines.push(McpPanelLine::new(
                    McpPanelTone::Heading,
                    format!("servers ({})", self.rows.len()),
                ));
            }
            if prefix_budget >= 2 {
                lines.extend(self.visual_server_lines(1));
            }
            if prefix_budget >= 3 {
                lines.push(self.section_tabs_line());
            }
            lines.extend(tail);
            lines.truncate(capacity - 1);
            lines.push(hint);
            return lines;
        }

        let notice = self.notice_line().into_iter().collect::<Vec<_>>();
        // Two rows can still identify the selected server and explain how to
        // move; three rows also retain the active section.
        if capacity <= 3_usize.saturating_add(notice.len()) {
            let mut lines = self.visual_server_lines(1);
            if capacity >= 3_usize.saturating_add(notice.len()) {
                lines.push(self.section_tabs_line());
            }
            lines.extend(notice);
            lines.truncate(capacity - 1);
            lines.push(hint);
            return lines;
        }

        let fixed = 3_usize.saturating_add(notice.len());
        let remaining = capacity.saturating_sub(fixed);
        let section_has_lines = !self.section_lines().is_empty();
        let reserve_for_section = usize::from(section_has_lines && remaining >= 2);
        let server_budget = remaining
            .saturating_sub(reserve_for_section)
            .min(VISIBLE_SERVER_ROWS.saturating_add(1));
        let server_lines = self.visual_server_lines(server_budget);
        let detail_budget = remaining.saturating_sub(server_lines.len());

        let mut lines = vec![McpPanelLine::new(
            McpPanelTone::Heading,
            format!("servers ({})", self.rows.len()),
        )];
        lines.extend(server_lines);
        lines.push(self.section_tabs_line());
        lines.extend(self.visual_section_lines(detail_budget));
        lines.extend(notice);
        if lines.len() >= capacity {
            lines.truncate(capacity - 1);
        }
        lines.push(hint);
        lines
    }

    /// Publish the current visual line hit areas for pointer routing.
    ///
    /// `body` is the paragraph area inside the panel's top border. The model
    /// owns no global coordinates; the renderer supplies this one frame's
    /// exact content rectangle, and stale points are replaced atomically.
    pub fn set_mouse_layout(&self, body: Rect) {
        self.mouse_area.set(Some(body));
        let mut rows = Vec::new();
        for (offset, line) in self.lines_for_height(body.height).iter().enumerate() {
            let Ok(offset) = u16::try_from(offset) else {
                break;
            };
            let y = body.y.saturating_add(offset);
            if y >= body.bottom() {
                break;
            }
            for hit in &line.hits {
                let start = u16::try_from(hit.start).unwrap_or(u16::MAX);
                let end = u16::try_from(hit.end).unwrap_or(u16::MAX);
                let x = body.x.saturating_add(start.min(body.width));
                let right = body.x.saturating_add(end.min(body.width));
                let width = right.saturating_sub(x);
                if width > 0 {
                    rows.push((Rect::new(x, y, width, 1), hit.target));
                }
            }
        }
        *self.mouse_rows.borrow_mut() = rows;
    }

    /// Consume one pointer event without executing an MCP operation.
    ///
    /// Clicks only move a cursor. Wheel movement is bounded to the focused
    /// server/action list and only applies inside the current panel body.
    pub fn handle_mouse(&mut self, event: MouseEvent) -> McpPanelKeyOutcome {
        let point = Position::new(event.column, event.row);
        if !self
            .mouse_area
            .get()
            .is_some_and(|area| area.contains(point))
        {
            return McpPanelKeyOutcome::Handled;
        }
        match event.kind {
            MouseEventKind::ScrollUp if !self.is_prompting() => {
                self.move_focused_by(-1);
            }
            MouseEventKind::ScrollDown if !self.is_prompting() => {
                self.move_focused_by(1);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                let target = self
                    .mouse_rows
                    .borrow()
                    .iter()
                    .find(|(area, _)| area.contains(point))
                    .map(|(_, target)| *target);
                if let Some(target) = target {
                    self.select_mouse_target(target);
                }
            }
            _ => {}
        }
        McpPanelKeyOutcome::Handled
    }

    /// Consume bracketed paste inside the MCP modal.
    ///
    /// Paste writes only to the selected add/edit text field, strips control
    /// characters and obeys the same fixed field bounds as typing. It is a
    /// no-op on transport, confirmation, and ordinary panel states, so pasted
    /// newlines can never submit or confirm an operation.
    pub fn paste(&mut self, text: &str) -> McpPanelKeyOutcome {
        let Some(form) = self.form.as_mut() else {
            return McpPanelKeyOutcome::Handled;
        };
        match form.field {
            McpFormField::Name => append_name(&mut form.name, text.chars()),
            McpFormField::Target => append_target(&mut form.target, text.chars()),
            McpFormField::Transport => {}
        }
        McpPanelKeyOutcome::Handled
    }

    fn section_tabs_line(&self) -> McpPanelLine {
        let mut text = String::new();
        let mut hits = Vec::new();
        for (index, section) in McpPanelSection::ALL.iter().copied().enumerate() {
            if index > 0 {
                text.push(' ');
            }
            let start = text.chars().count();
            if section == self.section {
                text.push('[');
                text.push_str(section.title());
                text.push(']');
            } else {
                text.push(' ');
                text.push_str(section.title());
                text.push(' ');
            }
            hits.push(McpPanelLineHit {
                start,
                end: text.chars().count(),
                target: McpPanelHitTarget::Section(section),
            });
        }
        McpPanelLine::new(McpPanelTone::Heading, text).with_hits(hits)
    }

    fn visual_server_lines(&self, budget: usize) -> Vec<McpPanelLine> {
        if budget == 0 {
            return Vec::new();
        }
        if self.rows.is_empty() {
            return vec![McpPanelLine::new(
                McpPanelTone::Warn,
                "  none configured — use Add server…",
            )];
        }
        let show_range = self.rows.len() > 1 && budget >= 2;
        let shown = budget
            .saturating_sub(usize::from(show_range))
            .clamp(1, VISIBLE_SERVER_ROWS)
            .min(self.rows.len());
        let start = self
            .selected
            .saturating_add(1)
            .saturating_sub(shown)
            .min(self.rows.len().saturating_sub(shown));
        let end = start.saturating_add(shown).min(self.rows.len());
        let mut lines = self.rows[start..end]
            .iter()
            .enumerate()
            .map(|(offset, row)| {
                let index = start + offset;
                let selected = index == self.selected;
                McpPanelLine::new(
                    if selected {
                        McpPanelTone::Selected
                    } else {
                        McpPanelTone::Body
                    },
                    format!(
                        "{}{:<20} {:<15} {:<9} {}{}",
                        if selected { "● " } else { "  " },
                        row.name,
                        transport_word(row.transport),
                        if row.enabled { "enabled" } else { "disabled" },
                        health_word(row.health),
                        if row.managed { "  [managed]" } else { "" }
                    ),
                )
                .with_full_hit(McpPanelHitTarget::Server(index))
            })
            .collect::<Vec<_>>();
        if show_range {
            lines.push(McpPanelLine::new(
                McpPanelTone::Dim,
                format!("  servers {}-{} of {}", start + 1, end, self.rows.len()),
            ));
        }
        lines
    }

    fn visual_section_lines(&self, budget: usize) -> Vec<McpPanelLine> {
        if budget == 0 {
            return Vec::new();
        }
        let all = self.section_lines();
        if all.len() <= budget {
            return all;
        }
        let selected = if self.section == McpPanelSection::Actions {
            self.action_selected
        } else {
            self.section_offset
        };
        if budget == 1 {
            return all
                .get(selected.min(all.len().saturating_sub(1)))
                .cloned()
                .into_iter()
                .collect();
        }
        let visible = budget - 1;
        let start = selected
            .saturating_add(1)
            .saturating_sub(visible)
            .min(all.len().saturating_sub(visible));
        let end = start.saturating_add(visible).min(all.len());
        let mut lines = all[start..end].to_vec();
        lines.push(McpPanelLine::new(
            McpPanelTone::Dim,
            format!("details {}-{} of {} · PgUp/PgDn", start + 1, end, all.len()),
        ));
        lines
    }

    fn visual_prompt_lines(&self) -> Vec<McpPanelLine> {
        let mut lines = if let Some(form) = self.form.as_ref() {
            form_lines(form)
        } else if let Some(confirm) = self.confirm.as_ref() {
            vec![
                McpPanelLine::new(
                    McpPanelTone::Warn,
                    format!("Remove `{}`? The definition is deleted.", confirm.name),
                ),
                confirm_choice_line(confirm),
            ]
        } else {
            Vec::new()
        };
        lines.extend(self.notice_line());
        lines
    }

    fn compact_prompt_lines(&self, budget: usize) -> Vec<McpPanelLine> {
        if budget == 0 {
            return Vec::new();
        }
        let notice = self.notice_line();
        let content_budget = budget.saturating_sub(usize::from(notice.is_some()));
        let prompt = if let Some(form) = self.form.as_ref() {
            let all = form_lines(form);
            let selected = all
                .iter()
                .position(|line| {
                    line.hits
                        .iter()
                        .any(|hit| hit.target == McpPanelHitTarget::FormField(form.field))
                })
                .unwrap_or(0);
            let shown = content_budget.min(all.len());
            let start = selected
                .saturating_add(1)
                .saturating_sub(shown)
                .min(all.len().saturating_sub(shown));
            all[start..start + shown].to_vec()
        } else if let Some(confirm) = self.confirm.as_ref() {
            vec![
                McpPanelLine::new(McpPanelTone::Warn, format!("Remove `{}`?", confirm.name)),
                confirm_choice_line(confirm),
            ]
            .into_iter()
            .rev()
            .take(content_budget)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect()
        } else {
            Vec::new()
        };
        let mut lines = prompt;
        lines.extend(notice);
        lines.truncate(budget);
        lines
    }

    fn notice_line(&self) -> Option<McpPanelLine> {
        self.notice.as_ref().map(|notice| {
            McpPanelLine::new(
                if notice.ok {
                    McpPanelTone::Good
                } else {
                    McpPanelTone::Bad
                },
                notice.text.clone(),
            )
        })
    }

    fn move_focused_by(&mut self, delta: isize) {
        if self.focus == McpPanelFocus::Actions {
            self.action_selected =
                move_clamped(self.action_selected, delta, McpOperation::ALL.len());
        } else {
            self.selected = move_clamped(self.selected, delta, self.rows.len());
            self.section_offset = 0;
        }
    }

    fn select_mouse_target(&mut self, target: McpPanelHitTarget) {
        match target {
            McpPanelHitTarget::Server(index) if index < self.rows.len() && !self.is_prompting() => {
                self.selected = index;
                self.focus = McpPanelFocus::Servers;
                self.section_offset = 0;
            }
            McpPanelHitTarget::Section(section) if !self.is_prompting() => {
                self.section = section;
                self.focus = if section == McpPanelSection::Actions {
                    McpPanelFocus::Actions
                } else {
                    McpPanelFocus::Servers
                };
                self.section_offset = 0;
            }
            McpPanelHitTarget::Action(index)
                if !self.is_prompting() && index < McpOperation::ALL.len() =>
            {
                self.section = McpPanelSection::Actions;
                self.focus = McpPanelFocus::Actions;
                self.action_selected = index;
            }
            McpPanelHitTarget::FormField(field) => {
                if let Some(form) = self.form.as_mut()
                    && (field != McpFormField::Name || form.operation == McpOperation::Add)
                {
                    form.field = field;
                }
            }
            McpPanelHitTarget::Confirm(remove) => {
                if let Some(confirm) = self.confirm.as_mut() {
                    confirm.remove = remove;
                }
            }
            McpPanelHitTarget::Server(_)
            | McpPanelHitTarget::Section(_)
            | McpPanelHitTarget::Action(_) => {}
        }
    }

    fn section_lines(&self) -> Vec<McpPanelLine> {
        let view = section_view(self.section, self.selected_row(), self.support);
        if self.section == McpPanelSection::Actions {
            let actions = self.actions();
            return actions
                .iter()
                .enumerate()
                .map(|(index, action)| {
                    let selected =
                        index == self.action_selected && self.focus == McpPanelFocus::Actions;
                    McpPanelLine::new(
                        if selected {
                            McpPanelTone::Selected
                        } else if action.is_available() {
                            McpPanelTone::Body
                        } else {
                            McpPanelTone::Warn
                        },
                        match action.unavailable_reason.as_deref() {
                            Some(reason) => format!(
                                "{}{} — unavailable: {reason}",
                                if selected { "● " } else { "  " },
                                action.label
                            ),
                            None => {
                                format!("{}{}", if selected { "● " } else { "  " }, action.label)
                            }
                        },
                    )
                    .with_full_hit(McpPanelHitTarget::Action(index))
                })
                .collect();
        }
        let tone = match view.state {
            McpSectionState::Live => McpPanelTone::Body,
            McpSectionState::NotAdvertised | McpSectionState::NoEvidence => McpPanelTone::Dim,
            McpSectionState::Unsupported => McpPanelTone::Warn,
        };
        view.lines
            .into_iter()
            .map(|line| McpPanelLine::new(tone, format!("  {line}")))
            .collect()
    }

    fn hint(&self) -> &'static str {
        if self.confirm.is_some() {
            "←→ choose · enter confirm · esc cancel"
        } else if self.form.is_some() {
            "tab field · ←→ transport · enter submit · esc cancel"
        } else if self.section == McpPanelSection::Actions {
            "↑↓ action · ←→ server · tab section · enter run · esc close"
        } else {
            "↑↓ server · tab section · enter actions · esc close"
        }
    }

    /// Apply one key press.
    pub fn handle_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> McpPanelKeyOutcome {
        if self.confirm.is_some() {
            return self.handle_confirm_key(code);
        }
        if self.form.is_some() {
            return self.handle_form_key(code, modifiers);
        }
        match code {
            KeyCode::Esc => McpPanelKeyOutcome::Close,
            KeyCode::Tab => {
                self.section = self.section.next();
                self.section_offset = 0;
                self.focus = if self.section == McpPanelSection::Actions {
                    McpPanelFocus::Actions
                } else {
                    McpPanelFocus::Servers
                };
                McpPanelKeyOutcome::Handled
            }
            KeyCode::BackTab => {
                self.section = self.section.previous();
                self.section_offset = 0;
                self.focus = if self.section == McpPanelSection::Actions {
                    McpPanelFocus::Actions
                } else {
                    McpPanelFocus::Servers
                };
                McpPanelKeyOutcome::Handled
            }
            KeyCode::PageUp => {
                self.section_offset = self.section_offset.saturating_sub(3);
                McpPanelKeyOutcome::Handled
            }
            KeyCode::PageDown => {
                self.section_offset = self.section_offset.saturating_add(3);
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Up | KeyCode::Down if self.focus == McpPanelFocus::Actions => {
                let count = McpOperation::ALL.len();
                self.action_selected = if code == KeyCode::Up {
                    self.action_selected
                        .checked_sub(1)
                        .unwrap_or(count.saturating_sub(1))
                } else {
                    (self.action_selected + 1) % count
                };
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                self.move_server(matches!(code, KeyCode::Down | KeyCode::Right));
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Enter if self.focus == McpPanelFocus::Actions => self.run_selected_action(),
            KeyCode::Enter => {
                self.section = McpPanelSection::Actions;
                self.focus = McpPanelFocus::Actions;
                McpPanelKeyOutcome::Handled
            }
            _ => McpPanelKeyOutcome::Handled,
        }
    }

    fn move_server(&mut self, forward: bool) {
        if self.rows.is_empty() {
            return;
        }
        self.selected = if forward {
            (self.selected + 1) % self.rows.len()
        } else {
            self.selected
                .checked_sub(1)
                .unwrap_or(self.rows.len().saturating_sub(1))
        };
        self.section_offset = 0;
    }

    fn run_selected_action(&mut self) -> McpPanelKeyOutcome {
        let actions = self.actions();
        let Some(action) = actions.get(self.action_selected) else {
            return McpPanelKeyOutcome::Handled;
        };
        if let Some(reason) = action.unavailable_reason.as_deref() {
            self.note_failure(format!("{} unavailable: {reason}", action.label));
            return McpPanelKeyOutcome::Handled;
        }
        let row = self.selected_row().cloned();
        match action.operation {
            McpOperation::Add => {
                self.notice = None;
                self.form = Some(McpPanelForm {
                    operation: McpOperation::Add,
                    name: String::new(),
                    transport: McpTransportKind::Stdio,
                    target: String::new(),
                    field: McpFormField::Name,
                });
                McpPanelKeyOutcome::Handled
            }
            McpOperation::List => McpPanelKeyOutcome::Run(McpPanelIntent::ListProbed),
            McpOperation::Auth => row.map_or(McpPanelKeyOutcome::Handled, |row| {
                McpPanelKeyOutcome::Run(McpPanelIntent::Auth { name: row.name })
            }),
            McpOperation::Test => row.map_or(McpPanelKeyOutcome::Handled, |row| {
                McpPanelKeyOutcome::Run(McpPanelIntent::Test { name: row.name })
            }),
            McpOperation::Edit => {
                if let Some(row) = row {
                    self.notice = None;
                    self.form = Some(McpPanelForm {
                        operation: McpOperation::Edit,
                        name: row.name,
                        transport: row.transport,
                        // The stored target is never seeded back into the form:
                        // an HTTP target is rendered with its query elided, and
                        // re-submitting that rendering would silently rewrite
                        // the definition to the elided text.
                        target: String::new(),
                        field: McpFormField::Target,
                    });
                }
                McpPanelKeyOutcome::Handled
            }
            McpOperation::Enable => row.map_or(McpPanelKeyOutcome::Handled, |row| {
                McpPanelKeyOutcome::Run(McpPanelIntent::Enable {
                    name: row.name,
                    on: !row.enabled,
                })
            }),
            McpOperation::Remove => {
                if let Some(row) = row {
                    self.confirm = Some(McpPanelConfirm {
                        name: row.name,
                        remove: false,
                    });
                }
                McpPanelKeyOutcome::Handled
            }
        }
    }

    fn handle_confirm_key(&mut self, code: KeyCode) -> McpPanelKeyOutcome {
        match code {
            KeyCode::Esc => {
                self.confirm = None;
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => {
                if let Some(confirm) = self.confirm.as_mut() {
                    confirm.remove = !confirm.remove;
                }
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Enter => match self.confirm.take() {
                Some(confirm) if confirm.remove => {
                    McpPanelKeyOutcome::Run(McpPanelIntent::Remove { name: confirm.name })
                }
                _ => McpPanelKeyOutcome::Handled,
            },
            _ => McpPanelKeyOutcome::Handled,
        }
    }

    fn handle_form_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> McpPanelKeyOutcome {
        match code {
            KeyCode::Esc => {
                self.form = None;
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Tab | KeyCode::Down => {
                if let Some(form) = self.form.as_mut() {
                    form.field = next_field(form.operation, form.field, true);
                }
                McpPanelKeyOutcome::Handled
            }
            KeyCode::BackTab | KeyCode::Up => {
                if let Some(form) = self.form.as_mut() {
                    form.field = next_field(form.operation, form.field, false);
                }
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Left | KeyCode::Right => {
                if let Some(form) = self.form.as_mut()
                    && form.field == McpFormField::Transport
                {
                    form.transport = match form.transport {
                        McpTransportKind::Stdio => McpTransportKind::StreamableHttp,
                        McpTransportKind::StreamableHttp => McpTransportKind::Stdio,
                    };
                }
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Backspace => {
                if let Some(form) = self.form.as_mut() {
                    match form.field {
                        McpFormField::Name => form.name.pop(),
                        McpFormField::Target => form.target.pop(),
                        McpFormField::Transport => None,
                    };
                }
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Char(character)
                if !modifiers.contains(KeyModifiers::CONTROL)
                    && !modifiers.contains(KeyModifiers::ALT) =>
            {
                if let Some(form) = self.form.as_mut() {
                    match form.field {
                        McpFormField::Name => {
                            append_name(&mut form.name, std::iter::once(character));
                        }
                        McpFormField::Target => {
                            append_target(&mut form.target, std::iter::once(character));
                        }
                        McpFormField::Transport => {}
                    }
                }
                McpPanelKeyOutcome::Handled
            }
            KeyCode::Enter => match self.form.take() {
                Some(form) if form.operation == McpOperation::Add => {
                    McpPanelKeyOutcome::Run(McpPanelIntent::Add {
                        name: form.name,
                        transport: form.transport,
                        target: form.target,
                    })
                }
                Some(form) => McpPanelKeyOutcome::Run(McpPanelIntent::Edit {
                    name: form.name,
                    transport: form.transport,
                    target: form.target,
                }),
                None => McpPanelKeyOutcome::Handled,
            },
            _ => McpPanelKeyOutcome::Handled,
        }
    }
}

const fn next_field(operation: McpOperation, field: McpFormField, forward: bool) -> McpFormField {
    // `edit` keeps the server it was opened on, so its name is not a field.
    let has_name = matches!(operation, McpOperation::Add);
    match (field, forward) {
        (McpFormField::Name, true) | (McpFormField::Target, false) => McpFormField::Transport,
        (McpFormField::Transport, true) => McpFormField::Target,
        (McpFormField::Transport, false) if has_name => McpFormField::Name,
        (McpFormField::Transport, false) => McpFormField::Target,
        (McpFormField::Target, true) if has_name => McpFormField::Name,
        (McpFormField::Target, true) => McpFormField::Transport,
        (McpFormField::Name, false) => McpFormField::Target,
    }
}

fn form_lines(form: &McpPanelForm) -> Vec<McpPanelLine> {
    let mut lines = vec![McpPanelLine::new(
        McpPanelTone::Heading,
        match form.operation {
            McpOperation::Edit => format!("edit `{}`", form.name),
            _ => "add server".to_owned(),
        },
    )];
    if form.operation == McpOperation::Add {
        lines.push(field_line(
            "name",
            &form.name,
            form.field == McpFormField::Name,
            McpFormField::Name,
        ));
    }
    lines.push(field_line(
        "transport",
        transport_word(form.transport),
        form.field == McpFormField::Transport,
        McpFormField::Transport,
    ));
    lines.push(field_line(
        match form.transport {
            McpTransportKind::Stdio => "command",
            McpTransportKind::StreamableHttp => "url",
        },
        // The typed target is echoed only while the operator is typing it, and
        // never reaches a notice or an error line afterwards.
        &form.target,
        form.field == McpFormField::Target,
        McpFormField::Target,
    ));
    lines
}

fn field_line(label: &str, value: &str, selected: bool, field: McpFormField) -> McpPanelLine {
    McpPanelLine::new(
        if selected {
            McpPanelTone::Selected
        } else {
            McpPanelTone::Body
        },
        format!(
            "{}{label:<10} {}",
            if selected { "● " } else { "  " },
            if value.is_empty() { "—" } else { value }
        ),
    )
    .with_full_hit(McpPanelHitTarget::FormField(field))
}

fn confirm_choice_line(confirm: &McpPanelConfirm) -> McpPanelLine {
    McpPanelLine::new(
        McpPanelTone::Selected,
        if confirm.remove {
            "  cancel   [remove]"
        } else {
            "  [cancel]   remove"
        },
    )
    .with_hits(vec![
        McpPanelLineHit {
            start: 0,
            end: 11,
            target: McpPanelHitTarget::Confirm(false),
        },
        McpPanelLineHit {
            start: 11,
            end: usize::MAX,
            target: McpPanelHitTarget::Confirm(true),
        },
    ])
}

fn append_name(value: &mut String, characters: impl Iterator<Item = char>) {
    let remaining = MAX_MCP_NAME_CHARS.saturating_sub(value.chars().count());
    value.extend(
        characters
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .take(remaining),
    );
}

fn append_target(value: &mut String, characters: impl Iterator<Item = char>) {
    let remaining = MAX_MCP_TARGET_CHARS.saturating_sub(value.chars().count());
    value.extend(
        characters
            .filter(|character| !character.is_control())
            .take(remaining),
    );
}

fn move_clamped(current: usize, delta: isize, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current
            .saturating_add(delta.unsigned_abs())
            .min(len.saturating_sub(1))
    }
}

/// U03 descriptor for the MCP panel contribution.
///
/// Shared with the plugin so the registered identity and the tested identity
/// cannot drift.
///
/// # Errors
/// Static descriptor validation failure.
pub fn panel_descriptor()
-> Result<heycode_ui::UiContributionDescriptor, heycode_ui::UiRegistryError> {
    heycode_ui::UiContributionDescriptor::new(heycode_ui::UiSlot::Panel, "mcp", "MCP servers", 60)
}
