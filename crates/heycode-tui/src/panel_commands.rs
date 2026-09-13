//! CMD04: slash commands that open the shell's capability panels.
//!
//! A panel command carries no panel logic and owns no capability state. It
//! records which panel the human asked for on a shared [`PanelCommandBridge`];
//! the interactive loop drains that request on its next wake and opens the
//! panel that owns the capability. The wake is guaranteed rather than lucky:
//! the loop already selects on the command task it spawned, so a command that
//! returns wakes the loop that dispatched it.
//!
//! The bridge also records which panels the running shell actually attached
//! services for, and that is what [`heycode_agent::Command::availability`]
//! reports. A panel whose service layer was never wired can only render an
//! error, so the command says so in the palette — visible with a reason, per
//! CMD01 — instead of opening nothing and calling it a panel.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use heycode_agent::{
    Command, CommandArgument, CommandAvailability, CommandDescriptor, CommandMetadataError,
    CommandRegistry, CommandSource, CommandTiming, SubagentContinuation, SubagentId,
    SubagentProviderId, SubagentRegistry, SubagentRequest, SubagentSeed,
};
use heycode_core::{Context, CoreError, CoreResult, Plugin};
use heycode_llm::CapabilitySupport;
use heycode_session::{ScheduleId, ScheduleRecord, ScheduleRule};

/// A capability panel this shell owns and can open.
///
/// Closed on purpose: a variant here is a panel that exists in
/// [`crate::app::AppState`], not a capability heycode merely has. `/skills`,
/// `/agents` and `/hooks` have no variant because no such panel exists yet —
/// a command that opened an empty shell would be worse than its absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CapabilityPanel {
    /// MCP server management (U12) — [`crate::mcp_panel`].
    Mcp,
    /// Installed-plugin lifecycle (U13) — [`crate::plugin_panel`].
    Plugins,
    /// Discovered instruction packs contributed by `heycode-skills`.
    Skills,
    /// Registered delegation providers and aggregate live-child state.
    Agents,
    /// Effect-owned lifecycle hooks and available handler kinds.
    Hooks,
    /// Schema-derived/custom settings browser (U14).
    Settings,
    /// Native workflow phases, agents and lifecycle controls.
    Workflows,
}

impl CapabilityPanel {
    /// Every panel, in declaration order.
    pub const ALL: [Self; 7] = [
        Self::Mcp,
        Self::Plugins,
        Self::Skills,
        Self::Agents,
        Self::Hooks,
        Self::Settings,
        Self::Workflows,
    ];

    /// Stable slash id and diagnostic name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Mcp => "mcp",
            Self::Plugins => "plugins",
            Self::Skills => "skills",
            Self::Agents => "agents",
            Self::Hooks => "hooks",
            Self::Settings => "settings",
            Self::Workflows => "workflows",
        }
    }

    /// One-line command description shown in the palette.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Mcp => "Open or manage MCP servers",
            Self::Plugins => "Open the installed-plugin panel",
            Self::Skills => "Open the discovered-skills panel",
            Self::Agents => "Open the delegated-agent panel",
            Self::Hooks => "Open the lifecycle-hooks panel",
            Self::Settings => "Open the settings browser",
            Self::Workflows => "Open workflow phases and agents",
        }
    }

    /// Why the command cannot run while the shell has not attached this
    /// panel's service layer.
    #[must_use]
    pub const fn detached_reason(self) -> &'static str {
        match self {
            Self::Mcp => "MCP management is not attached to this shell",
            Self::Plugins => "Plugin lifecycle is not attached to this shell",
            Self::Skills => "Skills are not attached to this shell",
            Self::Agents => "Subagent registry is not attached to this shell",
            Self::Hooks => "Hook registry is not attached to this shell",
            Self::Settings => "Settings are not attached to this shell",
            Self::Workflows => "Workflow service is not attached to this shell",
        }
    }

    /// Resolve the stable cross-crate panel id emitted by a capability owner.
    #[must_use]
    pub fn from_id(id: &heycode_agent::UiPanelId) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|panel| panel.as_str() == id.as_str())
    }
}

/// Panels whose slash command this plugin owns.
///
/// [`CapabilityPanel::Plugins`] is deliberately absent: `/plugins` is already
/// registered by the built-in inventory command in `heycode-agent`, and
/// [`CommandRegistry`] fails a duplicate id loud rather than shadowing the
/// live owner. The panel itself stays reachable through the bridge, so
/// re-pointing `/plugins` at it is a one-line change once that id is free.
const COMMANDED_PANELS: [CapabilityPanel; 4] = [
    CapabilityPanel::Mcp,
    CapabilityPanel::Agents,
    CapabilityPanel::Hooks,
    CapabilityPanel::Workflows,
];

const MAX_SUBTASK_CHARS: usize = 16_384;

/// One bounded, control-free row in a read-only capability panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityPanelRow {
    name: String,
    detail: String,
}

impl CapabilityPanelRow {
    fn new(name: impl AsRef<str>, detail: impl AsRef<str>) -> Self {
        Self {
            name: safe_text(name.as_ref(), 128),
            detail: safe_text(detail.as_ref(), 384),
        }
    }

    /// Stable primary label.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Bounded safe metadata.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// Read-only catalog shared by skills, agents and hooks panels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCatalogView {
    panel: CapabilityPanel,
    title: String,
    summary: String,
    rows: Vec<CapabilityPanelRow>,
    selected: usize,
}

impl CapabilityCatalogView {
    fn new(
        panel: CapabilityPanel,
        title: impl AsRef<str>,
        summary: impl AsRef<str>,
        rows: Vec<CapabilityPanelRow>,
    ) -> Self {
        Self {
            panel,
            title: safe_text(title.as_ref(), 128),
            summary: safe_text(summary.as_ref(), 256),
            rows,
            selected: 0,
        }
    }

    /// Capability whose data this view owns.
    #[must_use]
    pub const fn panel(&self) -> CapabilityPanel {
        self.panel
    }

    /// Human title.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Aggregate safe status line.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }

    /// Ordered current rows.
    #[must_use]
    pub fn rows(&self) -> &[CapabilityPanelRow] {
        &self.rows
    }

    /// Highlighted row.
    #[must_use]
    pub const fn selected(&self) -> usize {
        self.selected
    }

    pub(crate) fn preserve_selection(&mut self, other: &Self) {
        self.selected = other
            .rows
            .get(other.selected)
            .and_then(|old| self.rows.iter().position(|row| row.name == old.name))
            .unwrap_or(0);
    }

    /// Move the highlight, wrapping through current rows.
    pub fn move_selection(&mut self, delta: isize) {
        let len = self.rows.len();
        if len == 0 {
            self.selected = 0;
            return;
        }
        self.selected = if delta < 0 {
            self.selected
                .checked_sub(delta.unsigned_abs())
                .unwrap_or(len - 1)
        } else {
            (self.selected + delta as usize) % len
        };
    }
}

/// Snapshot the current discovered-skill service for its owning panel.
#[must_use]
pub fn skills_catalog(skills: &heycode_skills::SkillSet) -> CapabilityCatalogView {
    let mut rows = skills
        .snapshot_records()
        .unwrap_or_default()
        .into_iter()
        .map(|record| {
            let access = if record.skill.disable_model_invocation {
                "user-only"
            } else {
                "model + user"
            };
            let location = if record.source.directory().is_empty() {
                record.source.root().to_owned()
            } else {
                format!("{}/{}", record.source.root(), record.source.directory())
            };
            CapabilityPanelRow::new(
                record.skill.name,
                format!(
                    "{access} — {} — {}: {location}",
                    record.skill.description,
                    record.source.scope().as_str()
                ),
            )
        })
        .collect::<Vec<_>>();
    let discovered = rows.len();
    // A skipped directory is shown where the user looks for their skill, with
    // the reason, instead of silently missing from the list.
    let skipped = skills.skipped();
    for skipped in &skipped {
        rows.push(CapabilityPanelRow::new(
            format!("{}/{}", skipped.root, skipped.directory),
            format!("skipped — {}", skipped.reason),
        ));
    }
    let reload_diagnostics = skills.reload_diagnostics();
    rows.extend(
        reload_diagnostics
            .iter()
            .enumerate()
            .map(|(index, diagnostic)| {
                CapabilityPanelRow::new(
                    format!("reload-failed-{}", index + 1),
                    format!("blocked — {diagnostic}"),
                )
            }),
    );
    let mut summary = format!("{discovered} discovered");
    if !skipped.is_empty() {
        summary.push_str(&format!(", {} skipped", skipped.len()));
    }
    if !reload_diagnostics.is_empty() {
        summary.push_str(&format!(", {} reload failed", reload_diagnostics.len()));
    }
    CapabilityCatalogView::new(CapabilityPanel::Skills, "Skills", summary, rows)
}

/// Snapshot registered subagent providers without crossing child ownership.
#[must_use]
pub fn agents_catalog(registry: &heycode_agent::SubagentRegistry) -> CapabilityCatalogView {
    agents_catalog_with_readiness(registry, &std::collections::BTreeMap::new())
}

pub(crate) fn agents_catalog_with_readiness(
    registry: &heycode_agent::SubagentRegistry,
    readiness: &std::collections::BTreeMap<String, &'static str>,
) -> CapabilityCatalogView {
    let mut rows = registry
        .descriptors()
        .into_iter()
        .map(|descriptor| {
            let capabilities = descriptor.capabilities();
            CapabilityPanelRow::new(
                descriptor.id().as_str(),
                format!(
                    "{} — {} — fork={} continuation={} interrupt={}",
                    readiness
                        .get(descriptor.id().as_str())
                        .copied()
                        .unwrap_or("Unknown (not probed)"),
                    descriptor.display(),
                    support_word(capabilities.fork),
                    support_word(capabilities.continuation),
                    support_word(capabilities.interrupt),
                ),
            )
        })
        .collect::<Vec<_>>();
    let presets = registry.presets();
    rows.extend(presets.iter().map(|preset| {
        CapabilityPanelRow::new(
            preset.id().as_str(),
            format!(
                "preset — {} — provider={} seed={:?} continuation={:?}",
                preset.display(),
                preset.provider().map_or("automatic", |id| id.as_str()),
                preset.seed(),
                preset.continuation(),
            ),
        )
    }));
    CapabilityCatalogView::new(
        CapabilityPanel::Agents,
        "Agents",
        format!(
            "{} providers; {} presets; {} live continuable children",
            registry.descriptors().len(),
            presets.len(),
            registry.live_child_count()
        ),
        rows,
    )
}

/// Snapshot effect-owned hooks without rendering command/prompt/argument data.
#[must_use]
pub fn hooks_catalog(service: &heycode_hooks::HookService) -> CapabilityCatalogView {
    let mut rows = Vec::new();
    for event in heycode_hooks::HookEvent::ALL {
        for phase in [
            heycode_hooks::HookPhase::Pre,
            heycode_hooks::HookPhase::Post,
        ] {
            rows.extend(service.matching(phase, event).into_iter().map(|hook| {
                CapabilityPanelRow::new(
                    &hook.owner,
                    format!(
                        "{} {} via {} ({})",
                        phase.as_str(),
                        event.as_str(),
                        hook.action.kind().as_str(),
                        if hook.project_scoped {
                            "project-scoped"
                        } else {
                            "user-scoped"
                        }
                    ),
                )
            }));
        }
    }
    let mut handler_words = vec!["command"];
    handler_words.extend(
        service
            .handler_kinds()
            .into_iter()
            .map(heycode_hooks::HookHandlerKind::as_str),
    );
    handler_words.sort_unstable();
    handler_words.dedup();
    let handlers = handler_words.join(", ");
    CapabilityCatalogView::new(
        CapabilityPanel::Hooks,
        "Hooks",
        format!("{} registered; handlers: {}", rows.len(), handlers),
        rows,
    )
}

fn support_word(support: heycode_llm::CapabilitySupport) -> &'static str {
    match support {
        heycode_llm::CapabilitySupport::Supported => "supported",
        heycode_llm::CapabilitySupport::Unsupported => "unsupported",
        heycode_llm::CapabilitySupport::Unknown => "unknown",
    }
}

fn safe_text(value: &str, max_chars: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max_chars)
        .collect()
}

/// UI registry descriptor for a read-only skills/agents/hooks catalog.
///
/// # Errors
/// Descriptor validation fails for an invalid static panel contract.
pub fn catalog_panel_descriptor(
    panel: CapabilityPanel,
) -> Result<heycode_ui::UiContributionDescriptor, heycode_ui::UiRegistryError> {
    let title = match panel {
        CapabilityPanel::Skills => "Discovered skills",
        CapabilityPanel::Agents => "Delegated agents",
        CapabilityPanel::Workflows => "Workflow phases and agents",
        CapabilityPanel::Hooks => "Lifecycle hooks",
        CapabilityPanel::Mcp | CapabilityPanel::Plugins | CapabilityPanel::Settings => {
            return Err(heycode_ui::UiRegistryError::InvalidId);
        }
    };
    heycode_ui::UiContributionDescriptor::new(heycode_ui::UiSlot::Panel, panel.as_str(), title, 80)
}

#[derive(Debug, Default)]
struct BridgeState {
    pending: Option<CapabilityPanel>,
    attached: BTreeSet<CapabilityPanel>,
    mcp_management: Option<Arc<heycode_mcp::management::McpManagement>>,
    mcp_runtime_control: Option<Arc<heycode_mcp::McpRuntimeControl>>,
}

/// The shell's panel-open inbox, shared with the panel slash commands.
///
/// Cloning shares one inbox; a default-constructed bridge is a shell that has
/// attached nothing, which is exactly what a world without the owning
/// services should report.
#[derive(Clone, Default)]
pub struct PanelCommandBridge {
    state: Arc<Mutex<BridgeState>>,
}

impl PanelCommandBridge {
    /// An inbox with nothing attached and nothing requested.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record that the shell wired this panel's service layer.
    ///
    /// MCP additionally requires its concrete management owner through
    /// [`Self::attach_mcp`], so a bare MCP marker cannot claim availability.
    pub fn attach(&self, panel: CapabilityPanel) {
        self.lock().attached.insert(panel);
    }

    /// Attach the exact MCP management owner used by the running shell.
    ///
    /// Direct `/mcp enable` and `/mcp disable` requests must reach the same
    /// store-backed owner as the panel. Holding an [`Arc`] here shares that
    /// owner; it does not construct a second management surface.
    pub fn attach_mcp(&self, management: Arc<heycode_mcp::management::McpManagement>) {
        let mut state = self.lock();
        state.mcp_management = Some(management);
        state.attached.insert(CapabilityPanel::Mcp);
    }

    /// Attach the product context's exact live MCP control owner.
    ///
    /// This is separate from management because the panel remains useful in a
    /// management-only shell. `/mcp reconnect`, however, must reach the same
    /// effect-owned supervisors as the running MCP plugin.
    pub fn attach_mcp_runtime_control(&self, control: Arc<heycode_mcp::McpRuntimeControl>) {
        self.lock().mcp_runtime_control = Some(control);
    }

    /// Whether the shell wired this panel's service layer.
    #[must_use]
    pub fn is_attached(&self, panel: CapabilityPanel) -> bool {
        let state = self.lock();
        state.attached.contains(&panel)
            && (panel != CapabilityPanel::Mcp || state.mcp_management.is_some())
    }

    fn mcp_management(&self) -> Option<Arc<heycode_mcp::management::McpManagement>> {
        self.lock().mcp_management.clone()
    }

    fn mcp_runtime_control(&self) -> Option<Arc<heycode_mcp::McpRuntimeControl>> {
        self.lock().mcp_runtime_control.clone()
    }

    /// Ask the shell to open `panel` on its next wake.
    ///
    /// One request is pending at a time; a second replaces the first, which is
    /// what a human pressing two panel commands in a row means. The shell
    /// dispatches at most one command at a time, so this is not a queue.
    pub fn request(&self, panel: CapabilityPanel) {
        self.lock().pending = Some(panel);
    }

    /// Take the pending request, leaving the inbox empty.
    #[must_use]
    pub fn take(&self) -> Option<CapabilityPanel> {
        self.lock().pending.take()
    }

    /// A poisoned inbox still answers: every mutation here is a single
    /// assignment, so no torn invariant can survive a panic and refusing to
    /// open a panel would be a worse answer than opening it.
    fn lock(&self) -> std::sync::MutexGuard<'_, BridgeState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl std::fmt::Debug for PanelCommandBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.lock();
        formatter
            .debug_struct("PanelCommandBridge")
            .field("pending", &state.pending)
            .field("attached", &state.attached)
            .field("mcp_management_attached", &state.mcp_management.is_some())
            .field(
                "mcp_runtime_control_attached",
                &state.mcp_runtime_control.is_some(),
            )
            .finish()
    }
}

struct PanelCommand {
    descriptor: CommandDescriptor,
    panel: CapabilityPanel,
    detached: CommandAvailability,
    bridge: PanelCommandBridge,
}

/// Claude-compatible `/subtask`: admit a fork-seeded native child as a
/// durable background job whose settled result returns through the parent's
/// follow-up inbox. Provider selection is deliberately explicit: the native
/// child follows the parent route rather than choosing whichever external
/// subagent provider happened to register first.
struct SubtaskCommand {
    descriptor: CommandDescriptor,
    subagents: Option<Arc<SubagentRegistry>>,
    unavailable: CommandAvailability,
}

/// Current-session agent inventory. This is distinct from `/agents`, which
/// describes providers and presets; the human needs both the capability
/// catalog and the actual admitted conversations.
struct ListAgentsCommand {
    descriptor: CommandDescriptor,
    subagents: Option<Arc<SubagentRegistry>>,
    unavailable: CommandAvailability,
}

/// Explicit local control over the existing durable session schedule owner.
/// This does not pretend to be Claude's hosted conversational routines UI.
struct ScheduleCommand {
    descriptor: CommandDescriptor,
    schedules: Option<Arc<heycode_agent::DurableScheduleService>>,
    unavailable: CommandAvailability,
}

#[async_trait]
impl Command for ScheduleCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.schedules.is_some() {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let schedules = self.schedules.as_ref().ok_or_else(|| {
            anyhow::anyhow!("durable schedules are not configured in this profile")
        })?;
        let args = args.trim();
        let text = if args.is_empty() || args == "list" {
            render_schedules(&schedules.list()?)
        } else if let Some(id) = args.strip_prefix("delete ") {
            let id = ScheduleId::new(id.trim())
                .map_err(|_| anyhow::anyhow!("usage: /schedule delete <schedule-id>"))?;
            schedules.delete(&id)?;
            format!("deleted schedule {id}")
        } else {
            let (kind, remainder) = args
                .split_once(char::is_whitespace)
                .ok_or_else(schedule_usage)?;
            let (value, prompt) = remainder
                .trim_start()
                .split_once(char::is_whitespace)
                .ok_or_else(schedule_usage)?;
            let prompt = prompt.trim();
            if prompt.is_empty() {
                return Err(schedule_usage());
            }
            let record = match kind {
                "after" => schedules.create_after(
                    prompt,
                    std::time::Duration::from_secs(positive_schedule_u64(value)?),
                )?,
                "every" => schedules.create_every(
                    prompt,
                    std::time::Duration::from_secs(positive_schedule_u64(value)?),
                )?,
                "at" => schedules.create_at(
                    prompt,
                    value
                        .parse::<i64>()
                        .ok()
                        .filter(|value| *value > 0)
                        .ok_or_else(schedule_usage)?,
                )?,
                _ => return Err(schedule_usage()),
            };
            format!("created {}", render_schedule(&record))
        };
        agent.ui().emit(heycode_agent::UiEvent::Info { text });
        Ok(())
    }
}

fn positive_schedule_u64(value: &str) -> anyhow::Result<u64> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(schedule_usage)
}

fn schedule_usage() -> anyhow::Error {
    anyhow::anyhow!(
        "usage: /schedule [list|after <seconds> <prompt>|at <unix-ms> <prompt>|every <seconds> <prompt>|delete <schedule-id>]"
    )
}

fn render_schedules(records: &[ScheduleRecord]) -> String {
    if records.is_empty() {
        return "No active local schedules.".to_owned();
    }
    let mut lines = vec![format!("Local schedules: {}", records.len())];
    lines.extend(records.iter().map(render_schedule));
    lines.join("\n")
}

fn render_schedule(record: &ScheduleRecord) -> String {
    let rule = match record.rule() {
        ScheduleRule::After { delay_ms } => format!("after {delay_ms}ms"),
        ScheduleRule::At => "at".to_owned(),
        ScheduleRule::Every { every_ms } => format!("every {every_ms}ms"),
        ScheduleRule::Cron {
            expression,
            recurring,
            timezone: heycode_session::ScheduleTimeZone::Local,
        } => format!(
            "cron {expression} ({}; local time)",
            if *recurring { "recurring" } else { "one-shot" }
        ),
        ScheduleRule::Wakeup { delay_ms } => {
            format!("wakeup after {delay_ms}ms (self-paced)")
        }
    };
    format!(
        "{} · {} · next={} · {}",
        record.id(),
        rule,
        record.scheduled_at_ms(),
        record.prompt()
    )
}

#[async_trait]
impl Command for ListAgentsCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.subagents.is_some() {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        if !args.trim().is_empty() {
            anyhow::bail!("usage: /list-agents");
        }
        let registry = self
            .subagents
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("the current profile has no subagent registry"))?;
        let owner = {
            let session = agent
                .session()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            SubagentId::new(session.id().as_str())?
        };
        let rows = registry.task_snapshots_for(&registry.root_authority(owner.clone()));
        let mut lines = vec![
            format!("this session: {owner}"),
            format!("agents: {}", rows.len()),
        ];
        if rows.is_empty() {
            lines.push("No admitted agent conversations for this session.".to_owned());
        } else {
            lines.extend(rows.into_iter().map(|row| {
                let state = format!("{:?}", row.state).to_ascii_lowercase();
                let session = row.session_id.as_deref().unwrap_or("not-started");
                let job = row.job_id.as_deref().unwrap_or("foreground");
                format!(
                    "{} · {} · {} · provider={} · session={} · job={}",
                    row.id, row.label, state, row.provider, session, job
                )
            }));
        }
        agent.ui().emit(heycode_agent::UiEvent::Info {
            text: lines.join("\n"),
        });
        Ok(())
    }
}

#[async_trait]
impl Command for SubtaskCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.subagents.as_ref().is_some_and(|registry| {
            registry.background_available()
                && registry.descriptors().iter().any(|provider| {
                    provider.id().as_str() == "native"
                        && provider.capabilities().fork == CapabilitySupport::Supported
                })
        }) {
            CommandAvailability::available()
        } else {
            self.unavailable.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let task = required_subtask(args)?;
        let registry = self.subagents.as_ref().ok_or_else(|| {
            anyhow::anyhow!("native background subagents are not configured in this profile")
        })?;
        if !registry.background_available() {
            anyhow::bail!("native background subagent jobs are not attached to this session");
        }
        if !registry.descriptors().iter().any(|provider| {
            provider.id().as_str() == "native"
                && provider.capabilities().fork == CapabilitySupport::Supported
        }) {
            anyhow::bail!("a fork-capable native subagent provider is unavailable");
        }
        let owner = {
            let session = agent
                .session()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            SubagentId::new(session.id().as_str())?
        };
        let request = SubagentRequest::with_authority(
            subtask_label(&task),
            task,
            SubagentSeed::ForkParent,
            SubagentContinuation::OneShot,
            registry.root_authority(owner),
        )?
        .with_provider(SubagentProviderId::new("native")?);
        let (task_id, job_id) =
            registry.start_background_task(request, heycode_session::InboxDelivery::FollowUp)?;
        agent.ui().emit(heycode_agent::UiEvent::Info {
            text: format!(
                "background subtask admitted [task_id: {task_id}] [job_id: {job_id}]; result will return to this conversation"
            ),
        });
        Ok(())
    }
}

fn required_subtask(args: &str) -> anyhow::Result<String> {
    let task = args.trim();
    if task.is_empty() {
        anyhow::bail!("usage: /subtask <task>");
    }
    if task.chars().count() > MAX_SUBTASK_CHARS
        || task
            .chars()
            .any(|character| character.is_control() && !matches!(character, '\n' | '\t'))
    {
        anyhow::bail!("subtask text is invalid or too long");
    }
    Ok(task.to_owned())
}

fn subtask_label(task: &str) -> String {
    let words = task
        .split_whitespace()
        .take(5)
        .collect::<Vec<_>>()
        .join(" ");
    let mut label = String::new();
    for character in words.chars() {
        if label.len() + character.len_utf8() > 96 {
            break;
        }
        label.push(character);
    }
    if label.is_empty() {
        "subtask".to_owned()
    } else {
        label
    }
}

#[async_trait]
impl Command for PanelCommand {
    fn descriptor(&self) -> &CommandDescriptor {
        &self.descriptor
    }

    fn availability(&self) -> CommandAvailability {
        if self.bridge.is_attached(self.panel) {
            CommandAvailability::available()
        } else {
            self.detached.clone()
        }
    }

    async fn execute(&self, agent: &heycode_agent::Agent, args: &str) -> anyhow::Result<()> {
        let args = args.trim();
        if args.is_empty() {
            self.bridge.request(self.panel);
            return Ok(());
        }
        if self.panel != CapabilityPanel::Mcp {
            anyhow::bail!("usage: /{}", self.panel.as_str());
        }

        let mut words = args.split_whitespace();
        let action = words.next().ok_or_else(mcp_usage)?;
        let name = words.next().ok_or_else(mcp_usage)?;
        if words.next().is_some() || !valid_mcp_server_name(name) {
            return Err(mcp_usage());
        }
        let management = self
            .bridge
            .mcp_management()
            .ok_or_else(|| anyhow::anyhow!("MCP management is not attached to this shell"))?;

        match action {
            "enable" | "disable" => {
                let on = action == "enable";
                crate::mcp_panel::dispatch(
                    &management,
                    &crate::mcp_panel::McpPanelIntent::Enable {
                        name: name.to_owned(),
                        on,
                    },
                )
                .map_err(anyhow::Error::msg)?;
                agent.ui().emit(heycode_agent::UiEvent::Info {
                    text: format!(
                        "{action}d stored MCP definition `{name}`; current live connections are unchanged until MCP connections are recomposed"
                    ),
                });
                Ok(())
            }
            "reconnect" => {
                let server = management
                    .list()?
                    .into_iter()
                    .find(|row| row.server.name == name)
                    .map(|row| row.server)
                    .ok_or_else(|| anyhow::anyhow!("no MCP server named `{name}`"))?;
                if !server.enabled {
                    anyhow::bail!("MCP server `{name}` is disabled");
                }
                let control = self.bridge.mcp_runtime_control().ok_or_else(|| {
                    anyhow::anyhow!("MCP runtime control is not attached to this shell")
                })?;
                let admission = control.reconnect(name).map_err(anyhow::Error::new)?;
                let text = match admission {
                    heycode_mcp::McpRecoveryAdmission::Started => format!(
                        "started bounded reconnect for MCP server `{name}`; readiness will appear in /mcp after a complete generation is published"
                    ),
                    heycode_mcp::McpRecoveryAdmission::AlreadyRunning => {
                        format!("MCP server `{name}` already has a bounded reconnect in progress")
                    }
                    heycode_mcp::McpRecoveryAdmission::Disabled => {
                        anyhow::bail!(
                            "MCP server `{name}` has reconnect disabled by its active definition"
                        );
                    }
                    heycode_mcp::McpRecoveryAdmission::Exhausted => {
                        anyhow::bail!("MCP server `{name}` has exhausted its reconnect budget");
                    }
                    heycode_mcp::McpRecoveryAdmission::ShutDown => {
                        anyhow::bail!("MCP server `{name}` is shutting down");
                    }
                    heycode_mcp::McpRecoveryAdmission::Unavailable => {
                        anyhow::bail!("MCP server `{name}` cannot start reconnect in this runtime");
                    }
                };
                agent.ui().emit(heycode_agent::UiEvent::Info { text });
                Ok(())
            }
            _ => Err(mcp_usage()),
        }
    }
}

fn valid_mcp_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
}

fn mcp_usage() -> anyhow::Error {
    anyhow::anyhow!("usage: /mcp [enable|disable|reconnect] <server>")
}

/// One panel command bound to the shell's inbox.
///
/// The unavailable projection is validated here rather than at call time, so a
/// malformed reason fails composition instead of degrading into a command that
/// silently claims to be available.
///
/// # Errors
/// Invalid descriptor or availability metadata fails loud.
pub fn panel_command(
    panel: CapabilityPanel,
    source: CommandSource,
    bridge: PanelCommandBridge,
) -> Result<Arc<dyn Command>, CommandMetadataError> {
    let arguments = if panel == CapabilityPanel::Mcp {
        vec![
            CommandArgument::optional("action", "enable, disable, or reconnect")?,
            CommandArgument::optional("server", "MCP server name")?,
        ]
    } else {
        Vec::new()
    };
    Ok(Arc::new(PanelCommand {
        descriptor: CommandDescriptor::new(
            panel.as_str(),
            panel.description(),
            arguments,
            CommandTiming::Immediate,
            source,
        )?,
        panel,
        detached: CommandAvailability::unavailable(panel.detached_reason())?,
        bridge,
    }))
}

/// Register the capability-panel slash commands against the live shell.
///
/// Injects service `"tui"` rather than building its own inbox: the bridge the
/// commands write to must be the one the running shell drains, and taking it
/// from the published [`crate::TuiHandle`] makes a misordered composition fail
/// loud instead of dropping every request into an inbox nobody reads.
#[must_use]
pub fn panel_commands_plugin() -> Box<dyn Plugin> {
    struct PanelCommandsPlugin;

    impl Plugin for PanelCommandsPlugin {
        fn name(&self) -> &'static str {
            "panel-commands"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                "panel-commands",
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Command],
            )
        }

        fn inventory(&self) -> Vec<heycode_core::PluginContributionSpec> {
            COMMANDED_PANELS
                .into_iter()
                .map(|panel| {
                    heycode_core::PluginContributionSpec::new(
                        heycode_core::ContributionKind::Command,
                        panel.as_str(),
                    )
                })
                .chain(
                    ["list-agents", "subtask", "schedule"]
                        .into_iter()
                        .map(|name| {
                            heycode_core::PluginContributionSpec::new(
                                heycode_core::ContributionKind::Command,
                                name,
                            )
                        }),
                )
                .collect()
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[heycode_agent::SERVICE_COMMANDS, crate::SERVICE_TUI]
        }

        fn apply(&self, context: &mut Context) -> CoreResult<()> {
            let commands = context
                .get::<CommandRegistry>(heycode_agent::SERVICE_COMMANDS)
                .ok_or_else(|| CoreError::other("command registry missing"))?;
            let bridge = context
                .get::<crate::TuiHandle>(crate::SERVICE_TUI)
                .ok_or_else(|| CoreError::other("tui handle missing"))?
                .panels();
            let source = CommandSource::from_plugin(self.name())
                .map_err(|error| CoreError::other(error.to_string()))?;
            for panel in COMMANDED_PANELS {
                let command = panel_command(panel, source.clone(), bridge.clone())
                    .map_err(|error| CoreError::other(error.to_string()))?;
                commands
                    .register_effect(context, command)
                    .map_err(|error| CoreError::other(error.to_string()))?;
            }
            let subagents = context.get::<SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS);
            commands
                .register_effect(
                    context,
                    Arc::new(ListAgentsCommand {
                        descriptor: CommandDescriptor::new(
                            "list-agents",
                            "List this session and its admitted agent conversations",
                            Vec::new(),
                            CommandTiming::Immediate,
                            source.clone(),
                        )
                        .map_err(|error| CoreError::other(error.to_string()))?,
                        subagents: subagents.clone(),
                        unavailable: CommandAvailability::unavailable(
                            "Subagent registry is not attached to this profile",
                        )
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    context,
                    Arc::new(SubtaskCommand {
                        descriptor: CommandDescriptor::new(
                            "subtask",
                            "Run a forked native subagent in the background and return its result",
                            vec![
                                CommandArgument::required(
                                    "task",
                                    "Self-contained task for the forked subagent",
                                )
                                .map_err(|error| CoreError::other(error.to_string()))?
                                .variadic(),
                            ],
                            CommandTiming::ModelScheduling,
                            source.clone(),
                        )
                        .map_err(|error| CoreError::other(error.to_string()))?,
                        subagents,
                        unavailable: CommandAvailability::unavailable(
                            "Fork-capable native background subagents are not attached",
                        )
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            commands
                .register_effect(
                    context,
                    Arc::new(ScheduleCommand {
                        descriptor: CommandDescriptor::new(
                            "schedule",
                            "Manage durable local session schedules without a hosted routine",
                            vec![
                                CommandArgument::optional(
                                    "action",
                                    "list, after, at, every, or delete",
                                )
                                .map_err(|error| CoreError::other(error.to_string()))?,
                                CommandArgument::optional(
                                    "arguments",
                                    "Action-specific value and reminder text",
                                )
                                .map_err(|error| CoreError::other(error.to_string()))?
                                .variadic(),
                            ],
                            CommandTiming::ModelScheduling,
                            source,
                        )
                        .map_err(|error| CoreError::other(error.to_string()))?,
                        schedules: context.get::<heycode_agent::DurableScheduleService>(
                            heycode_agent::SERVICE_SCHEDULES,
                        ),
                        unavailable: CommandAvailability::unavailable(
                            "Durable schedule service is not attached to this profile",
                        )
                        .map_err(|error| CoreError::other(error.to_string()))?,
                    }),
                )
                .map_err(|error| CoreError::other(error.to_string()))?;
            Ok(())
        }
    }

    Box::new(PanelCommandsPlugin)
}
