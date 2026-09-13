//! Interactive approval: the TUI dialog policy.
//!
//! `decide()` publishes an [`UiEvent::ApprovalRequested`] on the shared bus
//! and parks until a front end calls [`InteractiveApproval::answer`]. The
//! headless world never mounts this policy; `auto`/`deny` remain non-blocking.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio::sync::oneshot;

use heycode_tools::{ToolCallInput, Verdict};

use crate::approval::{ApprovalPolicy, ApprovalPolicyKind};
use crate::ui::UiEvent;

/// One dialog waiting for a human decision.
#[derive(Debug, Clone)]
pub struct PendingAsk {
    /// Monotonic id matching the emitted event.
    pub id: u64,
    /// Tool name.
    pub name: String,
    /// Short argument preview for the card.
    pub args_preview: String,
}

/// One pending ask delivered to subscribers (TUI, ACP forwarder, …).
#[derive(Debug, Clone)]
pub struct AskNotification {
    /// Monotonic id to pass back to [`InteractiveApproval::answer`].
    pub id: u64,
    /// Tool name.
    pub name: String,
    /// Argument preview.
    pub args_preview: String,
}

/// One transport-owned stream of interactive approval notifications.
///
/// Dropping the stream unregisters it immediately, so an inactive transport
/// cannot retain an unbounded queue across unrelated turns.
pub struct AskSubscription {
    id: u64,
    receiver: tokio::sync::mpsc::UnboundedReceiver<AskNotification>,
    subscribers: Arc<Mutex<HashMap<u64, tokio::sync::mpsc::UnboundedSender<AskNotification>>>>,
}

impl AskSubscription {
    /// Wait for the next approval notification.
    pub async fn recv(&mut self) -> Option<AskNotification> {
        self.receiver.recv().await
    }

    /// Receive one already-buffered approval notification.
    pub fn try_recv(&mut self) -> Result<AskNotification, tokio::sync::mpsc::error::TryRecvError> {
        self.receiver.try_recv()
    }
}

impl Drop for AskSubscription {
    fn drop(&mut self) {
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.remove(&self.id);
        }
    }
}

/// Shared handle installed as service `"approval-interactive"` when the
/// config selects `ask` mode. Consumers either listen on bus events or take an
/// independent typed subscription; answers always flow through
/// [`Self::answer`].
#[derive(Clone)]
pub struct InteractiveApproval {
    bus: Arc<std::sync::RwLock<heycode_core::EventBus>>,
    counter: ArcCounter,
    waiters: Arc<Mutex<HashMap<u64, oneshot::Sender<AskAnswer>>>>,
    subscriber_counter: ArcCounter,
    subscribers: Arc<Mutex<HashMap<u64, tokio::sync::mpsc::UnboundedSender<AskNotification>>>>,
    plan_waiters: Arc<Mutex<HashMap<u64, oneshot::Sender<crate::PlanReviewDecision>>>>,
    plan_review_available: Arc<std::sync::atomic::AtomicBool>,
    /// Rules the user granted for the rest of this session.
    session_rules: Arc<Mutex<Vec<SessionAllowRule>>>,
}

/// Longest denial reason a card accepts. A reason is a sentence for the
/// model, not a document; the R02 detail bound is far above this.
pub const MAX_DENY_REASON_CHARS: usize = 500;

/// How a user answered one approval card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskAnswer {
    /// Allow this one call.
    Allow,
    /// Allow this call and every later call matching its rule this session.
    AllowSession,
    /// Refuse this call.
    Deny,
    /// Refuse this call and tell the model why.
    ///
    /// The text is the model-visible denial, so "no, use the staging bucket"
    /// redirects the turn where a bare refusal only stops it. Build it with
    /// [`AskAnswer::deny_with_reason`], which normalises and bounds the text.
    DenyWithReason(String),
}

impl AskAnswer {
    /// A denial carrying the user's own words.
    ///
    /// Blank text is not a reason and degrades to [`AskAnswer::Deny`].
    /// Control characters (a pasted newline, an escape sequence) collapse to
    /// spaces so a reason can never carry a control sequence into a
    /// front end, and the whole thing is bounded by
    /// [`MAX_DENY_REASON_CHARS`] with a visible cut.
    #[must_use]
    pub fn deny_with_reason(reason: impl AsRef<str>) -> Self {
        let flattened = reason
            .as_ref()
            .chars()
            .map(|character| {
                if character.is_control() {
                    ' '
                } else {
                    character
                }
            })
            .collect::<String>();
        let trimmed = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
        if trimmed.is_empty() {
            return Self::Deny;
        }
        if trimmed.chars().count() <= MAX_DENY_REASON_CHARS {
            return Self::DenyWithReason(trimmed);
        }
        let mut bounded: String = trimmed
            .chars()
            .take(MAX_DENY_REASON_CHARS.saturating_sub(1))
            .collect();
        bounded.push('…');
        Self::DenyWithReason(bounded)
    }

    /// The user's reason, when they gave one.
    #[must_use]
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::DenyWithReason(reason) => Some(reason),
            Self::Allow | Self::AllowSession | Self::Deny => None,
        }
    }
}

/// A grant that auto-allows later calls without a card.
///
/// Matches the complete JSON input and exact tool name. Object key order does
/// not matter; strings, array order, missing fields and values must match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionAllowRule {
    /// Tool name the rule applies to.
    pub tool: String,
    /// Complete input parameters; never a command prefix or a truncated preview.
    pub args: serde_json::Value,
}

impl SessionAllowRule {
    /// The exact permission one call would create.
    #[must_use]
    pub fn for_call(name: &str, args: &serde_json::Value) -> Self {
        Self {
            tool: name.to_owned(),
            args: args.clone(),
        }
    }

    /// Human-readable scope, without echoing potentially sensitive arguments.
    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} with identical inputs", self.tool)
    }
}

struct PlanWaiterCleanup {
    policy: InteractiveApproval,
    id: u64,
}
impl Drop for PlanWaiterCleanup {
    fn drop(&mut self) {
        if let Ok(mut waiters) = self.policy.plan_waiters.lock() {
            waiters.remove(&self.id);
        }
        if let Ok(bus) = self.policy.bus.read() {
            bus.emit(UiEvent::PlanReviewResolved { id: self.id });
        }
    }
}

type ArcCounter = std::sync::Arc<AtomicU64>;

impl InteractiveApproval {
    /// Bind the policy to the event bus it should announce asks on.
    #[must_use]
    pub fn new(bus: heycode_core::EventBus) -> Self {
        Self {
            bus: Arc::new(std::sync::RwLock::new(bus)),
            counter: ArcCounter::new(AtomicU64::new(0)),
            waiters: Arc::new(Mutex::new(HashMap::new())),
            subscriber_counter: ArcCounter::new(AtomicU64::new(0)),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            session_rules: Arc::new(Mutex::new(Vec::new())),
            plan_waiters: Arc::new(Mutex::new(HashMap::new())),
            plan_review_available: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Attach a surface which implements the full typed plan-review protocol.
    /// Ordinary permission subscribers do not establish this capability.
    pub fn set_plan_review_available(&self, available: bool) {
        self.plan_review_available
            .store(available, Ordering::SeqCst);
        if !available {
            self.dismiss_plan_reviews();
        }
    }

    fn dismiss_plan_reviews(&self) -> usize {
        let senders = self
            .plan_waiters
            .lock()
            .map(|mut waiters| {
                waiters
                    .drain()
                    .map(|(_, sender)| sender)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let count = senders.len();
        for sender in senders {
            let _sent = sender.send(crate::PlanReviewDecision::dismissed());
        }
        count
    }

    /// Resolve only a dedicated review, with an explicit target or feedback.
    pub fn answer_plan(&self, id: u64, decision: crate::PlanReviewDecision) -> bool {
        self.plan_waiters
            .lock()
            .ok()
            .and_then(|mut waiters| waiters.remove(&id))
            .is_some_and(|sender| sender.send(decision).is_ok())
    }

    /// Whether a full-document review still owns a live waiter.
    #[must_use]
    pub fn plan_is_pending(&self, id: u64) -> bool {
        self.plan_waiters
            .lock()
            .is_ok_and(|waiters| waiters.contains_key(&id))
    }

    /// Rules granted with "allow for this session", oldest first.
    #[must_use]
    pub fn session_rules(&self) -> Vec<SessionAllowRule> {
        self.session_rules
            .lock()
            .map(|rules| rules.clone())
            .unwrap_or_default()
    }

    fn session_rule_allows(&self, call: &ToolCallInput) -> bool {
        let candidate = SessionAllowRule::for_call(&call.name, &call.args);
        self.session_rules
            .lock()
            .is_ok_and(|rules| rules.contains(&candidate))
    }

    pub(crate) fn bind_bus(&self, bus: heycode_core::EventBus) {
        if let Ok(mut current) = self.bus.write() {
            *current = bus;
        }
    }

    /// Resolve a pending ask: `true` allows, `false` denies. Unknown ids are
    /// ignored so stale dialogs cannot crash the loop.
    pub fn answer(&self, id: u64, allow: bool) {
        self.answer_with(
            id,
            if allow {
                AskAnswer::Allow
            } else {
                AskAnswer::Deny
            },
        );
    }

    /// Resolve a pending ask with a full answer. Unknown ids are ignored.
    pub fn answer_with(&self, id: u64, answer: AskAnswer) {
        let _delivered = self.try_answer_with(id, answer);
    }

    /// Deliver an answer only to a still-live decision. A stale card must not
    /// report successful approval after its owning call has already ended.
    #[must_use]
    pub fn try_answer_with(&self, id: u64, answer: AskAnswer) -> bool {
        let sender = self
            .waiters
            .lock()
            .map(|mut w| w.remove(&id))
            .ok()
            .flatten();
        sender.is_some_and(|sender| sender.send(answer).is_ok())
    }

    /// Fail closed every currently pending decision. Front ends call this
    /// during cancellation/teardown so an abandoned dialog cannot keep an
    /// admitted turn alive.
    #[must_use]
    pub fn deny_all_pending(&self) -> usize {
        let reviews = self.dismiss_plan_reviews();
        let senders = self
            .waiters
            .lock()
            .map(|mut waiters| {
                waiters
                    .drain()
                    .map(|(_, sender)| sender)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let count = senders.len();
        for sender in senders {
            let _sent = sender.send(AskAnswer::Deny);
        }
        count + reviews
    }

    /// One currently outstanding ask, if any (front-end state hydration).
    ///
    /// There can be more than one: a batch admits serially, but a background
    /// subagent runs its own turn against this same policy. Callers that own
    /// a single dialog slot must re-hydrate from here after answering, or
    /// fail the rest closed with [`Self::deny_all_pending`].
    #[must_use]
    pub fn pending_id(&self) -> Option<u64> {
        self.waiters.lock().ok()?.keys().next().copied()
    }

    /// Whether one exact ask is still waiting for an answer.
    ///
    /// Transport adapters use this to discard a typed notification left in
    /// their queue after the owning tool call was cancelled.
    #[must_use]
    pub fn is_pending(&self, id: u64) -> bool {
        self.waiters
            .lock()
            .is_ok_and(|waiters| waiters.contains_key(&id))
    }

    /// Create an independent typed notification stream. Transport layers
    /// prefer this over parsing bus events; more than one can be present in a
    /// composed world even though only the active surface drives turns.
    pub fn take_subscription(&self) -> Option<AskSubscription> {
        let id = self
            .subscriber_counter
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |current| {
                current.checked_add(1)
            })
            .ok()?;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        self.subscribers.lock().ok()?.insert(id, sender);
        Some(AskSubscription {
            id,
            receiver,
            subscribers: self.subscribers.clone(),
        })
    }
}

#[async_trait]
impl ApprovalPolicy for InteractiveApproval {
    async fn review_plan(
        &self,
        plan: &str,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> crate::PlanReviewDecision {
        if !self.plan_review_available.load(Ordering::SeqCst) {
            return crate::PlanReviewDecision::StayInPlan {
                feedback:
                    "This surface does not support full-document plan review; remain read-only"
                        .into(),
            };
        }
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut waiters) = self.plan_waiters.lock() {
            waiters.insert(id, tx);
        }
        let _cleanup = PlanWaiterCleanup {
            policy: self.clone(),
            id,
        };
        if let Ok(bus) = self.bus.read() {
            bus.emit(UiEvent::PlanReviewRequested {
                id,
                plan: plan.to_owned(),
            });
        }
        tokio::select! {
            biased;
            () = cancellation.cancelled() => crate::PlanReviewDecision::dismissed(),
            decision = rx => decision.unwrap_or_else(|_| crate::PlanReviewDecision::dismissed()),
        }
    }

    fn kind(&self) -> ApprovalPolicyKind {
        ApprovalPolicyKind::Ask
    }

    async fn decide(&self, call: &ToolCallInput) -> Verdict {
        self.decide_cancellable(call, tokio_util::sync::CancellationToken::new())
            .await
    }

    async fn decide_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Verdict {
        self.request_approval(call, cancellation, true).await.0
    }

    async fn decide_grant_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> (Verdict, bool) {
        self.request_approval(call, cancellation, false).await
    }

    async fn decide_fresh_cancellable(
        &self,
        call: &ToolCallInput,
        cancellation: tokio_util::sync::CancellationToken,
    ) -> Verdict {
        self.request_approval(call, cancellation, false).await.0
    }
}

impl InteractiveApproval {
    async fn request_approval(
        &self,
        call: &ToolCallInput,
        cancellation: tokio_util::sync::CancellationToken,
        remember: bool,
    ) -> (Verdict, bool) {
        if cancellation.is_cancelled() {
            return (
                Verdict::Deny {
                    reason: "approval cancelled".into(),
                },
                false,
            );
        }
        if remember && self.session_rule_allows(call) {
            return (Verdict::Allow, false);
        }
        let id = self.counter.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = oneshot::channel();
        if let Ok(mut waiters) = self.waiters.lock() {
            waiters.insert(id, tx);
        }
        let owner = crate::code_mode::EXECUTING_TOOLS
            .try_with(Clone::clone)
            .ok()
            .filter(|owner| self.bus.read().is_ok_and(|bus| !bus.same_bus(&owner.bus)));
        let mut cleanup = ApprovalWaiterCleanup {
            policy: self.clone(),
            id,
            answered: false,
            owner_bus: owner.as_ref().map(|owner| owner.bus.clone()),
        };
        let args_preview = preview_of(&call.args);
        let owner_session = owner.as_ref().map(|owner| {
            owner
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .id()
                .to_string()
        });
        let event = UiEvent::ApprovalRequested {
            id,
            name: call.name.clone(),
            args_preview: args_preview.clone(),
            owner_session,
        };
        if let Some(owner) = &owner {
            owner.bus.emit(event.clone());
        }
        if let Ok(bus) = self.bus.read() {
            bus.emit(event);
        }
        let notification = AskNotification {
            id,
            name: call.name.clone(),
            args_preview,
        };
        if let Ok(mut subscribers) = self.subscribers.lock() {
            subscribers.retain(|_, subscriber| subscriber.send(notification.clone()).is_ok());
        }
        let answer = tokio::select! {
            biased;
            () = cancellation.cancelled() => None,
            answer = rx => answer.ok(),
        };
        cleanup.answered = answer.is_some();
        let grant = matches!(answer, Some(AskAnswer::AllowSession)) && !cancellation.is_cancelled();
        let verdict = match answer {
            Some(AskAnswer::Allow) => Verdict::Allow,
            Some(AskAnswer::AllowSession) => {
                if remember
                    && grant
                    && let Ok(mut rules) = self.session_rules.lock()
                {
                    let rule = SessionAllowRule::for_call(&call.name, &call.args);
                    if !rules.contains(&rule) {
                        rules.push(rule);
                    }
                }
                Verdict::Allow
            }
            Some(AskAnswer::Deny) => Verdict::Deny {
                reason: DENIED_BY_DIALOG.to_owned(),
            },
            Some(AskAnswer::DenyWithReason(reason)) => Verdict::Deny {
                reason: format!("denied by the user: {reason}"),
            },
            // The front end vanished mid-ask: fail closed.
            None => Verdict::Deny {
                reason: if cancellation.is_cancelled() {
                    "approval cancelled"
                } else {
                    "approval channel closed"
                }
                .to_owned(),
            },
        };
        if let Some(bus) = cleanup.owner_bus.take() {
            bus.emit(UiEvent::ApprovalResolved {
                id,
                allowed: matches!(verdict, Verdict::Allow),
            });
        }
        (verdict, grant)
    }
}

/// Withdraw the card even if the execution future is dropped rather than
/// explicitly cancelled. The waiter and UI must share the call's lifetime.
struct ApprovalWaiterCleanup {
    policy: InteractiveApproval,
    id: u64,
    answered: bool,
    owner_bus: Option<heycode_core::EventBus>,
}

impl Drop for ApprovalWaiterCleanup {
    fn drop(&mut self) {
        if !self.answered
            && let Some(bus) = &self.owner_bus
        {
            bus.emit(UiEvent::ApprovalResolved {
                id: self.id,
                allowed: false,
            });
        }

        if let Ok(mut waiters) = self.policy.waiters.lock() {
            waiters.remove(&self.id);
        }
        if !self.answered
            && let Ok(bus) = self.policy.bus.read()
        {
            bus.emit(UiEvent::ApprovalResolved {
                id: self.id,
                allowed: false,
            });
        }
    }
}

/// Model-visible text for a denial the user gave no reason for.
const DENIED_BY_DIALOG: &str = "denied via approval dialog";

/// Longest argument preview a card carries; the R02 detail bound is 16 KiB.
const MAX_PREVIEW_CHARS: usize = 8 * 1024;

/// The card shows the WHOLE argument the user is approving — a truncated
/// command is a command they did not see — with the shell command first and
/// unquoted. Only pathological sizes are cut, at the end, with a marker.
fn preview_of(args: &serde_json::Value) -> String {
    let joined = match args {
        serde_json::Value::Object(map) => {
            let mut parts: Vec<String> = map
                .iter()
                .map(|(k, v)| match v {
                    serde_json::Value::String(text) => format!("{k}: {text}"),
                    other => format!("{k}: {other}"),
                })
                .collect();
            // `command` and `path` are what the user is deciding about.
            parts.sort_by_key(|part| {
                (
                    !(part.starts_with("command: ") || part.starts_with("path: ")),
                    part.clone(),
                )
            });
            parts.join("\n")
        }
        other => other.to_string(),
    };
    if joined.chars().count() > MAX_PREVIEW_CHARS {
        let cut: String = joined.chars().take(MAX_PREVIEW_CHARS).collect();
        format!("{cut}… (truncated)")
    } else {
        joined
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use heycode_core::EventBus;

    fn call(name: &str) -> ToolCallInput {
        ToolCallInput {
            name: name.to_owned(),
            args: serde_json::json!({"path":"a.rs"}),
        }
    }

    #[tokio::test]
    async fn allow_answer_resolves_to_allow_verdict() {
        let bus = EventBus::default();
        let policy = InteractiveApproval::new(bus.clone());
        let seen = Arc::new(Mutex::<Vec<UiEvent>>::default());
        let sink = seen.clone();
        bus.on::<UiEvent>(move |e| sink.lock().unwrap().push(e.clone()));

        let p = policy.clone();
        let task = tokio::spawn(async move { p.decide(&call("bash")).await });
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(matches!(
            seen.lock().unwrap().last(),
            Some(UiEvent::ApprovalRequested { id: 0, .. })
        ));
        assert!(policy.is_pending(0));
        assert!(!policy.is_pending(99));
        policy.answer(0, true);
        assert!(matches!(task.await.unwrap(), Verdict::Allow));
        assert!(!policy.is_pending(0));
    }

    #[tokio::test]
    async fn cancelled_or_dropped_call_withdraws_card_and_rejects_stale_answers() {
        for abort in [false, true] {
            let bus = EventBus::default();
            let events = Arc::new(Mutex::new(Vec::new()));
            let observed = events.clone();
            bus.on::<UiEvent>(move |event| observed.lock().unwrap().push(event.clone()));
            let policy = InteractiveApproval::new(bus);
            let mut requests = policy.take_subscription().unwrap();
            let pending = policy.clone();
            let cancellation = tokio_util::sync::CancellationToken::new();
            let task_cancellation = cancellation.clone();
            let task = tokio::spawn(async move {
                pending
                    .decide_cancellable(&call("write"), task_cancellation)
                    .await
            });
            let request = requests.recv().await.unwrap();
            if abort {
                task.abort();
                assert!(task.await.unwrap_err().is_cancelled());
            } else {
                cancellation.cancel();
                assert!(
                    matches!(task.await.unwrap(), Verdict::Deny { reason } if reason == "approval cancelled")
                );
            }
            assert!(!policy.is_pending(request.id));
            assert!(!policy.try_answer_with(request.id, AskAnswer::Allow));
            assert!(events.lock().unwrap().iter().any(|event| matches!(event,
                UiEvent::ApprovalResolved { id, allowed: false } if *id == request.id
            )));
        }
    }

    #[tokio::test]
    async fn each_transport_gets_an_independent_typed_subscription() {
        let policy = InteractiveApproval::new(EventBus::default());
        let mut first = policy.take_subscription().expect("first subscriber");
        let mut second = policy.take_subscription().expect("second subscriber");
        let pending = policy.clone();
        let task = tokio::spawn(async move { pending.decide(&call("read")).await });

        let first = first.recv().await.expect("first notification");
        let second = second.recv().await.expect("second notification");
        assert_eq!(first.id, second.id);
        assert_eq!(first.name, "read");
        assert_eq!(second.name, "read");
        policy.answer(first.id, false);
        assert!(matches!(task.await.unwrap(), Verdict::Deny { .. }));
    }

    #[tokio::test]
    async fn deny_answer_carries_reason_and_unknown_ids_are_ignored() {
        let policy = InteractiveApproval::new(EventBus::default());
        let p = policy.clone();
        let task = tokio::spawn(async move { p.decide(&call("write")).await });
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        policy.answer(99, true); // stale id: ignored
        policy.answer(0, false);
        match task.await.unwrap() {
            Verdict::Deny { reason } => assert!(reason.contains("dialog")),
            other => panic!("expected denial, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn teardown_denies_every_pending_ask_before_returning() {
        let policy = InteractiveApproval::new(EventBus::default());
        let first = policy.clone();
        let second = policy.clone();
        let first = tokio::spawn(async move { first.decide(&call("write")).await });
        let second = tokio::spawn(async move { second.decide(&call("bash")).await });
        for _ in 0..100 {
            if policy.waiters.lock().unwrap().len() == 2 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(policy.waiters.lock().unwrap().len(), 2);
        assert_eq!(policy.deny_all_pending(), 2);
        assert!(matches!(first.await.unwrap(), Verdict::Deny { .. }));
        assert!(matches!(second.await.unwrap(), Verdict::Deny { .. }));
        assert_eq!(policy.deny_all_pending(), 0);
    }

    #[tokio::test]
    async fn shared_policy_rebinds_to_the_composed_event_bus() {
        let original = EventBus::default();
        let composed = EventBus::default();
        let original_events = Arc::new(Mutex::new(Vec::new()));
        let composed_events = Arc::new(Mutex::new(Vec::new()));
        let original_sink = original_events.clone();
        original.on::<UiEvent>(move |event| {
            original_sink.lock().unwrap().push(event.clone());
        });
        let composed_sink = composed_events.clone();
        composed.on::<UiEvent>(move |event| {
            composed_sink.lock().unwrap().push(event.clone());
        });
        let policy = InteractiveApproval::new(original);
        policy.bind_bus(composed);

        let pending = policy.clone();
        let task = tokio::spawn(async move { pending.decide(&call("write")).await });
        for _ in 0..100 {
            if policy.pending_id().is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(original_events.lock().unwrap().is_empty());
        assert!(matches!(
            composed_events.lock().unwrap().last(),
            Some(UiEvent::ApprovalRequested { .. })
        ));
        policy.answer(0, false);
        assert!(matches!(task.await.unwrap(), Verdict::Deny { .. }));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod session_rule_tests {
    use super::*;
    use heycode_core::EventBus;

    fn bash(command: &str) -> ToolCallInput {
        ToolCallInput {
            name: "bash".to_owned(),
            args: serde_json::json!({"command": command}),
        }
    }

    /// Remembering one shell call never authorizes another command from the same program.
    #[tokio::test]
    async fn allow_session_grants_exact_inputs_not_a_program() {
        let policy = InteractiveApproval::new(EventBus::default());
        let first = tokio::spawn({
            let policy = policy.clone();
            async move { policy.decide(&bash("cargo test --workspace")).await }
        });
        tokio::task::yield_now().await;
        let id = policy.pending_id().expect("a card is open");
        policy.answer_with(id, AskAnswer::AllowSession);
        assert!(matches!(first.await.unwrap(), Verdict::Allow));
        assert_eq!(
            policy
                .session_rules()
                .iter()
                .map(SessionAllowRule::describe)
                .collect::<Vec<_>>(),
            ["bash with identical inputs"]
        );

        // Identical inputs: no card, allowed directly.
        assert!(matches!(
            policy.decide(&bash("cargo test --workspace")).await,
            Verdict::Allow
        ));
        assert!(policy.pending_id().is_none());

        // Same program with changed inputs: a card opens again.
        let second = tokio::spawn({
            let policy = policy.clone();
            async move { policy.decide(&bash("cargo fmt")).await }
        });
        tokio::task::yield_now().await;
        let id = policy.pending_id().expect("changed arguments ask again");
        policy.answer_with(id, AskAnswer::Deny);
        assert!(matches!(second.await.unwrap(), Verdict::Deny { .. }));
    }

    /// Claude Code lets a denial carry a sentence, and the model reads it:
    /// "no, use the staging bucket" is a redirection, "denied" is a wall.
    #[tokio::test]
    async fn a_denial_reason_becomes_the_model_visible_text() {
        let policy = InteractiveApproval::new(EventBus::default());
        let call = tokio::spawn({
            let policy = policy.clone();
            async move { policy.decide(&bash("rm -rf /")).await }
        });
        tokio::task::yield_now().await;
        let id = policy.pending_id().expect("a card is open");
        policy.answer_with(
            id,
            AskAnswer::deny_with_reason("use the staging bucket instead"),
        );
        let Verdict::Deny { reason } = call.await.unwrap() else {
            panic!("a reasoned answer still denies");
        };
        assert_eq!(reason, "denied by the user: use the staging bucket instead");

        // Blank text is not a reason; the plain phrase stands.
        let call = tokio::spawn({
            let policy = policy.clone();
            async move { policy.decide(&bash("rm -rf /tmp")).await }
        });
        tokio::task::yield_now().await;
        let id = policy.pending_id().expect("a card is open");
        policy.answer_with(id, AskAnswer::deny_with_reason("   "));
        let Verdict::Deny { reason } = call.await.unwrap() else {
            panic!("still denies");
        };
        assert_eq!(reason, "denied via approval dialog");

        // A pathological paste is bounded, and the cut is visible.
        assert_eq!(
            AskAnswer::deny_with_reason("x".repeat(5_000)),
            AskAnswer::DenyWithReason(format!("{}…", "x".repeat(MAX_DENY_REASON_CHARS - 1)))
        );
        assert_eq!(
            AskAnswer::deny_with_reason("line one\nline two"),
            AskAnswer::DenyWithReason("line one line two".to_owned()),
            "a reason is one line of text, never a control-bearing blob"
        );
    }

    #[test]
    fn previews_show_the_whole_command_first_and_never_cut_it_short() {
        let long = "x".repeat(600);
        let preview = preview_of(&serde_json::json!({
            "timeout_ms": 30000,
            "command": format!("echo {long}"),
        }));
        assert!(preview.starts_with("command: echo "), "{preview}");
        assert!(preview.contains(&long), "the whole command is present");
        assert!(preview.ends_with("timeout_ms: 30000"), "{preview}");
        assert!(!preview.starts_with('…'));
    }
}
