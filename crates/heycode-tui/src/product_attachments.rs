//! Product adapters joining MCP, Agent, HookService, Session and the TUI.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

const MCP_UI_EVENT_CAPACITY: usize = 256;
const MAX_MCP_UI_ELICITATIONS: usize = 16;

/// One session's reusable MCP11/MCP13/O09 product adapters.
pub struct McpProductSession {
    route: heycode_mcp::McpClientRoute,
    routed_servers: Mutex<BTreeSet<heycode_mcp::McpServerId>>,
    ui: Arc<McpTuiBridge>,
    approval: Arc<AgentMcpToolApproval>,
    hooks: Arc<ProductHookAdapter>,
}

impl McpProductSession {
    /// Mint one fresh session id and bind all product callbacks to it.
    ///
    /// The caller passes the same id to
    /// [`heycode_session::session_with_id_and_metadata_plugin`].
    ///
    /// # Errors
    /// The generated id unexpectedly fails the MCP route validator.
    pub fn fresh(
        approval: Arc<dyn heycode_agent::ApprovalPolicy>,
    ) -> Result<(heycode_core::SessionId, Self), McpProductAttachmentError> {
        let id = heycode_core::SessionId::generate();
        let product = Self::new(&id, approval)?;
        Ok((id, product))
    }

    /// Bind product callbacks to one exact durable session and approval policy.
    ///
    /// # Errors
    /// The session id cannot form a bounded opaque MCP route.
    pub fn new(
        session_id: &heycode_core::SessionId,
        approval: Arc<dyn heycode_agent::ApprovalPolicy>,
    ) -> Result<Self, McpProductAttachmentError> {
        let route = heycode_mcp::McpClientRoute::new(session_id.as_str())
            .map_err(|_| McpProductAttachmentError::InvalidSessionRoute)?;
        let ui = Arc::new(McpTuiBridge::new(route.clone()));
        Ok(Self {
            route,
            routed_servers: Mutex::new(BTreeSet::new()),
            ui,
            approval: Arc::new(AgentMcpToolApproval { policy: approval }),
            hooks: Arc::new(ProductHookAdapter::default()),
        })
    }

    /// Mint the sole router for one configured server/session connection.
    ///
    /// # Errors
    /// This product session already minted a router for the server or its
    /// registry state is unavailable.
    pub fn router(
        &self,
        server: heycode_mcp::McpServerId,
        capabilities: heycode_mcp::McpElicitationCapabilities,
    ) -> Result<heycode_mcp::McpClientEventRouter, McpProductAttachmentError> {
        let mut routed = self
            .routed_servers
            .lock()
            .map_err(|_| McpProductAttachmentError::Unavailable)?;
        if !routed.insert(server.clone()) {
            return Err(McpProductAttachmentError::DuplicateServerRoute);
        }
        let handler: Arc<dyn heycode_mcp::McpElicitationHandler> = self.ui.clone();
        let sink: Arc<dyn heycode_mcp::McpClientEventSink> = self.ui.clone();
        Ok(heycode_mcp::McpClientEventRouter::new(
            server,
            self.route.clone(),
            capabilities,
            handler,
            sink,
        ))
    }

    /// Shared ordinary Agent approval adapter for MCP13.
    #[must_use]
    pub fn approval_handler(&self) -> Arc<dyn heycode_mcp::McpToolApprovalHandler> {
        self.approval.clone()
    }

    /// O09 lifecycle adapter passed to MCP product connection builders.
    #[must_use]
    pub fn mcp_lifecycle_hooks(&self) -> Arc<dyn heycode_mcp::McpLifecycleHookPort> {
        self.hooks.clone()
    }

    /// O09 lifecycle adapter attached to Agent and SubagentRegistry.
    #[must_use]
    pub fn agent_lifecycle_hooks(&self) -> Arc<dyn heycode_agent::LifecycleHookPort> {
        self.hooks.clone()
    }

    /// Shared human-event and elicitation bridge captured by the TUI plugin.
    #[must_use]
    pub fn tui_bridge(&self) -> Arc<McpTuiBridge> {
        self.ui.clone()
    }

    /// Effect-attachable durable HookService adapter.
    #[must_use]
    pub fn hook_adapter(&self) -> Arc<ProductHookAdapter> {
        self.hooks.clone()
    }
}

/// Safe product-attachment construction failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum McpProductAttachmentError {
    /// Session id is not a valid opaque MCP route.
    #[error("MCP product session route is invalid")]
    InvalidSessionRoute,
    /// One connection router was requested more than once.
    #[error("MCP server already has a router for this session")]
    DuplicateServerRoute,
    /// Attachment registry state is unavailable.
    #[error("MCP product attachment state is unavailable")]
    Unavailable,
}

/// MCP human event delivered to the active TUI loop.
#[derive(Clone)]
pub enum McpTuiEvent {
    /// One correlated elicitation awaiting a typed response.
    Elicitation {
        /// TUI-local correlation id.
        id: u64,
        /// Strict MCP request.
        request: heycode_mcp::McpElicitationRequest,
    },
    /// Bounded routed progress/log/completion event.
    Human(heycode_mcp::McpClientEvent),
}

impl std::fmt::Debug for McpTuiEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Elicitation { id, request } => formatter
                .debug_struct("McpTuiElicitation")
                .field("id", id)
                .field("request", request)
                .finish(),
            Self::Human(event) => event.fmt(formatter),
        }
    }
}

type ElicitationAnswer =
    Result<heycode_mcp::McpElicitationResponse, heycode_mcp::McpElicitationFailure>;

/// Session-scoped TUI broker and human event sink.
pub struct McpTuiBridge {
    route: heycode_mcp::McpClientRoute,
    sender: broadcast::Sender<McpTuiEvent>,
    active: AtomicBool,
    counter: AtomicU64,
    waiters: Mutex<HashMap<u64, oneshot::Sender<ElicitationAnswer>>>,
}

impl McpTuiBridge {
    fn new(route: heycode_mcp::McpClientRoute) -> Self {
        let (sender, _) = broadcast::channel(MCP_UI_EVENT_CAPACITY);
        Self {
            route,
            sender,
            active: AtomicBool::new(false),
            counter: AtomicU64::new(0),
            waiters: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) fn activate(
        self: &Arc<Self>,
    ) -> Result<(broadcast::Receiver<McpTuiEvent>, McpTuiActivation), McpProductAttachmentError>
    {
        if self
            .active
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(McpProductAttachmentError::Unavailable);
        }
        Ok((
            self.sender.subscribe(),
            McpTuiActivation {
                bridge: Arc::downgrade(self),
                active: true,
            },
        ))
    }

    pub(crate) fn answer(&self, id: u64, answer: ElicitationAnswer) -> bool {
        let sender = self
            .waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(&id));
        sender.is_some_and(|sender| sender.send(answer).is_ok())
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
        let waiters = self
            .waiters
            .lock()
            .map(|mut waiters| {
                waiters
                    .drain()
                    .map(|(_, sender)| sender)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        for waiter in waiters {
            let _ = waiter.send(Err(heycode_mcp::McpElicitationFailure::Cancelled));
        }
    }
}

impl std::fmt::Debug for McpTuiBridge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpTuiBridge")
            .field("route", &self.route)
            .field("active", &self.active.load(Ordering::SeqCst))
            .field(
                "pending",
                &self.waiters.lock().map(|rows| rows.len()).unwrap_or(0),
            )
            .finish()
    }
}

#[async_trait]
impl heycode_mcp::McpElicitationHandler for McpTuiBridge {
    async fn elicit(
        &self,
        request: heycode_mcp::McpElicitationRequest,
        cancellation: CancellationToken,
    ) -> Result<heycode_mcp::McpElicitationResponse, heycode_mcp::McpElicitationFailure> {
        if !self.active.load(Ordering::SeqCst) || request.route() != &self.route {
            return Err(heycode_mcp::McpElicitationFailure::Unavailable);
        }
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        let inserted = self.waiters.lock().is_ok_and(|mut waiters| {
            if waiters.len() >= MAX_MCP_UI_ELICITATIONS {
                return false;
            }
            waiters.insert(id, sender).is_none()
        });
        if !inserted {
            return Err(heycode_mcp::McpElicitationFailure::Unavailable);
        }
        if self
            .sender
            .send(McpTuiEvent::Elicitation { id, request })
            .is_err()
        {
            if let Ok(mut waiters) = self.waiters.lock() {
                waiters.remove(&id);
            }
            return Err(heycode_mcp::McpElicitationFailure::Unavailable);
        }
        let answer = tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(heycode_mcp::McpElicitationFailure::Cancelled),
            answer = receiver => answer.unwrap_or(Err(heycode_mcp::McpElicitationFailure::Cancelled)),
        };
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.remove(&id);
        }
        answer
    }
}

impl heycode_mcp::McpClientEventSink for McpTuiBridge {
    fn publish(&self, event: heycode_mcp::McpClientEvent) {
        if self.active.load(Ordering::SeqCst) && event.route() == &self.route {
            let _ = self.sender.send(McpTuiEvent::Human(event));
        }
    }
}

pub(crate) struct McpTuiActivation {
    bridge: Weak<McpTuiBridge>,
    active: bool,
}

impl Drop for McpTuiActivation {
    fn drop(&mut self) {
        if self.active {
            self.active = false;
            if let Some(bridge) = self.bridge.upgrade() {
                bridge.deactivate();
            }
        }
    }
}

/// The MCP call-time approval handler for the product shell.
///
/// Every `mcp__<server>__<tool>` call already went through the Agent's
/// `admit_call`, which is where the shared approval policy decides — with a
/// dialog under `ask`, silently under `auto`, refusing under `deny`. Asking the
/// same policy again here produced two, three or four identical cards for one
/// call, and answering the stale duplicate surfaced `app-server is unavailable`.
/// A server's `Prompt` admission therefore means "the ordinary approval applies
/// to this tool", never "ask twice": `Deny` rows were filtered at publication
/// and never reach this handler.
struct AgentMcpToolApproval {
    policy: Arc<dyn heycode_agent::ApprovalPolicy>,
}

#[async_trait]
impl heycode_mcp::McpToolApprovalHandler for AgentMcpToolApproval {
    async fn decide(
        &self,
        _request: heycode_mcp::McpToolApprovalRequest,
        _cancellation: CancellationToken,
    ) -> heycode_mcp::McpToolApprovalDecision {
        // `deny` mode never lets the call reach a tool; anything else was
        // decided once already by `admit_call`.
        if self.policy.kind() == heycode_agent::ApprovalPolicyKind::Deny {
            heycode_mcp::McpToolApprovalDecision::Deny
        } else {
            heycode_mcp::McpToolApprovalDecision::Allow
        }
    }
}

type HookBinding = (
    Arc<heycode_hooks::HookService>,
    Arc<Mutex<heycode_session::Session>>,
    Arc<()>,
);

/// HookService adapter shared by Agent, SubagentRegistry and MCP tools.
#[derive(Default)]
pub struct ProductHookAdapter {
    binding: Arc<Mutex<Option<HookBinding>>>,
}

impl ProductHookAdapter {
    fn attach(
        &self,
        context: &heycode_core::Context,
        hooks: Arc<heycode_hooks::HookService>,
        session: Arc<Mutex<heycode_session::Session>>,
    ) -> Result<(), heycode_core::CoreError> {
        let token = Arc::new(());
        let mut binding = self
            .binding
            .lock()
            .map_err(|_| heycode_core::CoreError::other("product hook adapter unavailable"))?;
        if binding.is_some() {
            return Err(heycode_core::CoreError::other(
                "product hook adapter is already attached",
            ));
        }
        *binding = Some((hooks, session, token.clone()));
        drop(binding);
        let slot = Arc::downgrade(&self.binding);
        context.effect(move || {
            let Some(slot) = slot.upgrade() else {
                return;
            };
            let Ok(mut binding) = slot.lock() else {
                return;
            };
            if binding
                .as_ref()
                .is_some_and(|(_, _, current)| Arc::ptr_eq(current, &token))
            {
                *binding = None;
            }
        });
        Ok(())
    }

    async fn run_hooks(
        &self,
        phase: heycode_hooks::HookPhase,
        event: heycode_hooks::HookEvent,
        payload: heycode_hooks::HookPayload,
        cancellation: CancellationToken,
    ) -> (bool, u32) {
        let binding = self.binding.lock().ok().and_then(|binding| {
            binding
                .as_ref()
                .map(|(hooks, session, _)| (hooks.clone(), session.clone()))
        });
        let Some((hooks, session)) = binding else {
            return (true, 0);
        };
        let outcomes = hooks
            .run_with(phase, event, &payload, cancellation.clone())
            .await;
        let refused = outcomes
            .iter()
            .any(|outcome| matches!(outcome, heycode_hooks::HookOutcome::Refused { .. }));
        let mut faults = outcomes
            .iter()
            .filter(|outcome| outcome.fault().is_some())
            .count() as u32;
        if refused {
            return (false, faults);
        }
        let bridge = SessionHookDurableBridge { session };
        for outcome in outcomes {
            if let Some(contribution) = outcome.into_contribution()
                && contribution
                    .commit(&bridge, cancellation.child_token())
                    .await
                    .is_err()
            {
                faults = faults.saturating_add(1);
            }
        }
        (true, faults)
    }
}

#[async_trait]
impl heycode_agent::LifecycleHookPort for ProductHookAdapter {
    async fn run(
        &self,
        request: heycode_agent::LifecycleHookRequest,
        cancellation: CancellationToken,
    ) -> heycode_agent::LifecycleHookReport {
        let phase = match request.phase() {
            heycode_agent::LifecycleHookPhase::Pre => heycode_hooks::HookPhase::Pre,
            heycode_agent::LifecycleHookPhase::Post => heycode_hooks::HookPhase::Post,
        };
        let event = match request.event() {
            heycode_agent::LifecycleHookEvent::UserPrompt => heycode_hooks::HookEvent::UserPrompt,
            heycode_agent::LifecycleHookEvent::Subagent => heycode_hooks::HookEvent::Subagent,
        };
        let (proceeds, faults) = self
            .run_hooks(
                phase,
                event,
                heycode_hooks::HookPayload::trusted(request.payload()),
                cancellation,
            )
            .await;
        if proceeds {
            heycode_agent::LifecycleHookReport::proceed_with_faults(faults)
        } else {
            heycode_agent::LifecycleHookReport::refuse(faults)
        }
    }
}

#[async_trait]
impl heycode_mcp::McpLifecycleHookPort for ProductHookAdapter {
    async fn run(
        &self,
        request: heycode_mcp::McpLifecycleHookRequest,
        cancellation: CancellationToken,
    ) -> heycode_mcp::McpLifecycleHookReport {
        let (phase, payload) = match request.phase() {
            heycode_mcp::McpLifecycleHookPhase::Pre => (
                heycode_hooks::HookPhase::Pre,
                heycode_hooks::HookPayload::trusted(bounded_mcp_hook_payload(&request)),
            ),
            heycode_mcp::McpLifecycleHookPhase::Post => (
                heycode_hooks::HookPhase::Post,
                heycode_hooks::HookPayload::untrusted(
                    request.result_text().unwrap_or_default(),
                    heycode_core::UntrustedContentBoundary::mcp(),
                ),
            ),
        };
        let (proceeds, faults) = self
            .run_hooks(
                phase,
                heycode_hooks::HookEvent::McpServer,
                payload,
                cancellation,
            )
            .await;
        if proceeds {
            heycode_mcp::McpLifecycleHookReport::proceed_with_faults(faults)
        } else {
            heycode_mcp::McpLifecycleHookReport::refuse(faults)
        }
    }
}

fn bounded_mcp_hook_payload(request: &heycode_mcp::McpLifecycleHookRequest) -> String {
    const MAX_BYTES: usize = 64 * 1024;
    let arguments = request
        .arguments()
        .and_then(|value| serde_json::to_string(value).ok())
        .unwrap_or_else(|| "null".to_owned());
    let prefix = format!(
        "MCP tool {} on server {} with arguments ",
        request.tool(),
        request.server().as_str()
    );
    let prefix_len = prefix.len();
    let remaining = MAX_BYTES.saturating_sub(prefix_len);
    let mut output = prefix;
    if arguments.len() <= remaining {
        output.push_str(&arguments);
    } else {
        let marker = "…[truncated]";
        let budget = remaining.saturating_sub(marker.len());
        for character in arguments.chars() {
            if output.len().saturating_add(character.len_utf8()) > prefix_len + budget {
                break;
            }
            output.push(character);
        }
        output.push_str(marker);
    }
    output
}

struct SessionHookDurableBridge {
    session: Arc<Mutex<heycode_session::Session>>,
}

#[async_trait]
impl heycode_hooks::HookDurableEventBridge for SessionHookDurableBridge {
    async fn commit(
        &self,
        event: heycode_hooks::HookContributionEvent<'_>,
        cancellation: CancellationToken,
    ) -> Result<(), heycode_hooks::HookBridgeFault> {
        if cancellation.is_cancelled() {
            return Err(heycode_hooks::HookBridgeFault::Cancelled);
        }
        let provenance = event.provenance();
        let phase = match provenance.phase() {
            heycode_hooks::HookPhase::Pre => heycode_session::HookContributionPhase::Pre,
            heycode_hooks::HookPhase::Post => heycode_session::HookContributionPhase::Post,
        };
        let lifecycle = match provenance.event() {
            heycode_hooks::HookEvent::ToolUse => heycode_session::HookContributionEvent::ToolUse,
            heycode_hooks::HookEvent::Turn => heycode_session::HookContributionEvent::Turn,
            heycode_hooks::HookEvent::Session => heycode_session::HookContributionEvent::Session,
            heycode_hooks::HookEvent::UserPrompt => {
                heycode_session::HookContributionEvent::UserPrompt
            }
            heycode_hooks::HookEvent::Subagent => heycode_session::HookContributionEvent::Subagent,
            heycode_hooks::HookEvent::McpServer => {
                heycode_session::HookContributionEvent::McpServer
            }
            _ => return Err(heycode_hooks::HookBridgeFault::Failed),
        };
        let handler = match provenance.handler_kind() {
            heycode_hooks::HookHandlerKind::Command => {
                heycode_session::HookContributionHandler::Command
            }
            heycode_hooks::HookHandlerKind::Prompt => {
                heycode_session::HookContributionHandler::Prompt
            }
            heycode_hooks::HookHandlerKind::Subagent => {
                heycode_session::HookContributionHandler::Subagent
            }
            heycode_hooks::HookHandlerKind::McpTool => {
                heycode_session::HookContributionHandler::McpTool
            }
            _ => return Err(heycode_hooks::HookBridgeFault::Failed),
        };
        let record = heycode_session::HookContributionRecord::new(
            provenance.owner(),
            phase,
            lifecycle,
            handler,
            provenance.boundary(),
            event.text(),
        )
        .map_err(|_| heycode_hooks::HookBridgeFault::Failed)?;
        let (sequence, directory) = {
            let mut session = self
                .session
                .lock()
                .map_err(|_| heycode_hooks::HookBridgeFault::Failed)?;
            let appended = session
                .append(heycode_session::SessionEventKind::HookContribution {
                    contribution: Box::new(record.clone()),
                })
                .map_err(|_| heycode_hooks::HookBridgeFault::Failed)?;
            session
                .flush()
                .map_err(|_| heycode_hooks::HookBridgeFault::Failed)?;
            let directory = session
                .path()
                .parent()
                .ok_or(heycode_hooks::HookBridgeFault::Failed)?
                .to_path_buf();
            (appended.seq, directory)
        };
        let reopened = heycode_session::Session::open(directory)
            .map_err(|_| heycode_hooks::HookBridgeFault::Failed)?;
        match reopened
            .events()
            .get(sequence as usize)
            .map(|event| &event.kind)
        {
            Some(heycode_session::SessionEventKind::HookContribution { contribution })
                if contribution.as_ref() == &record =>
            {
                Ok(())
            }
            _ => Err(heycode_hooks::HookBridgeFault::Failed),
        }
    }
}

/// Attach one ProductHookAdapter to HookService, Agent and SubagentRegistry.
#[must_use]
pub fn product_hook_attachments_plugin(
    adapter: Arc<ProductHookAdapter>,
) -> Box<dyn heycode_core::Plugin> {
    struct AttachmentsPlugin(Arc<ProductHookAdapter>);
    impl heycode_core::Plugin for AttachmentsPlugin {
        fn name(&self) -> &'static str {
            "product-hook-attachments"
        }

        fn descriptor(&self) -> heycode_core::PluginDescriptor {
            heycode_core::PluginDescriptor::built_in(
                self.name(),
                env!("CARGO_PKG_VERSION"),
                &[heycode_core::PluginContributionKind::Waterfall],
            )
        }

        fn inject(&self) -> &'static [heycode_core::ServiceKey] {
            &[
                heycode_hooks::SERVICE_HOOKS,
                heycode_session::SERVICE_SESSION,
                heycode_agent::SERVICE_AGENT,
                heycode_agent::SERVICE_SUBAGENTS,
            ]
        }

        fn apply(&self, context: &mut heycode_core::Context) -> heycode_core::CoreResult<()> {
            let hooks = context
                .get::<heycode_hooks::HookService>(heycode_hooks::SERVICE_HOOKS)
                .ok_or_else(|| heycode_core::CoreError::other("hook service missing"))?;
            let session = context
                .get::<Mutex<heycode_session::Session>>(heycode_session::SERVICE_SESSION)
                .ok_or_else(|| heycode_core::CoreError::other("session service missing"))?;
            let agent = context
                .get::<heycode_agent::Agent>(heycode_agent::SERVICE_AGENT)
                .ok_or_else(|| heycode_core::CoreError::other("agent service missing"))?;
            let subagents = context
                .get::<heycode_agent::SubagentRegistry>(heycode_agent::SERVICE_SUBAGENTS)
                .ok_or_else(|| heycode_core::CoreError::other("subagent service missing"))?;
            self.0.attach(context, hooks, session)?;
            let agent_port: Arc<dyn heycode_agent::LifecycleHookPort> = self.0.clone();
            agent
                .attach_lifecycle_hooks(context, agent_port)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))?;
            let subagent_port: Arc<dyn heycode_agent::LifecycleHookPort> = self.0.clone();
            subagents
                .attach_lifecycle_hooks(context, subagent_port)
                .map_err(|error| heycode_core::CoreError::other(error.to_string()))
        }
    }
    Box::new(AttachmentsPlugin(adapter))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    struct PromptRunner;

    #[async_trait]
    impl heycode_hooks::PromptHookRunner for PromptRunner {
        async fn run(
            &self,
            _prompt: &str,
            _cancellation: CancellationToken,
        ) -> Result<heycode_hooks::HookAnswer, heycode_hooks::HookFault> {
            Ok(heycode_hooks::HookAnswer::allow_with("committed context"))
        }
    }

    #[tokio::test]
    async fn hook_adapter_appends_flushes_reopens_and_withdraws_as_one_effect() {
        let mut context = heycode_core::Context::new();
        let hooks = Arc::new(heycode_hooks::HookService::new(
            Arc::new(heycode_exec::ShellService::local(
                heycode_exec::LocalShellConfig::platform(
                    std::env::current_dir().unwrap(),
                    std::time::Duration::from_secs(30),
                )
                .unwrap(),
            )),
            heycode_trust::WorkspaceTrustDecision::Trusted,
        ));
        hooks.register_handler(
            &context,
            Arc::new(heycode_hooks::PromptHookHandler::new(Arc::new(
                PromptRunner,
            ))),
        );
        hooks.register(
            &context,
            heycode_hooks::Hook {
                owner: "fixture-owner".to_owned(),
                phase: heycode_hooks::HookPhase::Pre,
                event: heycode_hooks::HookEvent::UserPrompt,
                action: heycode_hooks::HookAction::Prompt("review".to_owned()),
                project_scoped: false,
            },
        );
        let root = tempfile::tempdir().unwrap();
        let session = Arc::new(Mutex::new(
            heycode_session::Session::create(root.path()).unwrap(),
        ));
        let adapter = ProductHookAdapter::default();
        adapter.attach(&context, hooks, session.clone()).unwrap();

        let report = heycode_agent::LifecycleHookPort::run(
            &adapter,
            heycode_agent::LifecycleHookRequest::new(
                heycode_agent::LifecycleHookPhase::Pre,
                heycode_agent::LifecycleHookEvent::UserPrompt,
                "hello".to_owned(),
            ),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(report, heycode_agent::LifecycleHookReport::proceed());
        let messages = {
            let session = session.lock().unwrap();
            assert!(matches!(
                session.events().last().map(|event| &event.kind),
                Some(heycode_session::SessionEventKind::HookContribution { contribution })
                    if contribution.text() == "committed context"
            ));
            heycode_session::derive_messages(session.events())
        };
        assert_eq!(messages[0].content, "committed context");

        context.shutdown();
        let before = session.lock().unwrap().events().len();
        let report = heycode_agent::LifecycleHookPort::run(
            &adapter,
            heycode_agent::LifecycleHookRequest::new(
                heycode_agent::LifecycleHookPhase::Pre,
                heycode_agent::LifecycleHookEvent::UserPrompt,
                "later".to_owned(),
            ),
            CancellationToken::new(),
        )
        .await;
        assert_eq!(report, heycode_agent::LifecycleHookReport::proceed());
        assert_eq!(session.lock().unwrap().events().len(), before);
    }

    #[tokio::test]
    async fn one_router_per_server_routes_elicitation_and_deactivation_settles_waiters() {
        let product = McpProductSession::new(
            &heycode_core::SessionId::from_raw("session-one"),
            Arc::new(heycode_agent::AutoApprove),
        )
        .unwrap();
        let server = heycode_mcp::McpServerId::new("fixture").unwrap();
        let router = product
            .router(
                server.clone(),
                heycode_mcp::McpElicitationCapabilities::form_only(),
            )
            .unwrap();
        assert!(matches!(
            product.router(server, heycode_mcp::McpElicitationCapabilities::form_only()),
            Err(McpProductAttachmentError::DuplicateServerRoute)
        ));
        let (mut receiver, activation) = product.ui.activate().unwrap();
        let pending = router
            .admit_request(&serde_json::json!({
                "jsonrpc":"2.0",
                "id":7,
                "method":"elicitation/create",
                "params":{
                    "message":"Choose a name",
                    "requestedSchema":{
                        "type":"object",
                        "properties":{"name":{"type":"string"}},
                        "required":["name"]
                    }
                }
            }))
            .unwrap()
            .unwrap();
        let task = tokio::spawn(async move { pending.resolve().await.unwrap() });
        let event = receiver.recv().await.unwrap();
        let McpTuiEvent::Elicitation { id, request } = event else {
            panic!("expected elicitation")
        };
        assert_eq!(request.server().as_str(), "fixture");
        assert!(product.ui.answer(
            id,
            Ok(heycode_mcp::McpElicitationResponse::accept(
                serde_json::json!({"name":"Ada"}),
            )),
        ));
        assert_eq!(task.await.unwrap().as_json()["result"]["action"], "accept");

        let pending = router
            .admit_request(&serde_json::json!({
                "jsonrpc":"2.0",
                "id":8,
                "method":"elicitation/create",
                "params":{
                    "message":"Choose again",
                    "requestedSchema":{"type":"object","properties":{}}
                }
            }))
            .unwrap()
            .unwrap();
        let task = tokio::spawn(async move { pending.resolve().await });
        let _event = receiver.recv().await.unwrap();
        drop(activation);
        assert!(
            task.await.unwrap().is_none(),
            "shutdown sends no late reply"
        );
    }

    #[tokio::test]
    async fn routed_progress_logs_and_elicitation_reach_the_real_app_state_and_flat_view() {
        let product = McpProductSession::new(
            &heycode_core::SessionId::from_raw("session-one"),
            Arc::new(heycode_agent::AutoApprove),
        )
        .unwrap();
        let router = product
            .router(
                heycode_mcp::McpServerId::new("fixture").unwrap(),
                heycode_mcp::McpElicitationCapabilities::form_only(),
            )
            .unwrap();
        let (mut receiver, _activation) = product.ui.activate().unwrap();
        let progress = router.begin_progress();
        assert!(router.observe_notification(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"notifications/progress",
            "params":{
                "progressToken":progress.token().to_json(),
                "progress":1,
                "total":2,
                "message":"halfway"
            }
        })));
        router.enable_logging();
        assert!(router.observe_notification(&serde_json::json!({
            "jsonrpc":"2.0",
            "method":"notifications/message",
            "params":{"level":"warning","logger":"fixture","data":{"safe":"log"}}
        })));
        let mut state = crate::AppState::new("model", std::path::PathBuf::from("."));
        state.receive_mcp_event(receiver.recv().await.unwrap());
        state.receive_mcp_event(receiver.recv().await.unwrap());
        assert!(matches!(
            state.items.as_slice(),
            [crate::Item::Info(progress), crate::Item::Info(log)]
                if progress.contains("MCP fixture progress 1/2 · halfway")
                    && log.contains("MCP fixture warning fixture")
        ));

        let pending = router
            .admit_request(&serde_json::json!({
                "jsonrpc":"2.0",
                "id":9,
                "method":"elicitation/create",
                "params":{
                    "message":"Choose a name",
                    "requestedSchema":{
                        "type":"object",
                        "properties":{"name":{"type":"string"}}
                    }
                }
            }))
            .unwrap()
            .unwrap();
        let task = tokio::spawn(async move { pending.resolve().await });
        state.receive_mcp_event(receiver.recv().await.unwrap());
        let flat = crate::ScreenReaderSnapshot::from_state(&state);
        assert!(flat.as_text().contains("MCP elicitation"));
        assert!(flat.as_text().contains("server: fixture"));
        assert!(flat.as_text().contains("fields: name"));
        assert!(!state.handle_terminal_event(&crossterm::event::Event::Key(
            crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Esc,
                crossterm::event::KeyModifiers::NONE,
            ),
        )));
        let (id, response) = state.take_mcp_elicitation_response().unwrap();
        assert!(product.ui.answer(id, response));
        assert_eq!(
            task.await.unwrap().unwrap().as_json()["result"]["action"],
            "decline"
        );
    }

    struct RecordingPolicy {
        calls: Mutex<Vec<(String, serde_json::Value)>>,
    }

    #[async_trait]
    impl heycode_agent::ApprovalPolicy for RecordingPolicy {
        async fn decide(&self, call: &heycode_tools::ToolCallInput) -> heycode_tools::Verdict {
            self.calls
                .lock()
                .unwrap()
                .push((call.name.clone(), call.args.clone()));
            heycode_tools::Verdict::Allow
        }
    }

    /// The Agent's `admit_call` is the one decision point for an MCP call. The
    /// call-time handler must not ask the same policy a second time — that was
    /// the source of duplicate approval cards — so it consults the policy's
    /// kind only and never re-decides the call.
    #[tokio::test]
    async fn mcp_prompt_defers_to_the_single_agent_decision_and_never_asks_twice() {
        let policy = Arc::new(RecordingPolicy {
            calls: Mutex::new(Vec::new()),
        });
        let product = McpProductSession::new(
            &heycode_core::SessionId::from_raw("session-one"),
            policy.clone(),
        )
        .unwrap();
        let request = || {
            heycode_mcp::McpToolApprovalRequest::new(
                heycode_mcp::McpServerId::new("fixture").unwrap(),
                "write".to_owned(),
                "mcp__fixture__write".to_owned(),
                serde_json::json!({"path":"file"}),
            )
        };
        let decision = product
            .approval_handler()
            .decide(request(), CancellationToken::new())
            .await;
        assert_eq!(decision, heycode_mcp::McpToolApprovalDecision::Allow);
        assert!(
            policy.calls.lock().unwrap().is_empty(),
            "the policy was not asked a second time for the same call"
        );

        let denying = McpProductSession::new(
            &heycode_core::SessionId::from_raw("session-two"),
            Arc::new(heycode_agent::DenyAll),
        )
        .unwrap();
        assert_eq!(
            denying
                .approval_handler()
                .decide(request(), CancellationToken::new())
                .await,
            heycode_mcp::McpToolApprovalDecision::Deny
        );
    }
}
