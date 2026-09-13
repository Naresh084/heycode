//! CMD09/CMD10 commands over one private human-control inbox.
//!
//! Diff/copy/mention/theme/keymap/Vim requests never enter the Agent session.
//! Inspection commands schedule native child turns with a read-only ceiling
//! and durably record their user intent, child identity and result.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    AdvisorSelection, AdvisorService, Command, CommandArgument, CommandAvailability,
    CommandDescriptor, CommandSource, CommandTiming,
};
use heycode_ui::keymap::{KeyChord, Keymap, KeymapAction, SettingsBackedKeymap};
use heycode_ui::preferences::{SettingsBackedUiPreferences, ShellChromePreferences};
use tokio_util::sync::CancellationToken;

const MAX_PENDING_REQUESTS: usize = 8;
const MAX_MENTION_CHARS: usize = 4_096;
const MAX_REVIEW_INSTRUCTIONS_CHARS: usize = 16_384;

/// One UI-only command request consumed by the running AppState.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HumanCommandRequest {
    /// Open the committed-diff side panel.
    OpenDiff,
    /// Copy one completed assistant answer, zero-based from the latest.
    CopyAnswer {
        /// `0` is the latest completed answer, `1` the second latest, and so on.
        latest_index: usize,
    },
    /// Defer invalid copy arguments until the frontend has checked its answer history.
    CopyArgumentError {
        /// Bounded human-readable argument error.
        message: String,
    },
    /// Open mention navigation, or insert the supplied reference.
    Mention(Option<String>),
    /// Open the live theme preview.
    OpenTheme {
        /// Current live theme catalog.
        themes: Vec<heycode_ui::theme::Theme>,
        /// Persisted selection.
        selected_id: String,
        /// Exact preference CAS revision.
        revision: u64,
    },
    /// Open the shortcut browser/editor.
    OpenKeymap {
        /// Current resolved bindings.
        keymap: Keymap,
        /// Exact keymap Settings revision.
        revision: u64,
    },
    /// Open the mouse-wheel speed picker and preview ruler.
    OpenScrollSpeed {
        /// Current speed encoded in quarter steps (4 is 1x).
        quarters: u8,
        /// Exact UI preference CAS revision.
        revision: u64,
    },
    /// Apply a committed theme generation to live state.
    ApplyTheme(heycode_ui::theme::Theme),
    /// Apply a committed Vim-mode generation to live state.
    ApplyVim(bool),
    /// Apply committed persistent header/footer choices to live state.
    ApplyShell(ShellChromePreferences),
    /// Apply a committed mouse-wheel multiplier to the live transcript.
    ApplyScrollSpeed {
        /// Speed encoded in quarter steps (4 is 1x).
        quarters: u8,
    },
    /// Apply a committed keymap generation to live state.
    ApplyKeymap(Keymap),
    /// Apply the persisted compact transcript projection.
    ApplyFocus(bool),
    /// Set the current session's composer accent; `None` restores the theme.
    ApplyPromptColor(Option<PromptColor>),
}

/// Named prompt-bar accents supported by Claude's `/color` contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptColor {
    /// Red session accent.
    Red,
    /// Blue session accent.
    Blue,
    /// Green session accent.
    Green,
    /// Yellow session accent.
    Yellow,
    /// Purple session accent.
    Purple,
    /// Orange session accent.
    Orange,
    /// Pink session accent.
    Pink,
    /// Cyan session accent.
    Cyan,
}

impl PromptColor {
    const ALL: [Self; 8] = [
        Self::Red,
        Self::Blue,
        Self::Green,
        Self::Yellow,
        Self::Purple,
        Self::Orange,
        Self::Pink,
        Self::Cyan,
    ];

    /// Stable command/display name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Red => "red",
            Self::Blue => "blue",
            Self::Green => "green",
            Self::Yellow => "yellow",
            Self::Purple => "purple",
            Self::Orange => "orange",
            Self::Pink => "pink",
            Self::Cyan => "cyan",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == value)
    }

    fn random() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.subsec_nanos());
        Self::ALL[usize::try_from(nanos).unwrap_or_default() % Self::ALL.len()]
    }
}

#[derive(Default)]
struct BridgeState {
    attached: bool,
    pending: VecDeque<HumanCommandRequest>,
}

/// Shared, bounded human-command inbox.
#[derive(Clone, Default)]
pub struct HumanCommandBridge {
    state: Arc<Mutex<BridgeState>>,
}

impl HumanCommandBridge {
    /// Empty detached inbox.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Mark the live shell consumer attached.
    pub fn attach(&self) {
        self.lock().attached = true;
    }

    /// Whether a live shell consumes requests.
    #[must_use]
    pub fn is_attached(&self) -> bool {
        self.lock().attached
    }

    /// Queue one request. The oldest request is discarded only if a broken
    /// caller exceeds the small one-command-at-a-time product bound.
    pub fn request(&self, request: HumanCommandRequest) {
        let mut state = self.lock();
        if state.pending.len() == MAX_PENDING_REQUESTS {
            state.pending.pop_front();
        }
        state.pending.push_back(request);
    }

    /// Take the next request.
    #[must_use]
    pub fn take(&self) -> Option<HumanCommandRequest> {
        self.lock().pending.pop_front()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BridgeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for HumanCommandBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        formatter
            .debug_struct("HumanCommandBridge")
            .field("attached", &state.attached)
            .field("pending_count", &state.pending.len())
            .finish()
    }
}

struct HumanOnlyCommand {
    descriptor: CommandDescriptor,
    bridge: HumanCommandBridge,
    kind: HumanOnlyKind,
    settings: Option<SettingsBackedUiPreferences>,
    detached: CommandAvailability,
}

#[derive(Debug, Clone, Copy)]
enum HumanOnlyKind {
    Diff,
    Copy,
    Mention,
    Focus,
    Color,
}

#[async_trait]
impl Command for HumanOnlyCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let request = match self.kind {
            HumanOnlyKind::Diff => {
                no_args("diff", args)?;
                HumanCommandRequest::OpenDiff
            }
            HumanOnlyKind::Copy => match copy_latest_index(args) {
                Ok(latest_index) => HumanCommandRequest::CopyAnswer { latest_index },
                Err(error) => HumanCommandRequest::CopyArgumentError {
                    message: error.to_string(),
                },
            },
            HumanOnlyKind::Mention => {
                let reference = optional_human_text(args, MAX_MENTION_CHARS)?;
                HumanCommandRequest::Mention(reference)
            }
            HumanOnlyKind::Focus => {
                no_args("focus", args)?;
                let settings = self.settings.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("focus preferences are unavailable in this profile")
                })?;
                let current = settings.load()?;
                let committed = settings
                    .store_focus_view(!current.preferences.focus_view(), current.revision)?;
                HumanCommandRequest::ApplyFocus(committed.preferences.focus_view())
            }
            HumanOnlyKind::Color => HumanCommandRequest::ApplyPromptColor(prompt_color(args)?),
        };
        self.bridge.request(request);
        Ok(())
    }
}

struct InspectionCommand {
    descriptor: CommandDescriptor,
    preset_id: &'static str,
    registry: Option<Arc<heycode_agent::SubagentRegistry>>,
    unavailable: CommandAvailability,
}

struct AdvisorCommand {
    descriptor: CommandDescriptor,
    bridge: crate::advisor_panel::AdvisorPanelBridge,
    service: Option<Arc<AdvisorService>>,
    unavailable: CommandAvailability,
}

impl AdvisorCommand {
    async fn open(
        &self,
        service: &AdvisorService,
        cancellation: CancellationToken,
    ) -> Result<(), anyhow::Error> {
        let status = service.status()?;
        self.bridge
            .request(crate::advisor_panel::AdvisorPanelView::loading(
                &status,
                cancellation.clone(),
            ));
        let mut catalog = match service.refresh_route_choices(cancellation.clone()).await {
            Ok(catalog) => catalog,
            Err(heycode_agent::AdvisorError::Cancelled) => return Ok(()),
            Err(error) => {
                if !cancellation.is_cancelled() {
                    self.bridge.request(
                        crate::advisor_panel::AdvisorPanelView::new(
                            &status,
                            service.route_choices(),
                        )
                        .with_warnings(vec![format!(
                            "Advisor model catalogs could not load: {error}"
                        )])
                        .for_operation(cancellation),
                    );
                }
                return Ok(());
            }
        };
        if cancellation.is_cancelled() {
            return Ok(());
        }
        let status = match service.status() {
            Ok(status) => status,
            Err(error) => {
                catalog.warnings.push(format!(
                    "Advisor status changed while models loaded: {error}; showing the opening selection"
                ));
                status
            }
        };
        self.bridge.request(
            crate::advisor_panel::AdvisorPanelView::new(&status, catalog.choices)
                .with_warnings(catalog.warnings)
                .for_operation(cancellation),
        );
        Ok(())
    }

    async fn execute_inner(
        &self,
        agent: &heycode_agent::Agent,
        args: &str,
        cancellation: CancellationToken,
    ) -> anyhow::Result<()> {
        let service = self.service.as_ref().ok_or_else(|| {
            anyhow::anyhow!("persistent advisor control is not configured in this profile")
        })?;
        if !self.bridge.is_attached() {
            anyhow::bail!("interactive TUI is not attached");
        }
        let arguments = args.split_whitespace().collect::<Vec<_>>();
        match arguments.as_slice() {
            [] => self.open(service, cancellation).await,
            ["status" | "details"] => {
                agent.ui().emit(heycode_agent::UiEvent::Info {
                    text: service.status()?.plain_text(),
                });
                Ok(())
            }
            ["off"] => {
                service.disable(agent)?;
                agent.ui().emit(heycode_agent::UiEvent::Info {
                    text: "Advisor disabled".to_owned(),
                });
                Ok(())
            }
            [owner, model] | [owner, model, _]
                if owner.starts_with("native:") || owner.starts_with("runtime:") =>
            {
                let effort = arguments.get(2).map(|value| (*value).to_owned());
                let selection = AdvisorSelection::new(
                    heycode_agent::parse_advisor_owner(owner)?,
                    *model,
                    effort,
                )?;
                let status = service.select(agent, selection)?;
                let committed = status
                    .selection
                    .as_ref()
                    .map_or_else(|| "disabled".to_owned(), AdvisorSelection::display);
                agent.ui().emit(heycode_agent::UiEvent::Info {
                    text: format!("Advisor set to {committed}"),
                });
                Ok(())
            }
            [_, _] | [_, _, _] => anyhow::bail!(
                "advisor selection requires an explicit owner; use `/advisor native:<provider> <model> [effort]`, or `/ask-advisor <instructions>` for the former one-shot command"
            ),
            _ => anyhow::bail!(
                "usage: /advisor [status|details|off|<native:provider|runtime:id> <model> [effort]]; use /ask-advisor for a one-shot question"
            ),
        }
    }
}

#[async_trait]
impl Command for AdvisorCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() && self.service.is_some() {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        self.execute_inner(agent, args, CancellationToken::new())
            .await
    }

    async fn execute_cancellable(
        &self,
        agent: &heycode_agent::Agent,
        args: &str,
        cancellation: CancellationToken,
    ) -> anyhow::Result<()> {
        self.execute_inner(agent, args, cancellation).await
    }
}

#[async_trait]
impl Command for InspectionCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.registry.as_ref().is_some_and(|registry| {
            registry
                .descriptors()
                .iter()
                .any(|provider| provider.id().as_str() == "native")
                && registry.preset(self.preset_id).is_some()
        }) {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let instructions = optional_human_text(args, MAX_REVIEW_INSTRUCTIONS_CHARS)?;
        let registry = self.registry.as_ref().ok_or_else(|| {
            anyhow::anyhow!("native inspection runtime is not configured in this profile")
        })?;
        agent
            .run_native_inspection(
                registry,
                self.descriptor.id(),
                instructions.as_deref().unwrap_or_default(),
            )
            .await
    }
}

struct ThemeCommand {
    descriptor: CommandDescriptor,
    bridge: HumanCommandBridge,
    settings: SettingsBackedUiPreferences,
    ui: Arc<heycode_ui::UiRegistry>,
    detached: CommandAvailability,
}

#[async_trait]
impl Command for ThemeCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let Some(id) = optional_human_text(args, 128)? else {
            let current = self.settings.load()?;
            self.bridge.request(HumanCommandRequest::OpenTheme {
                themes: self.ui.themes()?,
                selected_id: current.preferences.theme_id().to_owned(),
                revision: current.revision,
            });
            return Ok(());
        };
        let theme = self
            .ui
            .theme(&id)?
            .ok_or_else(|| anyhow::anyhow!("unknown theme `{id}`"))?;
        let current = self.settings.load()?;
        let committed = self.settings.store_theme(id, current.revision)?;
        let _ = committed;
        self.bridge.request(HumanCommandRequest::ApplyTheme(theme));
        Ok(())
    }
}

struct KeymapCommand {
    descriptor: CommandDescriptor,
    bridge: HumanCommandBridge,
    settings: SettingsBackedKeymap,
    detached: CommandAvailability,
}

#[async_trait]
impl Command for KeymapCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let args = args.trim();
        if args.is_empty() {
            let (keymap, revision) = self.settings.load_versioned()?;
            self.bridge
                .request(HumanCommandRequest::OpenKeymap { keymap, revision });
            return Ok(());
        }
        let (current, revision) = self.settings.load_versioned()?;
        let next = if args == "reset" {
            Keymap::defaults()
        } else {
            let mut parts = args.split_whitespace();
            let action_text = parts.next().unwrap_or_default();
            let chord_text = parts.next().unwrap_or_default();
            if action_text.is_empty() || chord_text.is_empty() || parts.next().is_some() {
                anyhow::bail!("usage: /keymap [reset|<action> <chord>]");
            }
            let action = KeymapAction::parse(action_text)?;
            let chord = KeyChord::parse(chord_text)?;
            let mut overrides: BTreeMap<KeymapAction, KeyChord> = current.overrides();
            overrides.insert(action, chord);
            Keymap::resolve(&overrides)?
        };
        self.settings.store_at(&next, revision)?;
        self.bridge.request(HumanCommandRequest::ApplyKeymap(next));
        Ok(())
    }
}

struct VimCommand {
    descriptor: CommandDescriptor,
    bridge: HumanCommandBridge,
    settings: SettingsBackedUiPreferences,
    detached: CommandAvailability,
}

struct ScrollSpeedCommand {
    descriptor: CommandDescriptor,
    bridge: HumanCommandBridge,
    settings: SettingsBackedUiPreferences,
    detached: CommandAvailability,
}

#[async_trait]
impl Command for ScrollSpeedCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        no_args("scroll-speed", args)?;
        let current = self.settings.load()?;
        self.bridge.request(HumanCommandRequest::OpenScrollSpeed {
            quarters: (current.preferences.scroll_speed() * 4.0) as u8,
            revision: current.revision,
        });
        Ok(())
    }
}

struct StatuslineCommand {
    descriptor: CommandDescriptor,
    bridge: HumanCommandBridge,
    settings: SettingsBackedUiPreferences,
    detached: CommandAvailability,
}

#[async_trait]
impl Command for StatuslineCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let current = self.settings.load()?;
        let mut shell = current.preferences.shell();
        let tokens = args.split_whitespace().collect::<Vec<_>>();
        if tokens.is_empty() {
            agent.ui().emit(heycode_agent::UiEvent::Info {
                text: format!(
                    "foregrounded-agent status row: {}; footer hints: {}",
                    on_off(shell.footer_status()),
                    on_off(shell.footer_hints())
                ),
            });
            return Ok(());
        }
        shell = match tokens.as_slice() {
            ["on"] => shell.with_footer_status(true),
            ["off"] => shell.with_footer_status(false),
            ["hints", "on"] => shell.with_footer_hints(true),
            ["hints", "off"] => shell.with_footer_hints(false),
            ["reset"] => shell.with_footer_status(true).with_footer_hints(true),
            _ => anyhow::bail!("usage: /statusline [on|off|hints on|hints off|reset]"),
        };
        let committed = self.settings.store_shell(shell, current.revision)?;
        self.bridge.request(HumanCommandRequest::ApplyShell(
            committed.preferences.shell(),
        ));
        agent.ui().emit(heycode_agent::UiEvent::Info {
            text: format!(
                "foregrounded-agent status row: {}; footer hints: {}",
                on_off(shell.footer_status()),
                on_off(shell.footer_hints())
            ),
        });
        Ok(())
    }
}

#[async_trait]
impl Command for VimCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached() {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, _agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let current = self.settings.load()?;
        let enabled = match args.trim() {
            "" | "toggle" => !current.preferences.vim_mode(),
            "on" => true,
            "off" => false,
            _ => anyhow::bail!("usage: /vim [on|off|toggle]"),
        };
        let committed = self.settings.store_vim_mode(enabled, current.revision)?;
        self.bridge.request(HumanCommandRequest::ApplyVim(
            committed.preferences.vim_mode(),
        ));
        Ok(())
    }
}

/// Build every TUI-owned CMD09/CMD10 command. Inspection commands remain
/// discoverable but unavailable when the profile omits native subagents.
///
/// # Errors
/// Static command metadata validation failure.
pub fn commands(
    source: CommandSource,
    bridge: HumanCommandBridge,
    settings: Arc<heycode_settings::SettingsService>,
    ui: Arc<heycode_ui::UiRegistry>,
    subagents: Option<Arc<heycode_agent::SubagentRegistry>>,
) -> Result<Vec<Arc<dyn Command>>, heycode_agent::CommandMetadataError> {
    commands_with_advisor(
        source,
        bridge,
        settings,
        ui,
        subagents,
        None,
        crate::advisor_panel::AdvisorPanelBridge::new(),
    )
}

/// Build every TUI-owned command with the optional persistent advisor owner.
/// Minimal/test profiles may omit it; `/advisor` then remains discoverable but
/// unavailable, while `/ask-advisor` keeps the legacy one-shot inspection.
///
/// # Errors
/// Static command metadata validation failure.
pub fn commands_with_advisor(
    source: CommandSource,
    bridge: HumanCommandBridge,
    settings: Arc<heycode_settings::SettingsService>,
    ui: Arc<heycode_ui::UiRegistry>,
    subagents: Option<Arc<heycode_agent::SubagentRegistry>>,
    advisor: Option<Arc<AdvisorService>>,
    advisor_bridge: crate::advisor_panel::AdvisorPanelBridge,
) -> Result<Vec<Arc<dyn Command>>, heycode_agent::CommandMetadataError> {
    let detached = CommandAvailability::unavailable("interactive TUI is not attached")?;
    let descriptor = |id, description, arguments, timing| {
        CommandDescriptor::new(id, description, arguments, timing, source.clone())
    };
    let mut result: Vec<Arc<dyn Command>> = Vec::new();
    for (id, description, kind) in [
        (
            "diff",
            "Open the committed workspace diff",
            HumanOnlyKind::Diff,
        ),
        (
            "copy",
            "Copy the Nth-latest completed assistant answer",
            HumanOnlyKind::Copy,
        ),
    ] {
        result.push(Arc::new(HumanOnlyCommand {
            descriptor: descriptor(
                id,
                description,
                if id == "copy" {
                    vec![CommandArgument::optional(
                        "number",
                        "One-based completed-answer position; defaults to 1",
                    )?]
                } else {
                    Vec::new()
                },
                CommandTiming::Immediate,
            )?,
            bridge: bridge.clone(),
            kind,
            settings: None,
            detached: detached.clone(),
        }));
    }
    result.push(Arc::new(HumanOnlyCommand {
        descriptor: descriptor(
            "mention",
            "Attach or insert a file, session, or reference",
            vec![CommandArgument::optional("reference", "File, session, or reference")?.variadic()],
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        kind: HumanOnlyKind::Mention,
        settings: None,
        detached: detached.clone(),
    }));
    result.push(Arc::new(HumanOnlyCommand {
        descriptor: descriptor(
            "focus",
            "Toggle the compact current-turn transcript view",
            Vec::new(),
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        kind: HumanOnlyKind::Focus,
        settings: Some(SettingsBackedUiPreferences::new(settings.clone())),
        detached: detached.clone(),
    }));
    result.push(Arc::new(HumanOnlyCommand {
        descriptor: descriptor(
            "color",
            "Set the current session color shown in the prompt bar",
            vec![CommandArgument::optional(
                "color",
                "red, blue, green, yellow, purple, orange, pink, cyan, or default",
            )?],
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        kind: HumanOnlyKind::Color,
        settings: None,
        detached: detached.clone(),
    }));
    result.push(Arc::new(AdvisorCommand {
        descriptor: descriptor(
            "advisor",
            "Choose the persistent model consulted for stronger judgment",
            vec![
                CommandArgument::optional(
                    "selection",
                    "status/details, off, or explicit owner and model with optional effort",
                )?
                .variadic(),
            ],
            CommandTiming::Immediate,
        )?,
        bridge: advisor_bridge,
        service: advisor,
        unavailable: CommandAvailability::unavailable(
            "persistent advisor control or interactive TUI is not configured in this profile",
        )?,
    }));
    for (id, preset_id, description) in [
        (
            "review",
            "reviewer",
            "Review current changes with a read-only native reviewer",
        ),
        (
            "ask-advisor",
            "advisor",
            "Ask a one-shot read-only native technical advisor",
        ),
        (
            "security-review",
            "security-review",
            "Inspect security with a read-only native reviewer",
        ),
    ] {
        result.push(Arc::new(InspectionCommand {
            descriptor: descriptor(
                id,
                description,
                vec![
                    CommandArgument::optional(
                        "instructions",
                        "Question or additional inspection instructions",
                    )?
                    .variadic(),
                ],
                CommandTiming::ModelScheduling,
            )?,
            preset_id,
            registry: subagents.clone(),
            unavailable: CommandAvailability::unavailable(
                "native inspection runtime or preset is not configured in this profile",
            )?,
        }));
    }
    result.push(Arc::new(ThemeCommand {
        descriptor: descriptor(
            "theme",
            "Preview or persist a UI theme",
            vec![CommandArgument::optional("theme", "Live theme id")?],
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        settings: SettingsBackedUiPreferences::new(settings.clone()),
        ui,
        detached: detached.clone(),
    }));
    result.push(Arc::new(KeymapCommand {
        descriptor: descriptor(
            "keymap",
            "Browse, edit, or reset keyboard shortcuts",
            vec![
                CommandArgument::optional("action", "Action id or reset")?,
                CommandArgument::optional("chord", "Canonical key chord")?,
            ],
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        settings: SettingsBackedKeymap::new(settings.clone()),
        detached: detached.clone(),
    }));
    result.push(Arc::new(VimCommand {
        descriptor: descriptor(
            "vim",
            "Toggle or set composer Vim mode",
            vec![CommandArgument::optional("state", "on, off, or toggle")?],
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        settings: SettingsBackedUiPreferences::new(settings.clone()),
        detached: detached.clone(),
    }));
    result.push(Arc::new(ScrollSpeedCommand {
        descriptor: descriptor(
            "scroll-speed",
            "Preview and persist mouse-wheel scroll speed",
            Vec::new(),
            CommandTiming::Immediate,
        )?,
        bridge: bridge.clone(),
        settings: SettingsBackedUiPreferences::new(settings.clone()),
        detached: detached.clone(),
    }));
    result.push(Arc::new(StatuslineCommand {
        descriptor: descriptor(
            "statusline",
            "Show or configure the built-in status line",
            vec![
                CommandArgument::optional("action", "on, off, hints, or reset")?,
                CommandArgument::optional("value", "on or off after hints")?,
            ],
            CommandTiming::Immediate,
        )?,
        bridge,
        settings: SettingsBackedUiPreferences::new(settings),
        detached,
    }));
    Ok(result)
}

const fn on_off(enabled: bool) -> &'static str {
    if enabled { "on" } else { "off" }
}

fn no_args(command: &str, args: &str) -> anyhow::Result<()> {
    if args.trim().is_empty() {
        Ok(())
    } else {
        anyhow::bail!("usage: /{command}")
    }
}

fn prompt_color(args: &str) -> anyhow::Result<Option<PromptColor>> {
    let value = args.trim().to_ascii_lowercase();
    match value.as_str() {
        "" => Ok(Some(PromptColor::random())),
        "default" => Ok(None),
        name => PromptColor::parse(name).map(Some).ok_or_else(|| {
            anyhow::anyhow!("usage: /color [red|blue|green|yellow|purple|orange|pink|cyan|default]")
        }),
    }
}

fn copy_latest_index(args: &str) -> anyhow::Result<usize> {
    let number = args.trim();
    if number.is_empty() {
        return Ok(0);
    }
    let error = || {
        anyhow::anyhow!(
            "Usage: /copy [N] where N is 1 (latest), 2, 3, … Got: {}",
            crate::markdown::terminal_safe_span(&number.chars().take(128).collect::<String>())
        )
    };
    let one_based = number.parse::<usize>().map_err(|_| error())?;
    one_based.checked_sub(1).ok_or_else(error)
}

fn optional_human_text(args: &str, max_chars: usize) -> anyhow::Result<Option<String>> {
    let text = args.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if text.chars().count() > max_chars
        || text
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        anyhow::bail!("command text is invalid or too long");
    }
    Ok(Some(text.to_owned()))
}
