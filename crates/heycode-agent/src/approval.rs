//! Approval policy seam: who may run which tool call.

use async_trait::async_trait;
use std::sync::Arc;

use heycode_tools::{ToolCallInput, Verdict};
use tokio_util::sync::CancellationToken;

/// Effective behavior class of the composed approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicyKind {
    /// Allow tool calls without prompting.
    FullAccess,
    /// Read-only planning with explicit human review before implementation.
    Plan,
    /// Allow file reads and edits; ask for other actions, with optional exact grants.
    AcceptedEdits,
    /// AI classification, only when a classifier is installed.
    Auto,
    /// Ask a human through the active front end.
    Ask,
    /// Deny every tool call.
    Deny,
    /// Plugin-defined policy outside the built-in classes.
    Custom,
}

impl ApprovalPolicyKind {
    /// Stable UI/diagnostic id.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::FullAccess => "full_access",
            Self::AcceptedEdits => "accepted_edits",
            Self::Auto => "auto",
            Self::Ask => "ask",
            Self::Deny => "deny",
            Self::Custom => "custom",
        }
    }
    /// Short product name, shared by controls and status.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Plan => "Plan",
            Self::FullAccess => "Full access",
            Self::AcceptedEdits => "Accepted edits",
            Self::Ask => "Default",
            Self::Auto => "Auto",
            Self::Deny => "Blocked",
            Self::Custom => "Custom",
        }
    }
    /// Plain-language explanation shown alongside the mode.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Plan => "Inspect and plan; review the full plan before making changes.",
            Self::FullAccess => "We won't ask for permission.",
            Self::AcceptedEdits => {
                "Read and edit files without asking; ask before running commands and other actions."
            }
            Self::Ask => "We'll ask for permission each time.",
            Self::Auto => "AI decides when to ask for permission.",
            Self::Deny => "Actions are paused.",
            Self::Custom => "Permissions are managed by your connection.",
        }
    }
}

/// Decides whether a model-issued tool call may execute.
///
/// Implemented by the headless default (`AutoApprove`) and by interactive
/// front ends (the TUI dialog). Called BEFORE the guard waterfall; a denial
/// here short-circuits execution and becomes the model-visible error result.
#[async_trait]
pub trait ApprovalPolicy: Send + Sync {
    /// Explicit plan review is independent of ordinary automatic tool approval.
    async fn review_plan(
        &self,
        _plan: &str,
        _cancellation: CancellationToken,
    ) -> crate::PlanReviewDecision {
        crate::PlanReviewDecision::StayInPlan {
            feedback: "Explicit plan review is unavailable on this surface".into(),
        }
    }

    /// Commit a reviewed target and its durable plan record while policy readers are excluded.
    fn commit_plan_transition(
        &self,
        _target: ApprovalPolicyKind,
        _commit: &mut (dyn FnMut() -> Result<(), String> + Send),
    ) -> Result<(), String> {
        Err("Plan acceptance requires a switchable interactive policy".into())
    }

    /// Effective behavior for status/diagnostic consumers.
    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::Custom
    }
    /// Build a fresh child-specific permission policy from the surface's prompter.
    /// Returning none makes explicit interactive child modes fail closed.
    fn child_policy(&self, _kind: ApprovalPolicyKind) -> Option<Arc<dyn ApprovalPolicy>> {
        None
    }

    /// Apply a child ceiling in addition to this policy. Switchable policies
    /// snapshot their active policy before deciding, so transitions cannot skip
    /// either ceiling. A fresh parent prompt already satisfies an asking child.
    async fn decide_with_child_policy(
        &self,
        call: &ToolCallInput,
        child: &dyn ApprovalPolicy,
        cancellation: CancellationToken,
    ) -> Verdict {
        if child.kind() == ApprovalPolicyKind::Deny {
            return child.decide_cancellable(call, cancellation).await;
        }
        if self.kind() == ApprovalPolicyKind::Ask {
            return self.decide_fresh_cancellable(call, cancellation).await;
        }
        match self.decide_cancellable(call, cancellation.clone()).await {
            verdict @ Verdict::Deny { .. } => verdict,
            Verdict::Allow => child.decide_cancellable(call, cancellation).await,
        }
    }

    /// Return [`Verdict::Allow`] or a [`Verdict::Deny`] with the reason the
    /// model will see.
    async fn decide(&self, call: &ToolCallInput) -> Verdict;

    /// Ask and report whether the user explicitly granted identical future calls.
    async fn decide_grant_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> (Verdict, bool) {
        (
            self.decide_fresh_cancellable(call, cancellation).await,
            false,
        )
    }

    /// Ask without using or granting a remembered permission.
    async fn decide_fresh_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> Verdict {
        self.decide_cancellable(call, cancellation).await
    }

    /// Decide with one caller-owned cancellation token.
    ///
    /// Stateful interactive implementations override this to withdraw their
    /// exact pending waiter. The default is correct for stateless policies.
    async fn decide_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> Verdict {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Verdict::Deny {
                reason: "approval cancelled".to_owned(),
            },
            verdict = self.decide(call) => verdict,
        }
    }
}

/// Approves everything — headless/default policy.
#[derive(Debug, Default, Clone, Copy)]
pub struct AutoApprove;

#[async_trait]
impl ApprovalPolicy for AutoApprove {
    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::FullAccess
    }

    async fn decide(&self, _call: &ToolCallInput) -> Verdict {
        Verdict::Allow
    }
}

/// Denies every tool call — used by tests and locked-down profiles.
#[derive(Debug, Default, Clone, Copy)]
pub struct DenyAll;

#[async_trait]
impl ApprovalPolicy for DenyAll {
    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::Deny
    }

    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        Verdict::Deny {
            reason: format!("tool `{}` denied by approval policy", call.name),
        }
    }
}

/// `approval.mode = ask` where nothing can show a dialog: headless `heycode run`.
///
/// Waiting on a prompt nobody will answer hung the process forever with no
/// output. This policy denies every call with a reason the model — and the
/// user reading the transcript — can act on, and reports itself as `Ask` so
/// `/status` and the durable record still say what the configuration asked
/// for.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnpromptedDeny;

/// The deny reason [`UnpromptedDeny`] gives every call.
pub const UNPROMPTED_DENY_REASON: &str = "approval required: approval.mode=ask has no interactive prompt in headless mode — pass `--approval full_access` to allow tools, or run the interactive TUI";

#[async_trait]
impl ApprovalPolicy for UnpromptedDeny {
    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::Ask
    }

    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        Verdict::Deny {
            reason: format!("tool `{}` denied: {UNPROMPTED_DENY_REASON}", call.name),
        }
    }
}

/// An approval policy the user can change while the session runs.
///
/// `/permissions full_access|accepted_edits|default` swaps the active policy
/// without a restart. AI Auto requires a classifier and cannot imply Full access. `ask`
/// needs the surface's interactive prompter; a surface without one refuses
/// the switch instead of parking calls on a dialog nobody will answer.
pub struct SwitchableApproval {
    child_interactive: Option<Arc<dyn ApprovalPolicy>>,
    current: std::sync::RwLock<Arc<dyn ApprovalPolicy>>,
    interactive: Option<Arc<dyn ApprovalPolicy>>,
    accepted: Option<Arc<dyn ApprovalPolicy>>,
    review: Option<Arc<dyn ApprovalPolicy>>,
    plan: std::sync::RwLock<Option<std::sync::Weak<crate::PlanMode>>>,
}

impl SwitchableApproval {
    /// Start on `initial`; `interactive` is the policy `ask` switches to.
    #[must_use]
    pub fn new(
        initial: Arc<dyn ApprovalPolicy>,
        interactive: Option<Arc<dyn ApprovalPolicy>>,
    ) -> Self {
        let review = interactive.clone();
        let child_interactive = interactive.clone();
        let accepted = interactive
            .as_ref()
            .map(|policy| Arc::new(AcceptedEdits::new(policy.clone())) as Arc<dyn ApprovalPolicy>);
        let interactive =
            interactive.map(|policy| Arc::new(AlwaysAsk(policy)) as Arc<dyn ApprovalPolicy>);
        let initial = match initial.kind() {
            ApprovalPolicyKind::Ask => interactive.clone().unwrap_or(initial),
            ApprovalPolicyKind::AcceptedEdits => accepted.clone().unwrap_or(initial),
            _ => initial,
        };
        Self {
            child_interactive,
            current: std::sync::RwLock::new(initial),
            interactive,
            accepted,
            review,
            plan: std::sync::RwLock::new(None),
        }
    }

    /// Switch to `mode` for the rest of the session.
    ///
    /// # Errors
    /// `Ask` on a surface with no interactive prompter, or `Custom`, which
    /// names no policy.
    pub fn switch(&self, mode: ApprovalPolicyKind) -> Result<(), &'static str> {
        let next = self.policy_for(mode)?;
        let plan = self
            .plan
            .read()
            .map_err(|_| "Plan policy binding unavailable")?
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let update = || {
            let mut current = self
                .current
                .write()
                .map_err(|_| "approval policy state is poisoned")?;
            *current = next;
            Ok(())
        };
        match plan {
            Some(plan) => plan.switch_policy_if_not_planning(update),
            None => update(),
        }
    }

    /// Apply an explicit user mode selection, including leaving Plan manually.
    /// Tool-driven policy changes must continue to use [`Self::switch`].
    pub fn switch_by_user(&self, mode: ApprovalPolicyKind) -> Result<(), String> {
        let next = self.policy_for(mode).map_err(str::to_owned)?;
        let plan = self
            .plan
            .read()
            .map_err(|_| "Plan policy binding unavailable")?
            .as_ref()
            .and_then(std::sync::Weak::upgrade);
        let update = |commit: &mut (dyn FnMut() -> Result<(), String> + Send)| {
            let mut current = self
                .current
                .write()
                .map_err(|_| "approval policy state is poisoned")?;
            commit()?;
            *current = next;
            Ok(())
        };
        match plan {
            Some(plan) => plan.switch_policy_by_user(update),
            None => update(&mut || Ok(())),
        }
    }

    /// Attach the session's guard so status and direct switches use actual Plan authority.
    pub fn attach_plan(&self, plan: &Arc<crate::PlanMode>) {
        *self.plan.write().unwrap_or_else(|e| e.into_inner()) = Some(Arc::downgrade(plan));
    }

    fn plan_active(&self) -> bool {
        self.plan.read().map_or(true, |plan| {
            plan.as_ref()
                .and_then(std::sync::Weak::upgrade)
                .is_some_and(|plan| plan.active())
        })
    }

    fn policy_for(
        &self,
        mode: ApprovalPolicyKind,
    ) -> Result<Arc<dyn ApprovalPolicy>, &'static str> {
        let next: Arc<dyn ApprovalPolicy> = match mode {
            ApprovalPolicyKind::Plan => return Err("Enter Plan through the session mode owner"),
            ApprovalPolicyKind::FullAccess => Arc::new(AutoApprove),
            ApprovalPolicyKind::AcceptedEdits => self
                .accepted
                .clone()
                .ok_or("Accepted edits needs an interactive conversation")?,
            ApprovalPolicyKind::Auto => {
                return Err("Auto is unavailable for this connection. Choose another mode.");
            }
            ApprovalPolicyKind::Deny => Arc::new(DenyAll),
            ApprovalPolicyKind::Ask => self
                .interactive
                .clone()
                .ok_or("this surface has no interactive prompt; `ask` is unavailable here")?,
            ApprovalPolicyKind::Custom => return Err("`custom` is not a mode to switch to"),
        };
        Ok(next)
    }

    fn snapshot(&self) -> Arc<dyn ApprovalPolicy> {
        self.current
            .read()
            .map(|policy| policy.clone())
            .unwrap_or_else(|_| Arc::new(DenyAll))
    }
}

#[async_trait]
impl ApprovalPolicy for SwitchableApproval {
    fn child_policy(&self, kind: ApprovalPolicyKind) -> Option<Arc<dyn ApprovalPolicy>> {
        let interactive = self.child_interactive.clone()?;
        match kind {
            ApprovalPolicyKind::Ask => Some(Arc::new(AlwaysAsk(interactive))),
            ApprovalPolicyKind::AcceptedEdits => Some(Arc::new(AcceptedEdits::new(interactive))),
            _ => None,
        }
    }

    async fn decide_with_child_policy(
        &self,
        call: &ToolCallInput,
        child: &dyn ApprovalPolicy,
        cancellation: CancellationToken,
    ) -> Verdict {
        self.snapshot()
            .decide_with_child_policy(call, child, cancellation)
            .await
    }

    fn kind(&self) -> ApprovalPolicyKind {
        if self.plan_active() {
            ApprovalPolicyKind::Plan
        } else {
            self.snapshot().kind()
        }
    }

    async fn review_plan(
        &self,
        plan: &str,
        cancellation: CancellationToken,
    ) -> crate::PlanReviewDecision {
        match &self.review {
            Some(review) => review.review_plan(plan, cancellation).await,
            None => crate::PlanReviewDecision::StayInPlan {
                feedback: "Explicit plan review is unavailable on this surface".into(),
            },
        }
    }

    fn commit_plan_transition(
        &self,
        target: ApprovalPolicyKind,
        commit: &mut (dyn FnMut() -> Result<(), String> + Send),
    ) -> Result<(), String> {
        if !matches!(
            target,
            ApprovalPolicyKind::Ask | ApprovalPolicyKind::AcceptedEdits
        ) {
            return Err("Plan review can select only Accepted edits or Default".into());
        }
        let next = self.policy_for(target)?;
        let mut current = self
            .current
            .write()
            .map_err(|_| "approval policy state is poisoned")?;
        commit()?;
        *current = next;
        Ok(())
    }

    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        self.snapshot().decide(call).await
    }

    async fn decide_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> Verdict {
        self.snapshot().decide_cancellable(call, cancellation).await
    }
}

/// Route one fully identified tool action through an existing approval policy.
///
/// This keeps product adapters from depending directly on the tool crate just
/// to construct the policy's internal request type.
#[must_use]
pub async fn decide_named_tool(
    policy: &dyn ApprovalPolicy,
    name: impl Into<String>,
    args: serde_json::Value,
    cancellation: CancellationToken,
) -> bool {
    let call = ToolCallInput {
        name: name.into(),
        args,
    };
    matches!(
        policy.decide_cancellable(&call, cancellation).await,
        Verdict::Allow
    )
}

/// Default mode ignores previous grants and asks on every call.
pub struct AlwaysAsk(pub Arc<dyn ApprovalPolicy>);

#[async_trait]
impl ApprovalPolicy for AlwaysAsk {
    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::Ask
    }
    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        self.decide_cancellable(call, CancellationToken::new())
            .await
    }
    async fn decide_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> Verdict {
        self.0.decide_fresh_cancellable(call, cancellation).await
    }
}

/// File reads and edits are approved for the conversation. Other actions need
/// an explicit decision, optionally remembered for identical tool inputs.
/// The gate makes concurrent first uses share a successful permission grant.
pub struct AcceptedEdits {
    interactive: Arc<dyn ApprovalPolicy>,
    allowed: tokio::sync::Mutex<Vec<crate::SessionAllowRule>>,
}

impl AcceptedEdits {
    /// Built-in file operations covered by the user's edit-mode selection.
    /// Shells, arbitrary MCP tools, network and task tools still require approval.
    #[must_use]
    pub fn allows_file_tool(name: &str) -> bool {
        matches!(
            name,
            "read"
                | "read_many"
                | "glob"
                | "grep"
                | "edit"
                | "multi_edit"
                | "write"
                | "apply_patch"
        )
    }

    /// Create an empty conversation grant set.
    #[must_use]
    pub fn new(interactive: Arc<dyn ApprovalPolicy>) -> Self {
        Self {
            interactive,
            allowed: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ApprovalPolicy for AcceptedEdits {
    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::AcceptedEdits
    }
    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        self.decide_cancellable(call, CancellationToken::new())
            .await
    }
    async fn decide_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> Verdict {
        if cancellation.is_cancelled() {
            return Verdict::Deny {
                reason: "approval cancelled".into(),
            };
        }
        if Self::allows_file_tool(&call.name) {
            return Verdict::Allow;
        }
        let mut allowed = tokio::select! {
            biased;
            () = cancellation.cancelled() => return Verdict::Deny { reason: "approval cancelled".into() },
            allowed = self.allowed.lock() => allowed,
        };
        let rule = crate::SessionAllowRule::for_call(&call.name, &call.args);
        if allowed.contains(&rule) {
            return Verdict::Allow;
        }
        let (verdict, remember) = self
            .interactive
            .decide_grant_cancellable(call, cancellation.clone())
            .await;
        if cancellation.is_cancelled() {
            return Verdict::Deny {
                reason: "approval cancelled".into(),
            };
        }
        if remember && matches!(verdict, Verdict::Allow) {
            allowed.push(rule);
        }
        verdict
    }
}

/// A custom child's additional approval ceiling. Parent transitions continue
/// to apply; a preset can never grant authority the parent denies.
pub(crate) struct ChildApproval {
    pub(crate) parent: Arc<dyn ApprovalPolicy>,
    pub(crate) child: Arc<dyn ApprovalPolicy>,
}

#[async_trait]
impl ApprovalPolicy for ChildApproval {
    fn kind(&self) -> ApprovalPolicyKind {
        self.child.kind()
    }
    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        self.decide_cancellable(call, CancellationToken::new())
            .await
    }
    async fn decide_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: CancellationToken,
    ) -> Verdict {
        self.parent
            .decide_with_child_policy(call, self.child.as_ref(), cancellation)
            .await
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod child_tests {
    use super::*;
    struct PromptCounter(std::sync::atomic::AtomicUsize);
    #[async_trait]
    impl ApprovalPolicy for PromptCounter {
        async fn decide(&self, _: &ToolCallInput) -> Verdict {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Verdict::Allow
        }
        async fn decide_grant_cancellable(
            &self,
            call: &ToolCallInput,
            _: CancellationToken,
        ) -> (Verdict, bool) {
            (self.decide(call).await, true)
        }
    }
    #[tokio::test]
    async fn child_grants_are_isolated_and_parent_transitions_still_deny() {
        let prompts = Arc::new(PromptCounter(std::sync::atomic::AtomicUsize::new(0)));
        let parent = Arc::new(SwitchableApproval::new(
            Arc::new(AutoApprove),
            Some(prompts.clone()),
        ));
        let first = ChildApproval {
            parent: parent.clone(),
            child: parent
                .child_policy(ApprovalPolicyKind::AcceptedEdits)
                .unwrap(),
        };
        let second = ChildApproval {
            parent: parent.clone(),
            child: parent
                .child_policy(ApprovalPolicyKind::AcceptedEdits)
                .unwrap(),
        };
        let call = ToolCallInput {
            name: "mcp__files__write".into(),
            args: serde_json::json!({"path":"a","content":"b"}),
        };
        assert!(matches!(first.decide(&call).await, Verdict::Allow));
        assert!(matches!(first.decide(&call).await, Verdict::Allow));
        assert_eq!(prompts.0.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(matches!(second.decide(&call).await, Verdict::Allow));
        assert_eq!(prompts.0.load(std::sync::atomic::Ordering::SeqCst), 2);
        parent.switch(ApprovalPolicyKind::Deny).unwrap();
        assert!(matches!(first.decide(&call).await, Verdict::Deny { .. }));
        assert_eq!(prompts.0.load(std::sync::atomic::Ordering::SeqCst), 2);
        parent.switch(ApprovalPolicyKind::Ask).unwrap();
        assert!(matches!(first.decide(&call).await, Verdict::Allow));
        assert_eq!(
            prompts.0.load(std::sync::atomic::Ordering::SeqCst),
            3,
            "one parent prompt satisfies the child ceiling"
        );
    }
}
