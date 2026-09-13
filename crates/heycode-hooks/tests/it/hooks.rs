//! O08: lifecycle, timeout, trust and disposal — the four the row names.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use heycode_core::Context;
use heycode_exec::{LocalShellConfig, ShellService};
use heycode_hooks::{
    HOOK_TIMEOUT, Hook, HookAction, HookEvent, HookFault, HookOutcome, HookPhase, HookService,
    SERVICE_HOOKS, hooks_plugin,
};
use heycode_trust::WorkspaceTrustDecision;
use tokio_util::sync::CancellationToken;

fn shell_config() -> LocalShellConfig {
    LocalShellConfig::platform(std::env::current_dir().unwrap(), Duration::from_secs(30)).unwrap()
}

fn shell() -> Arc<ShellService> {
    Arc::new(ShellService::local(shell_config()))
}

fn service(trust: WorkspaceTrustDecision) -> HookService {
    HookService::new(shell(), trust)
}

fn hook(owner: &str, phase: HookPhase, command: &str) -> Hook {
    Hook {
        owner: owner.to_owned(),
        phase,
        event: HookEvent::ToolUse,
        action: HookAction::Command(command.to_owned()),
        project_scoped: false,
    }
}

fn project_hook(owner: &str, command: &str) -> Hook {
    Hook {
        project_scoped: true,
        ..hook(owner, HookPhase::Pre, command)
    }
}

/// Lifecycle, half one: a `Pre` hook exiting non-zero refuses the operation.
#[cfg(unix)]
#[tokio::test]
async fn a_pre_hook_exiting_non_zero_refuses_the_operation() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(&context, hook("guard", HookPhase::Pre, "exit 1"));

    let outcomes = service
        .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
        .await;
    assert_eq!(
        outcomes,
        vec![HookOutcome::Refused {
            owner: "guard".to_owned()
        }]
    );
    assert!(!outcomes[0].proceeds());
}

/// Lifecycle, half two: the same failure in a `Post` hook cannot refuse,
/// because the operation already happened. The phase decides, not the code.
#[cfg(unix)]
#[tokio::test]
async fn the_same_failure_in_a_post_hook_is_a_fault_and_never_a_refusal() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(&context, hook("auditor", HookPhase::Post, "exit 1"));

    let outcomes = service
        .run(
            HookPhase::Post,
            HookEvent::ToolUse,
            CancellationToken::new(),
        )
        .await;
    assert_eq!(
        outcomes,
        vec![HookOutcome::Faulted {
            owner: "auditor".to_owned(),
            fault: HookFault::Failed
        }]
    );
    assert!(
        outcomes[0].proceeds(),
        "a post hook has nothing left to refuse"
    );
    assert!(!HookPhase::Post.can_refuse());
    assert!(HookPhase::Pre.can_refuse());
}

#[cfg(unix)]
#[tokio::test]
async fn a_successful_hook_allows_the_operation() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(&context, hook("ok", HookPhase::Pre, "exit 0"));

    let outcomes = service
        .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
        .await;
    assert_eq!(outcomes, vec![HookOutcome::allowed()]);
    assert!(outcomes[0].proceeds());
}

/// Timeout: a hook that hangs is abandoned rather than hanging heycode, and the
/// fault says so rather than looking like a failure the author chose.
#[cfg(unix)]
#[tokio::test]
async fn a_hanging_hook_is_abandoned_and_reported_as_a_timeout() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(&context, hook("hang", HookPhase::Pre, "sleep 600"));

    // The wait is bounded by the test, not only asserted afterwards. A missing
    // budget would otherwise make this hook run for ten minutes and the test
    // would *hang* rather than fail — and a hang reads as a loaded machine, not
    // as a caught defect (GOTCHAS #163).
    let outcomes = tokio::time::timeout(
        HOOK_TIMEOUT + Duration::from_secs(20),
        service.run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new()),
    )
    .await
    .expect("the hook budget must bound the wait");
    assert_eq!(
        outcomes,
        vec![HookOutcome::Faulted {
            owner: "hang".to_owned(),
            fault: HookFault::TimedOut
        }]
    );
    assert!(
        outcomes[0].proceeds(),
        "a broken hook must not become an outage"
    );
}

/// Trust, and the part that matters: `Unknown` is not trusted. K12's rule is
/// that project executable contributions stay inert until an affirmative
/// decision exists, and no decision is not an affirmative one.
#[tokio::test]
async fn a_project_hook_stays_inert_until_the_workspace_is_affirmatively_trusted() {
    for decision in [
        WorkspaceTrustDecision::Unknown,
        WorkspaceTrustDecision::Restricted,
    ] {
        let context = Context::default();
        let service = service(decision);
        service.register(&context, project_hook("project", "exit 0"));

        let outcomes = service
            .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
            .await;
        assert_eq!(
            outcomes,
            vec![HookOutcome::Faulted {
                owner: "project".to_owned(),
                fault: HookFault::UntrustedWorkspace
            }],
            "decision {decision:?} must not run a project hook"
        );
    }
}

/// A user's own hook is not gated by workspace trust — the gate is about code
/// that arrived with the checkout.
#[cfg(unix)]
#[tokio::test]
async fn a_user_scoped_hook_runs_even_in_an_untrusted_workspace() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Unknown);
    service.register(&context, hook("mine", HookPhase::Pre, "exit 0"));

    assert!(
        service
            .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
            .await
            == vec![HookOutcome::allowed()]
    );
}

/// Disposal: a hook leaves with its owner, and takes nothing else.
#[cfg(unix)]
#[tokio::test]
async fn a_hook_is_removed_when_its_owning_context_shuts_down() {
    let keeper = Context::default();
    let mut leaver = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(&keeper, hook("keeper", HookPhase::Pre, "exit 0"));
    service.register(&leaver, hook("leaver", HookPhase::Pre, "exit 1"));
    assert_eq!(service.len(), 2);

    leaver.shutdown();

    assert_eq!(service.len(), 1, "only the departing owner's hook leaves");
    let outcomes = service
        .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
        .await;
    assert_eq!(
        outcomes,
        vec![HookOutcome::allowed()],
        "the surviving hook is the keeper's"
    );
}

/// A refused operation must not keep running the hooks meant to observe it.
#[cfg(unix)]
#[tokio::test]
async fn a_refusal_stops_the_remaining_hooks_for_that_point() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(&context, hook("first", HookPhase::Pre, "exit 1"));
    service.register(&context, hook("second", HookPhase::Pre, "exit 0"));

    let outcomes = service
        .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
        .await;
    assert_eq!(outcomes.len(), 1, "the second hook must not run");
    assert!(matches!(outcomes[0], HookOutcome::Refused { .. }));
}

/// A fault is not a refusal, so later hooks still run — a broken hook must not
/// silently suppress the ones after it.
#[tokio::test]
async fn a_fault_does_not_stop_the_remaining_hooks() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Restricted);
    service.register(&context, project_hook("blocked", "exit 0"));
    service.register(&context, project_hook("also-blocked", "exit 0"));

    let outcomes = service
        .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
        .await;
    assert_eq!(outcomes.len(), 2);
    assert!(outcomes.iter().all(HookOutcome::proceeds));
}

#[tokio::test]
async fn hooks_are_selected_by_both_phase_and_event() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    for (phase, event) in [
        (HookPhase::Pre, HookEvent::ToolUse),
        (HookPhase::Post, HookEvent::ToolUse),
        (HookPhase::Pre, HookEvent::Turn),
    ] {
        service.register(
            &context,
            Hook {
                owner: format!("{}-{}", phase.as_str(), event.as_str()),
                phase,
                event,
                action: HookAction::Command("exit 0".to_owned()),
                project_scoped: false,
            },
        );
    }
    assert_eq!(
        service.matching(HookPhase::Pre, HookEvent::ToolUse).len(),
        1
    );
    assert_eq!(
        service.matching(HookPhase::Post, HookEvent::ToolUse).len(),
        1
    );
    assert_eq!(service.matching(HookPhase::Pre, HookEvent::Turn).len(), 1);
    assert!(
        service
            .matching(HookPhase::Post, HookEvent::Session)
            .is_empty()
    );
}

/// Counts launches so a test can assert that nothing ran, not merely that the
/// answer looked right. Without this, removing the pre-launch cancellation
/// check survives mutation: the shell refuses a cancelled token anyway and the
/// outcome is identical — a decoration test.
struct CountingBackend {
    inner: heycode_exec::LocalShellConfig,
    launches: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl heycode_exec::ShellBackend for CountingBackend {
    fn resolve(
        &self,
        request: heycode_exec::ShellRequest,
    ) -> Result<heycode_exec::ShellSpec, heycode_exec::ProcessError> {
        ShellService::local(self.inner.clone()).resolve(request)
    }

    async fn execute(
        &self,
        spec: heycode_exec::ShellSpec,
        cancellation: CancellationToken,
    ) -> Result<heycode_exec::ProcessOutput, heycode_exec::ProcessError> {
        self.launches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        ShellService::local(self.inner.clone())
            .execute(spec, cancellation)
            .await
    }
}

/// A cancelled operation must not launch a hook **at all** — asserted by
/// counting launches, because asserting the outcome alone cannot tell a
/// pre-flight check from the shell refusing a cancelled token.
#[tokio::test]
async fn cancellation_before_a_hook_launches_nothing() {
    let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = Arc::new(CountingBackend {
        inner: shell_config(),
        launches: Arc::clone(&launches),
    });
    let context = Context::default();
    let service = HookService::new(
        Arc::new(ShellService::new(backend)),
        WorkspaceTrustDecision::Trusted,
    );
    service.register(&context, hook("never", HookPhase::Pre, "exit 0"));

    let cancelled = CancellationToken::new();
    cancelled.cancel();
    let outcomes = service
        .run(HookPhase::Pre, HookEvent::ToolUse, cancelled)
        .await;

    assert_eq!(
        outcomes,
        vec![HookOutcome::Faulted {
            owner: "never".to_owned(),
            fault: HookFault::Cancelled
        }]
    );
    assert_eq!(
        launches.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a cancelled operation must not launch the hook"
    );
}

/// The counting backend must actually count, or the assertion above is vacuous.
#[cfg(unix)]
#[tokio::test]
async fn the_launch_counter_observes_a_hook_that_does_run() {
    let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let backend = Arc::new(CountingBackend {
        inner: shell_config(),
        launches: Arc::clone(&launches),
    });
    let context = Context::default();
    let service = HookService::new(
        Arc::new(ShellService::new(backend)),
        WorkspaceTrustDecision::Trusted,
    );
    service.register(&context, hook("runs", HookPhase::Pre, "exit 0"));

    service
        .run(HookPhase::Pre, HookEvent::ToolUse, CancellationToken::new())
        .await;
    assert_eq!(launches.load(std::sync::atomic::Ordering::SeqCst), 1);
}

/// A fault carries a closed class and the owner, never the hook's output — a
/// hook's stdout is third-party text whenever the workspace is.
#[cfg(unix)]
#[tokio::test]
async fn a_fault_never_carries_hook_output() {
    let context = Context::default();
    let service = service(WorkspaceTrustDecision::Trusted);
    service.register(
        &context,
        hook("noisy", HookPhase::Post, "echo SECRET-CANARY; exit 3"),
    );

    let outcomes = service
        .run(
            HookPhase::Post,
            HookEvent::ToolUse,
            CancellationToken::new(),
        )
        .await;
    let rendered = format!("{outcomes:?}");
    assert!(!rendered.contains("SECRET-CANARY"), "{rendered}");
    assert!(rendered.contains("noisy"), "the owner is safe and useful");
}

#[tokio::test]
async fn the_plugin_publishes_the_hook_service_and_declares_its_dependency() {
    let plugins: Vec<Box<dyn heycode_core::Plugin>> = vec![
        heycode_exec::local_execution_plugin(shell_config()),
        hooks_plugin(WorkspaceTrustDecision::Trusted),
    ];
    let context = heycode_core::compose(&plugins).expect("composes over shell");
    assert!(context.get::<HookService>(SERVICE_HOOKS).is_some());

    // Composing without the shell must fail loud, naming what is missing.
    let lonely: Vec<Box<dyn heycode_core::Plugin>> =
        vec![hooks_plugin(WorkspaceTrustDecision::Trusted)];
    assert!(heycode_core::compose(&lonely).is_err());
}
