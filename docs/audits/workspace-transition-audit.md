# Current-session workspace transitions — 2026-09-11

Scope: `P2-C-add-dir`, `P2-C-cd`, `P2-T-EnterWorktree`, and `P2-T-ExitWorktree`, plus the shared quiescence API needed by current-session recomposition. Changes were made directly in the existing checkout without commits or changes to the central parity classifications.

Final command closure: the adopted session-scoped `/add-dir` contract is complete with [its final dialog audit](add-directory-dialog.md), and `/cd` is complete with [its actual paired command evidence](workspace-terminal-evidence.md#final-cd-interaction-comparison-and-verdict). The historical shared implementation checks below remain evidence for both commands and worktree tools. The outstanding paired worktree-tool UI and staged live-provider pilot apply specifically to `P2-T-EnterWorktree` and `P2-T-ExitWorktree`; they do not reopen the completed command rows.

Later worktree capture: [the paired tool-card audit](worktree-tool-terminal-evidence.md) records both actual clients completing entry, nested-entry refusal, retained exit and inactive-exit refusal through localhost provider fixtures. It narrows the UI remainder to native success summaries that currently display trailing JSON instead of destination/retention; the staged live-provider pilot remains separate.

## Implemented behavior

`/add-dir <path>` grants a canonical filesystem root without loading its project instructions, skills, hooks, plugins or profiles. `/cd <path>` moves the real current-session cwd only within an already granted root. Paths containing spaces are supported as a single literal path argument. The path is not shell-expanded.

`EnterWorktree` / `enter_worktree` and `/worktree enter` capture the current Git state into a unique detached managed checkout. Tracked changes are applied without touching the source index. Untracked regular files and symlink contents are copied from a bounded snapshot (64 MiB, 10,000 entries); capability directories refuse symlink traversal in source and destination parents. Before publication, HEAD, tracked patch, complete untracked manifest and captured file contents are rechecked. The Git top-level must itself be within authorized roots, and worktree storage must be disjoint from the whole repository.

`ExitWorktree` / `exit_worktree` and `/worktree exit` restore the exact previous cwd and roots. All created worktrees, including clean ones, are durably retained. These operations do not merge results, change the source branch, delete a worktree, or accept force/delete arguments. A failed attempt after journaling leaves the previous scope active with visible pending recovery. `/worktree recover` acknowledges that attempt while retaining its files; `/worktree status` reports the current scope and retained locations.

## Authority and production consumers

The session's private atomic `workspace.json` sidecar records the revision, canonical directory identities, roots, active worktree/return scope, retained paths and pending recovery. Resume checks the original root and the exact sandbox mode; unavailable or replaced directory identities fail closed. A new filesystem generation invalidates prior read observations before edits can use the new scope.

The production `workspace-scope` plugin publishes stable forwarding filesystem and shell services before tool consumers are built. The late `workspace-transitions` plugin installs real human commands and model tool barriers. Agent tool contexts, prompt environment, native runtime and protocol session information use the current cwd. Trusted project instruction text is refreshed at its existing prompt section position. Native child guidance is pinned at spawn; delegated child launches use the admitted parent cwd. New terminal launches obtain the current scoped shell executor. The TUI updates cwd, session-browser context, welcome information and repository probes; voice activity holds a workspace lease until its process settles.

Transitions fence the native turn, job admission, resumable/archived child handles, terminal admission and retirement, and native protocol ownership. Running file/shell operations and auxiliary voice activity also pin authority. A model transition is admitted only at the exact native foreground worktree-tool barrier; direct model-origin calls and child/workflow/delegated callbacks cannot change the parent workspace.

`Agent::acquire_recomposition_permit()` holds the same live-owner fences for the coordinator's `/reload-plugins` and `/tui` lifecycle. Dropping the permit reopens admission. `begin_shutdown()` permanently closes old agent/job/child/terminal/workspace/protocol admission before releasing the turn gate for asynchronous teardown. Session close remains available after protocol admission shuts down.

## Explicit limits

- Outside-root `/add-dir` and managed worktree entry require an explicitly configured full-access session. The current restrictive process sandbox supports one write root and cannot safely authorize both source Git metadata and disjoint worktree storage. It refuses entry before Git/journal mutation. `/cd` within an existing authorized root remains available.
- Transitions refuse configured LSP/MCP servers, hooks, fixed-base worktree providers, loaded project skills and project agent/hook/plugin declaration directories whose state remains bound to the original composition. A fresh composition is required for those cases. User-only skills remain available; a project-root skill reload refuses a stale cwd binding.
- Filesystem grants do not provide OS confinement in full-access mode. Shell commands remain subject to the configured process sandbox; changing their default cwd does not restrict what full-access commands can read.
- The worktree changes provide working native behavior and controlled-test evidence. Their paired tool-card UI and live-provider validation remain separate outstanding checks. The later command-specific closure evidence is linked above; no pixel-identity claim is made.

## Validation

The production suite passed **7/7** tests in 2.27 seconds; the transition unit suite passed **12/12** in 0.48 seconds; the bounded-snapshot test passed **1/1** in 0.02 seconds using the same freshly compiled agent test executable. All Git and terminal execution uses disposable local fixtures, and inference uses an injected scripted provider through the real production composition. Targeted Rust formatting and diff whitespace checks also passed.

Test targets and logs:

- `cargo test -p dshx-cli --test workspace_transitions --no-fail-fast` — `tmp/terminal-evidence/workspace-transition-production.log`.
- `cargo test -p dshx-agent --lib workspace_transition --no-fail-fast` — `tmp/terminal-evidence/workspace-transition-unit.log`.
- `target/debug/deps/dshx_agent-af8736dada464cb3 worktree_snapshot --nocapture` — `tmp/terminal-evidence/workspace-transition-snapshot-direct.log`. This uses the executable produced by the preceding successful agent test target; the redundant Cargo invocation waiting for another worker's build lock was cancelled.

The CLI suite covers actual `/cd` and `/add-dir` authority, refreshed provider-visible guidance, current observation diagnostics, dirty Git worktree entry/exit, a real model `EnterWorktree → bash pwd → ExitWorktree` loop, job/terminal/activity refusals, new model terminal cwd, protocol cwd, and recomposition abort/shutdown cleanup. Unit fixtures cover durable resume, retention/recovery, failed journals, identity replacement, in-flight shell/file ownership, exact sandbox-mode drift, unauthorized ancestor repository refusal, and argument validation. The snapshot test covers bounded file capture, executable mode preservation, symlink-content preservation, and refusal of source/destination symlink traversal.

Follow-up memory composition correction: the production memory plugin now receives `options.sessions_dir` for persistent custom-agent memory and retains `credentials_root` for user instructions. The real-composition test seeds different content for the same preset under both roots and verifies that the manager reads the actual session-root memory. The full production target passed again, **7/7** in 1.66 seconds; log: `tmp/terminal-evidence/workspace-transition-memory-root-regression.log`.
