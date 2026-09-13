# Custom-agent audit implementation

Workstream: custom-agents. Assigned findings A23, A24, A25, A26; native built-in
review/advisor/security-review presets added at root's follow-up request.
Baseline: `174740a54b664ae2df2150ea2373f0718ea4201b` in isolated worktree.
Branch: `codex/custom-agents-audit`. Never modifies the primary checkout.

## Implemented behavior

- A23: strict `AgentDocument.config`, model/inference-provider/adapter-validated
  effort, exact tool allow/deny, permission ceilings, scoped skill catalog and
  argument checks, scoped existing MCP tools, lifetime inference-step limit,
  session/user/project/local memory and background defaults. Native actual
  provider requests and dispatch are tested. Unsupported runtime configuration
  is refused before provider execution. Native worktree selection is declared
  and fails closed here; the lifecycle task owns its lease/execution binding.
- A24: preset instructions enter a distinct section of the provider-neutral
  system instruction slot. User task bytes remain separate in provider requests
  and durable user/message records. The legacy shared request vocabulary has
  no Developer enum; adapters own their wire instruction-role representation.
- A25: shipping `/agent-config list|show|create|save|validate|import|reload`,
  public management service, strict diagnostics, bounded file reads, symlink
  refusal, atomic no-clobber creation/replacement, atomic owned registry reload,
  shutdown disposal and project-over-user-over-built-in alias precedence.
  Malformed files do not prevent startup; invalid reloads retain the old set.
- A26: strict Claude YAML-frontmatter Markdown and Codex standalone TOML import.
  Supported fields map to enforced native controls. Unsupported fields fail
  with explicit names; semantic mismatches fail before file creation. No
  silently discarded policy, inline connection creation, include processing
  or model-alias guessing. Imported descriptions survive conversion.
- Native reviewer/advisor/security-review defaults are discoverable and
  task-callable on the current configured inference route. Their fallback
  ownership permits file overrides. Human `/review` already belongs to TUI;
  root owns human command composition to avoid duplicate command registration.

## Public APIs and integration requirements

Shared configuration-only commit: `3e5654b` (lifecycle task may cherry-pick as a
shared dependency). Final implementation commit and validation results follow
below when checks settle.

- `heycode_agent::{SubagentConfig, ResolvedSubagentConfig, ChildPermissions, ChildMemory, ChildIsolation}`; `Agent::resolved_child_configuration()` exposes actual inference/tool values plus immutable declared policy.
- `SubagentPreset::{with_config, config, with_description, description, with_id}`;
  aliases retain an internal stable source identity for memory.
- `SubagentRequest::{with_configuration, with_preset, instructions, config,
  configuration_key}`. These additions preserve the existing constructor.
- `SubagentRegistry::{replace_presets_owned, register_fallback_preset_owned}`.
  Generation replacement validates duplicate/foreign claims under one lock and
  uses exact registration tokens; old disposers cannot remove new rows.
- `SubagentProvider::supports_configuration()` defaults false. Providers that
  opt in must enforce instructions and every requested supported control.
- `ApprovalPolicy::{child_policy, decide_with_child_policy}` and child prompter
  support in `SwitchableApproval`; parents remain authoritative after mode
  transitions and child remembered grants have separate storage.
- `PromptRegistry::render_excluding` removes the inherited full skill catalog
  before the child exposes its scoped names.
- `AgentDeclarationService::{new,save,reload,diagnostics,close}` published as
  `SERVICE_AGENT_DECLARATIONS`; `import_agent` and `AgentImportFormat` are public.
  `SkippedDeclaration.reason` is now a String for field-level diagnostics.
- `builtin_native_presets()` returns the three validated native defaults.

Runtime lifecycle merge MUST preserve these ordering rules:

1. Resolve the actual parent's current inference route, tools, approval and cwd.
2. Create/retain any native worktree lease using lifecycle's result policy.
3. Prepare memory with the ORIGINAL actual-parent cwd for project identity,
   not the lease cwd. Bind memory tools over lifecycle's resolved `tools`
   registry (including its filesystem/bash sandbox rebinding), never the stale
   runner `self.tools`. `bind_memory_tools` strips inherited memory tools so a
   fresh child cannot accidentally reuse its parent's captured memory identity.
4. Set the inherited inference route on the new Agent, then call
   `configure_custom_child` so explicit preset overrides win. The resulting
   filtered/wrapped registry and child approval are the execution context that
   CodeMode must inherit. Preserve original parent tool guards.
5. The private runner signatures now match lifecycle: `run_child(&SubagentRequest, CancellationToken)` and `run_child_at(&SubagentRequest, CancellationToken, PathBuf)`. Publish/retain the configured child and send using lifecycle's cancellation
   path. The lifecycle task already has `request.config/config()/with_isolation`;
   merge these with this workstream's field/getter rather than duplicating.
6. Replace this branch's explicit unconfigured-worktree refusal with lifecycle's
   native lease path; do not leave shared-workspace fallback or claim A23 fully
   integrated until native worktree tests pass.

Neighboring edits are confined to Agent policy/request construction, approval
composition, prompt section scoping and subagent registry/runner plumbing.
These intentionally overlap lifecycle and plan/permissions workstreams.

## Validation and remaining integration

Final crate regression run: `cargo test -p heycode-agent -p heycode-extension-host
-p heycode-prompt --quiet` passed all 323 tests: 74 Agent unit tests, 210 Agent
integration tests, 2 extension-host unit tests, 5 code-plugin tests, 5 file/import
tests, 12 installed activation tests, 1 product activation test, 7 WASI tests,
and 7 prompt tests. Doc-test targets also passed. The final additional legacy
scope-prefix compatibility test is validated separately below.

The 9 focused native tests include strict adapter effort/model/system/header
projection, actual allowed and denied tool dispatch, default permissions under
headless AutoApprove, lifetime cap across follow-ups, persisted memory across
preset aliases, built-in native preset invocation, background default/override,
and old-child versus new-child behavior across a registry reload. The latter
has an actual successful file-write positive control after reloading.

`cargo clippy -p heycode-agent -p heycode-extension-host -p heycode-prompt --all-targets
-- -D warnings` passed. Formatting and `git diff --check` passed. No generated
reference or full CLI/PTTY claim is made; root owns those cross-stream gates.

A memory unit test initially exposed macOS `/var` alias handling; roots now
canonicalize before validating the derived no-symlink subtree. A background
fixture initially requested the wrong service type (`JobRegistry` rather than
`Arc<JobRegistry>`); the fixture now uses the shipping service binding. These
were isolated and corrected rather than marked as passes. The retained-child
reload positive control also exposed a fixture-only sandbox/cwd mismatch; its
filesystem root now matches the native runner cwd, proving the new child can
write while the old child remains blocked.

Remaining integration belongs to root/lifecycle/UI: native worktree lease path,
actual-parent route/tool inheritance, first-turn cancellation, root CodeMode
execution-context binding, existing `/review` command integration, UI controls,
full product inventory/reference updates and end-to-end PTY QA. No live paid
provider or external CLI run is claimed by this workstream.

Final additional check: `cargo test -p heycode-extension-host --test custom_agents
legacy_scope_prefixed --quiet` passed (1 test). Existing `user-*`/`project-*`
file stems preserve their historical qualified ids and do not create ambiguous
bare aliases. This brings validation coverage to 324 passing tests across the
full crate run and this added regression. Final all-target clippy and formatter
checks also passed after that compatibility change.

## L16 human inspection command follow-up

`/review`, `/advisor`, and `/security-review` now call
`Agent::run_native_inspection` through the native child registry. The TUI declares
its subagent service dependency and inventories all three commands. A current
preset snapshot supplies role instructions and explicit inference overrides;
the command adds an immutable read-only ceiling, a maximum of 32 steps, native
runtime selection, and foreground one-shot execution. An external runtime
preset is refused. There is no automatic stronger-model selection.

Each invocation owns the parent's turn gate and cancellation lease, logs the
slash-command intent, and publishes the child's result and task identity into
the durable parent turn and UI. The lifecycle registry retains the actual child
session/cwd/outcome projection. `/review-runtime` is unchanged. The legacy
retained-turn-limit regression now resolves `TaskSnapshot.session_id` instead
of assuming the early stable task ID names a session directory.

Validation on integrated custom baseline `1767f99`, with the terminal status
identifier typo corrected locally (the root already has canonical fix
`9672d91`/`bf0ad34`): `cargo test -p heycode-tui --quiet` passed 304 tests (50 unit,
254 integration). This includes native preset system instructions, live
provider/model inheritance, explicit stronger inference override, forbidden
write dispatch under an auto-approving parent, a successful parent write
positive control, completed task snapshots, durable intent/result/task IDs,
child cancellation, and external-runtime refusal. The targeted agent
`max_turns_survives_retained_followups_and_settles_durably` integration test also
passed. No paid inference or external CLI smoke test was performed.

Strict clippy is blocked by baseline warnings in `execution_foreground.rs`
(`collapsible_if`), `execution_jobs.rs` (`let_and_return`),
`execution_output.rs` (`collapsible_if`), and `task_render.rs`
(`assign_op_pattern`). The TUI all-target no-dependency clippy check passes with
only `assign_op_pattern` excluded. Agent library no-dependency clippy also
passes with only `collapsible_if` and `let_and_return` excluded. Those unrelated files and the temporary terminal typo fix are excluded
from this follow-up delta. Root owns generated product reference/inventory
updates for the two additional commands and full product/PTY verification.

## Composition regression follow-up

The default-world user preset override failed because core contribution
ownership treated a fallback and an ordinary preset as the same exclusive row.
Built-in presets now contribute `agent_preset_fallback`, matching the registry's
separate fallback map; ordinary `agent_preset` ownership and collision checks
remain unchanged. The real file-authored CLI fixture proves user `reviewer`
shadowing, independent inventory attribution, fallback restoration after file
removal/reload, and cleanup on shutdown.

The TUI now accepts an optional subagent registry. Its inspection commands
remain in the catalog with a visible unavailable reason if their native runtime
or preset is absent; direct execution without the service refuses before any
history is created. This preserves intentional minimal TUI profiles. The real
binary migration fixture passes and confirms no subagent plugin was inserted.

Validation on root baseline `fad83e6`: both requested CLI fixture tests passed;
`cargo test -p heycode-core -p heycode-tui --quiet` passed 381 tests (73 core, 308 TUI),
including unavailable command state and the native inspection execution tests.
Root owns reference/inventory regeneration for the three renamed fallback rows.

Strict `cargo clippy -p heycode-core -p heycode-tui -p heycode-cli --all-targets -- -D
warnings` passed without allowances. The broad run including agent test targets
identified three pre-existing warnings in execution_jobs/native_runtime/subagent
tests; root fixes those in `f9fc776`, outside this delta. Formatting and diff
whitespace checks passed.
`cargo clippy -p heycode-agent --lib -- -D warnings` also passed without allowances.
