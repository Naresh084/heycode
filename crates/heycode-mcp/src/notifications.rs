//! Server-to-client notification routing, one epoch per family.
//!
//! Three listings can change independently, so three epochs must advance
//! independently. A single shared [`McpListChangeWatch`] would make a
//! `notifications/tools/list_changed` discard a prompt walk that nothing had
//! invalidated, and a `notifications/prompts/list_changed` discard a sound tool
//! generation — silently, and only under load.
//!
//! The separation is therefore structural rather than remembered.
//! [`McpNotificationRouter`] constructs all three epochs itself and accepts none
//! from a caller, so there is no way to hand the same watch to two families. The
//! resource registry is likewise built by the router and handed out through
//! [`McpNotificationRouter::resources`], so a Consumer cannot end up holding a
//! registry that notifications never reach.
//!
//! [`McpNotificationKind`] is deliberately not `#[non_exhaustive]`: a fourth
//! family must fail to compile here rather than be quietly unrouted.

use std::sync::Arc;

use crate::McpClientEventRouter;
use crate::generation::McpListChangeWatch;
use crate::prompts::PROMPTS_LIST_CHANGED_NOTIFICATION;
use crate::resources::{
    McpResourceListLimits, McpResourceRegistry, NOTIFICATION_RESOURCE_LIST_CHANGED,
    NOTIFICATION_RESOURCE_UPDATED,
};

/// `notifications/tools/list_changed` — `ToolListChangedNotification.method`.
///
/// <https://modelcontextprotocol.io/specification/2025-11-25/server/tools#list-changed-notification>
pub const TOOLS_LIST_CHANGED_NOTIFICATION: &str = "notifications/tools/list_changed";

/// Every server-to-client notification heycode routes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpNotificationKind {
    /// The server's tool list changed (MCP06).
    ToolsListChanged,
    /// The server's prompt list changed (MCP09).
    PromptsListChanged,
    /// The server's resource list changed (MCP08).
    ResourcesListChanged,
    /// One subscribed resource changed (MCP08).
    ResourceUpdated,
    /// Request-scoped progress (MCP11).
    Progress,
    /// Connection-scoped structured log message (MCP11).
    LogMessage,
    /// Cancellation of one server-originated client request (MCP11).
    Cancelled,
    /// Completion of one accepted URL elicitation (MCP11).
    ElicitationComplete,
}

impl McpNotificationKind {
    /// Every routed notification, in listing order.
    ///
    /// A surface can iterate this to prove it handles all of them; a test checks
    /// the array length against the variant count, so the list cannot silently
    /// fall behind the enum.
    pub const ALL: [Self; 8] = [
        Self::ToolsListChanged,
        Self::PromptsListChanged,
        Self::ResourcesListChanged,
        Self::ResourceUpdated,
        Self::Progress,
        Self::LogMessage,
        Self::Cancelled,
        Self::ElicitationComplete,
    ];

    /// Exact JSON-RPC method name.
    #[must_use]
    pub const fn method(self) -> &'static str {
        match self {
            Self::ToolsListChanged => TOOLS_LIST_CHANGED_NOTIFICATION,
            Self::PromptsListChanged => PROMPTS_LIST_CHANGED_NOTIFICATION,
            Self::ResourcesListChanged => NOTIFICATION_RESOURCE_LIST_CHANGED,
            Self::ResourceUpdated => NOTIFICATION_RESOURCE_UPDATED,
            Self::Progress => "notifications/progress",
            Self::LogMessage => "notifications/message",
            Self::Cancelled => "notifications/cancelled",
            Self::ElicitationComplete => "notifications/elicitation/complete",
        }
    }

    /// Classify one incoming JSON-RPC message.
    ///
    /// A message carrying an `id` is a response, never a notification, so it is
    /// not classified here even if it also names a notification method.
    #[must_use]
    pub fn parse(message: &serde_json::Value) -> Option<Self> {
        if message.get("id").is_some() {
            return None;
        }
        let method = message.get("method").and_then(serde_json::Value::as_str)?;
        Self::ALL.into_iter().find(|kind| kind.method() == method)
    }
}

struct RouterInner {
    tools: McpListChangeWatch,
    prompts: McpListChangeWatch,
    resources: Arc<McpResourceRegistry>,
    client_events: Option<McpClientEventRouter>,
}

/// The notification sink one connection hands to its transport.
///
/// Cloning shares the same three epochs and the same resource registry, so a
/// transport, a generation owner and a UI all observe one connection's truth.
#[derive(Clone)]
pub struct McpNotificationRouter {
    inner: Arc<RouterInner>,
}

impl std::fmt::Debug for McpNotificationRouter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpNotificationRouter")
            .field("tools_epoch", &self.inner.tools.epoch())
            .field("prompts_epoch", &self.inner.prompts.epoch())
            .finish_non_exhaustive()
    }
}

impl Default for McpNotificationRouter {
    fn default() -> Self {
        Self::new()
    }
}

impl McpNotificationRouter {
    /// Build one connection's routing plane with default resource bounds.
    #[must_use]
    pub fn new() -> Self {
        Self::with_resource_limits(McpResourceListLimits::default())
    }

    /// Build one connection's routing plane with explicit resource bounds.
    ///
    /// There is deliberately no constructor taking a watch or a registry: the
    /// three epochs must be independent, and the registry that receives updates
    /// must be the one the Consumer reads.
    #[must_use]
    pub fn with_resource_limits(limits: McpResourceListLimits) -> Self {
        Self {
            inner: Arc::new(RouterInner {
                tools: McpListChangeWatch::new(),
                prompts: McpListChangeWatch::new(),
                resources: Arc::new(McpResourceRegistry::new(limits)),
                client_events: None,
            }),
        }
    }

    /// Build one connection plane with an exact product-session event route.
    #[must_use]
    pub fn with_client_events(
        limits: McpResourceListLimits,
        client_events: McpClientEventRouter,
    ) -> Self {
        Self {
            inner: Arc::new(RouterInner {
                tools: McpListChangeWatch::new(),
                prompts: McpListChangeWatch::new(),
                resources: Arc::new(McpResourceRegistry::new(limits)),
                client_events: Some(client_events),
            }),
        }
    }

    /// The tool list-change epoch, for [`crate::McpToolGenerationOwner`].
    #[must_use]
    pub fn tools(&self) -> McpListChangeWatch {
        self.inner.tools.clone()
    }

    /// The prompt list-change epoch, for
    /// [`crate::prompts::McpPromptGenerationOwner`].
    #[must_use]
    pub fn prompts(&self) -> McpListChangeWatch {
        self.inner.prompts.clone()
    }

    /// This connection's resource registry, which owns the third epoch.
    #[must_use]
    pub fn resources(&self) -> Arc<McpResourceRegistry> {
        Arc::clone(&self.inner.resources)
    }

    /// MCP11 event/request router attached to this connection, when enabled.
    #[must_use]
    pub fn client_events(&self) -> Option<McpClientEventRouter> {
        self.inner.client_events.clone()
    }

    /// Exact client capability object for `initialize`.
    #[must_use]
    pub fn client_capabilities(&self) -> serde_json::Value {
        self.inner.client_events.as_ref().map_or_else(
            || serde_json::json!({}),
            McpClientEventRouter::client_capabilities,
        )
    }

    /// Enable human logging only after the server advertised it.
    pub fn enable_logging(&self) {
        if let Some(router) = &self.inner.client_events {
            router.enable_logging();
        }
    }

    /// Terminally retire this connection's MCP11 route.
    pub fn shutdown(&self) {
        if let Some(router) = &self.inner.client_events {
            router.shutdown();
        }
    }

    /// Cancel every MCP11 row the dead transport owned, keeping the route.
    ///
    /// The reconnect supervisor hands the replacement child this very router,
    /// so a crashed transport must not retire the plane the recovered one will
    /// use. [`Self::shutdown`] stays the terminal disposal.
    pub fn retire_pending(&self) {
        if let Some(router) = &self.inner.client_events {
            router.retire_pending();
        }
    }

    /// Route one incoming message, returning which family it advanced.
    ///
    /// Cannot fail: an unmodelled notification is `None` and is left alone, and
    /// a malformed resource update is absorbed and counted by the resource
    /// registry rather than failing the connection. A notification is never
    /// worth killing a working session over.
    pub fn observe(&self, message: &serde_json::Value) -> Option<McpNotificationKind> {
        let kind = McpNotificationKind::parse(message)?;
        match kind {
            McpNotificationKind::ToolsListChanged => self.inner.tools.mark_changed(),
            McpNotificationKind::PromptsListChanged => self.inner.prompts.mark_changed(),
            McpNotificationKind::ResourcesListChanged | McpNotificationKind::ResourceUpdated => {
                let _outcome = self.inner.resources.observe_notification(message);
            }
            McpNotificationKind::Progress
            | McpNotificationKind::LogMessage
            | McpNotificationKind::Cancelled
            | McpNotificationKind::ElicitationComplete => {
                if let Some(router) = &self.inner.client_events {
                    let _observed = router.observe_notification(message);
                }
            }
        }
        Some(kind)
    }
}
