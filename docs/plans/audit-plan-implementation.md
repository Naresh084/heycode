# Audit 37: Plan review and permission handoff

Owner: Plan workstream. Base: `174740a54b664ae2df2150ea2373f0718ea4201b`.

## Implemented native behavior

- `/permissions plan`, the permission picker and Shift+Tab expose Plan. Direct mode changes and `/plan off` cannot bypass full-plan acceptance. External primary runtimes are not advertised as having native read-only enforcement.
- Entry durably blocks new mutations immediately. During active work it waits for the current tool/model boundary, cancels and joins existing jobs, and retires session/world PTYs before announcing fully active Plan. A five-second settlement failure retains read-only state and a retryable pending entry. The final model step also settles a queued entry before idle publication.
- The existing default-deny local guard and delegated-child gate remain in force. Provider-executed routes also receive a shared request-layer guard: unrecognized/mutating hosted routes fail before dispatch. Native child requests inherit that same provider interception chain.
- Local mutating/unknown calls are refused before ordinary approval, using the same Plan decision as the execution guard. Rejected plans cannot produce misleading generic permission prompts; execution still rechecks Plan entry that occurs while approval is pending.
- `exit_plan_mode` bypasses ordinary tool approval and uses a separate typed human decision: Accepted edits, Default permissions, or remain Plan with feedback. AutoApprove, remembered grants, and generic Allow cannot accept a plan. A frontend must explicitly advertise dedicated plan-review support; other surfaces remain read-only without hanging on an unanswerable card.
- Accepted policy validation, one durable `plan/review` event containing the full document and target, the live policy swap, and release of Plan authority happen while policy/Plan readers are excluded. Missing policy support, cancellation, dismissal, or an append failure does not release the guard.
- Proposals, pending/rejected decisions and feedback are durable, version-validated, excluded from ordinary session projection, and available after reopen. The Plan prompt includes saved feedback/proposal; `/plan review` reopens the exact saved document. Restoring an accepted plan restores its specific policy or fails closed at composition.
- The dedicated TUI displays full Markdown, independently scrollable with Up/Down, PageUp/PageDown, Home/End. Tab/Left/Right select among all three choices; Enter confirms. Initial focus is “No, stay in Plan mode”. Typing/pasting adds feedback and selects No; ordinary approval shortcut `y` cannot authorize implementation. Full-screen and screen-reader surfaces use the same decision owner. Effective permission events refresh the footer after a committed acceptance.

## Public interfaces and neighboring wiring

- Agent: `ApprovalPolicyKind::Plan`; `PlanReviewDecision`; `PlanReviewRecord`; `PlanMode::review`; `Agent::enter_plan`; `ApprovalPolicy::{review_plan,commit_plan_transition}`; `SwitchableApproval::attach_plan`.
- Interactive host: `InteractiveApproval::{set_plan_review_available,answer_plan,plan_is_pending}`. Only a surface implementing full-document review should set availability true; it must reset false during teardown.
- UI events: `PlanReviewRequested { id, plan }`, `PlanReviewResolved { id }`, `PermissionModeChanged { mode }`.
- Session v2: `plan/review { plan, decision, feedback }`; decision is validated as `pending`, `accepted_edits`, `default`, or `stay_in_plan`. Accepted values atomically represent target permission and Plan exit. No new v1 kind.
- TUI: isolated `plan_review.rs`; small additions in `app.rs`, `render.rs`, accessibility, permission picker and module registration. Status command routes entry to the native Agent owner.
- Native runtime ignores the human-only review UI events; remote workstream is implementing its own typed native AppServer bridge using the same `InteractiveApproval` API. Generic runtime permission responses must never resolve plan review.

## Validation

- `cargo test -p heycode-agent --test main plan`: **22 passed**. Actual file/shell behavior; both accepted policies before the next mutation; generic-auto rejection; full feedback/reopen; cancellation and failed atomic commit; all four durable states reopened from disk; background process cancellation; delegation in either composition order; provider-executed mutation blocked before transport; shell hooks in both composition orders; worker-tail completion after its terminal row; five-second entry timeout retains read-only state and succeeds only after execution actually ends. Interactive Ask regression rejects a plan, then refuses write/bash/unknown calls without any ordinary approval prompt or file mutation.
- `cargo test -p heycode-session`: **64 unit + 137 integration passed**, including v2 vocabulary, v1 rejection and actual disk reopen.
- `cargo test -p heycode-tui --test main plan_review::`: **4 passed**, exercising the production reducer/typed waiter for all choices and Escape, long Markdown, complete accessible tail and control sanitization.
- `cargo test -p heycode-tui`: **48 unit + 239 integration passed** in the final run, including mode cycling, picker guard, all plan review branches, and existing dialog/shell journeys. The two existing three-mode cycle expectations were updated to include Plan.
- `cargo test -p heycode-hooks`: **22 integration + 1 doctest passed** (no hook-veto weakening).
- `cargo clippy -p heycode-agent -p heycode-hooks -p heycode-tui -p heycode-status -p heycode-session --all-targets -- -D warnings`: **passed on the final implementation**.
- **Production binary + real PTY:** `python3 scripts/plan_review_pty.py --binary target/debug/heycode`. All four independent journeys passed using a loopback OpenRouter-shaped fixture and fresh temporary homes/workspaces. Each entered Plan through the real permissions picker, reviewed a **42,255-byte Markdown document**, scrolled to the unique final marker, and issued the same real shell mutation after review. Accepted edits and Default committed their exact target and showed ordinary tool approval before the marker file was written. No+feedback and Escape durably remained in Plan and the marker never appeared. Evidence: `/tmp/dshx-plan-review-pty/result.json`, per-choice JSON events/requests and terminal captures. The reproducible script is checked in; no paid provider or subscription runtime was used.

## Followup hardening

Configured hooks run outside ordinary tool execution and have no read-only metadata.
`HookExecutionGate` now refuses their execution while Plan is active, retaining
existing hook-veto semantics. A hook-dependent operation may therefore be refused
while planning. Entry waits for already-running hooks to settle before announcing
fully active Plan. Wiring is effect-owned and works in either plugin order. This
adds the acyclic dependency `heycode-agent -> heycode-hooks` plus the optional
`hook-execution-gate` service.

`JobRegistry::wait_for_task_exit` retains registry ownership of the worker handle
while proving both settlement and actual future completion. Plan uses this stronger
boundary; timeout cannot drop a still-running handle and detach it. Root's lifecycle
workstream may replace the implementation with its stronger join API, but must
preserve this guarantee.

## Integration dependencies

- Preserve `ApprovalPolicy::child_policy` added by the custom-config workstream alongside these new methods. Child wrappers must keep parent authority and cannot auto-accept plan review.
- New background/CodeMode/workflow/delegated child APIs must retain the same execution-time guard. `run_code` remains denied by the default-deny allowlist.
- Task UI direct control of existing external children must not bypass Plan's delegated execution policy.
- Root owns all-provider real PTY matrix and full workspace QA after merging the workstreams.
