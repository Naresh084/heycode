# Phase 2 session rename

`/rename [title...]` now accepts a typed title or derives a local title from the first nonempty user message (up to twelve words). Empty conversations use `Untitled session`. No provider request is made. Typed titles and browser rename edits share trim/control normalization with preserved internal ordinary spaces and a 200-character Unicode-safe limit.

The session query owner picks the first available title variant among visible saved sessions, excluding the renamed session itself: `Name`, `Name (2)`, `Name (3)`. A store-wide file lock serializes rename decisions across independent store owners. Existing creation titles and legacy automatic titling are not a globally unique namespace; the guarantee covers these rename operations. Archived titles do not reserve active names. A suffix stays inside the 200-character bound. Both current-handle and saved-session rename use the same decision. The UI shows the committed variant rather than the requested base name.

The only durable change is a title event. Conversation events remain unchanged. The original strict `SessionTitle::new` validator remains available for callers with already-normalized input. The slash command remains queued and does not interrupt a foreground inference turn.

Verification on 2026-09-11:

- `tmp/rename-session-test-20260911T073051Z-29ab27.log`: seven lifecycle checks passed, including two independent concurrent store owners, stable rerename, Unicode limit/suffix, normalization, and unchanged original conversation events.
- `tmp/rename-browser-test-20260911T073046Z-e55164.log`: three unit and ten integration checks passed, including no-argument rename and the actual committed duplicate-name UI message.
- `tmp/rename-session-full.log`: 75 unit and 150 integration checks passed.
- `tmp/rename-session-clippy.log`: strict session clippy passed.

An earlier run exposed a lock-order issue: querying titles after taking the target's exclusive lifecycle lock blocked on reading that same target. The corrected order holds the store mutation lock, resolves the name, then acquires the target lifecycle lock before appending. Both the existing lifecycle suite and the concurrent-owner regression pass after the correction.

The CLI command inventory expectation uses `/rename [title...]`. Local deterministic title generation is a deliberate dshx equivalent and is not a claim to reproduce Claude's model-generated title wording.

## Reconciled typed-command evidence, 2026-09-12

The existing actual-terminal captures were inspected independently in this pass:

- Claude typed rename (local-only evidence: `tmp/terminal-evidence/command-reference-claude-20260911T074219Z-482a2c/01-rename.png`) and its result (local-only evidence: `tmp/terminal-evidence/command-reference-claude-20260911T074219Z-482a2c/result.json`).
- Native typed rename with command transcript (local-only evidence: `tmp/terminal-evidence/command-reference-shared-20260911T084917Z-4f8469/01-rename.png`) and its result (local-only evidence: `tmp/terminal-evidence/command-reference-shared-20260911T084917Z-4f8469/result.json`).

Both show the submitted command, the preserved internal spaces in `Parity   command session`, a completed rename receipt, and the same title in the session footer. This closes the typed-command presentation subcase. The earlier native `command-reference-dshx-20260911T082139Z-a28ba6` capture lacks the subsequently added command transcript line; use the shared-v1 capture for that comparison. The recorded journeys sent no model prompt. The native actual renderer/reload journey also proves that the committed title survives same-session recomposition; see [recomposition evidence](recomposition.md).

The installed source also advertises `/name`. The missing native alias has now been added to the central descriptor table, keeping the same queued handler and availability. Current focused browser verification still passes the no-argument derived title and resolved duplicate-title receipt. Alias registry verification and the coordinator's final row disposition are recorded in [the lifecycle review](session-command-acceptance-review.md). No new source session, provider request or worktree journey was run in this reconciliation.
