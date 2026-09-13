# Phase 2 advisor source evidence and native design

## Disposition

`P2-C-advisor` is **implemented and controlled-acceptance verified**. The
portable agent core, persistent settings/control, production composition,
interactive picker, AppState/render bridge, command migration, exact-route
consultation and same-parent-turn continuation are mounted. This is not a
commercial-provider validation claim: provider transport was exercised only
through a deterministic localhost fixture with a dummy credential. The
previously shipped dshx behavior was not the Claude command's behavior:

- Claude `/advisor` selects, disables, and persists a model that the main model
  may consult at key moments before resuming its own turn.
- dshx `/advisor [instructions...]` immediately starts a fresh read-only native
  child and commits that child's answer as a complete standalone parent turn.

The existing dshx operation is useful and verified. It should move to
`/ask-advisor [instructions...]`; `/advisor` should become the persistent
control surface described below. Free-form legacy input must get a migration
error pointing to `/ask-advisor`, not be guessed as a provider or model.

## Controlled source evidence

### Claude Code 2.1.268

The capture at
`tmp/terminal-evidence/claude-advisor-reference-20260911T115150Z-a6275e/result.json` used a disposable
home, config directory, workspace and session. Inherited credentials were
removed. All provider traffic was forced to a deterministic server on
`127.0.0.1`; no normal conversation prompt was submitted.

The packaged experimental command was enabled with
`CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL=1`. The capture proves:

- the command palette describes `/advisor` as “Let Claude consult a stronger
  model at key moments”;
- bare `/advisor` opens an “Advisor (experimental)” picker containing Fable
  5.1, Opus 5, Sonnet 5 and No advisor;
- the explanation says Claude escalates for stronger judgment, then resumes,
  and that the advisor consumes additional tokens;
- `/advisor off` reports `Advisor disabled`;
- `/advisor opus` sends one loopback-only validation request with
  `model=claude-opus-5` and `max_tokens=1`, then reports
  `Advisor set to Opus 5`;
- a new process reading the same disposable user settings selects
  `Opus 5 ✔`, proving persistence.

The captured executable is Claude Code 2.1.268 with SHA-256
`06a96d5423f83770f120859f1c58e60d7252cc4c122aa13043b7e7cd716bc76a`.
The only provider request was the loopback validation request. This proves the
command/control presentation and persistence; it does not claim an actual
advisor inference because no normal prompt was sent.

### Pre-change dshx baseline

The capture at `tmp/terminal-evidence/dshx-advisor-20260911T115534Z-6d85c4/result.json` ran the
real immutable dshx CLI at SHA-256
`25526dce041310f5ccf263a406ef0717afeed43616a343ecd6dc8a278fce5010`
against a disposable home/workspace and an OpenRouter-shaped loopback fixture.
There were no external requests or operator credentials.

One `/advisor choose the safer persistence boundary for the current advisor
design` command produced exactly one provider request. The request used the
current main route's `z-ai/glm-5.3-flash` model, included the question and
advertised seven child tools. The parent journal contains the literal slash
command, a complete assistant answer and a durable child task ID. Two session
logs exist: parent and child. The held and completed screenshots show a normal
scheduled child named `advisor`; there is no picker, toggle or persisted advisor
selection.

Source inspection agrees with the wire capture:

- `human_commands.rs` registers `/advisor [instructions...]` as a
  model-scheduling `InspectionCommand`;
- `Agent::run_native_inspection` uses `SubagentSeed::Fresh`,
  `SubagentContinuation::OneShot`, native child execution and a read-only
  permission ceiling;
- an explicit preset inference provider/model/effort wins, otherwise the child
  snapshots the current main route, model and effort;
- the built-in advisor preset owns read, glob, grep and LSP inspection tools;
- the result becomes a complete assistant message in a new parent turn.

That is an explicit one-shot inspection workflow, not a persistent advisor tool.

### Implemented dshx production lifecycle

The final controlled capture at
`tmp/terminal-evidence/advisor-lifecycle-20260911T193428Z-f74f83/result.json` ran the production CLI
at 80×24 from immutable binary SHA-256
`b1ea2c1501db8cdbadfb1775246c90b7f975b659e14de7e9142198dc64c2d0f8`.
It used a disposable `DSHX_HOME` and workspace, a dummy key, and an
OpenRouter-shaped server bound to `127.0.0.1`. The result records zero external
provider requests and all contract checks passed.

The journey proved that advisor starts disabled, exact
`native:openrouter + local/advisor-model` selection persists without inference,
survives a process restart, can be disabled and survives a second restart, and
can then be reselected. One ordinary parent prompt produced exactly three
localhost requests in route order:

1. `local/main-model` exposed the parameterless `advisor` tool;
2. `local/advisor-model` received the current parent question and zero tools;
3. `local/main-model` received the advisor guidance as its tool result and
   committed the final answer.

The parent journal contains exactly one `TurnStart` and one `TurnEnd` for this
cycle. PNG, text-cell, raw ANSI, request, settings and parent-session evidence
are retained beside the result. The reproducible driver is
`scripts/advisor_lifecycle_terminal_check.py`; it refuses to use an external
inference transport by construction.

## Adopted native contract

### One explicit route, never an inherited model

Persist an `AdvisorSelection` as one complete tuple:

```text
enabled
backend owner = native inference provider OR delegated runtime
model
optional reasoning effort
```

Use the existing `BackendControlOwner` distinction so a native provider and a
delegated subscription/runtime cannot share an ambiguous model string. The
selected advisor route is independent of the main route. Changing the main
provider or model must not silently change the advisor. There is no precedence,
fallback or “same model on whichever provider is active” behavior.

The settings schema should store a tagged owner plus exact model and optional
effort, for example `native:openrouter + anthropic/claude-opus-5 + high` or
`runtime:claude-code + opus + null`. Picker rows are grouped by connected owner
and show the owner on every model row. Catalog aliases resolve before commit;
the persisted value is the resolved route identity.

Selection is transactional and does not spend a provider request merely to
change a preference:

1. snapshot the settings revision and selected backend generation;
2. validate the exact native owner, model and effort locally against the
   connected provider, cached catalog and inference adapter;
3. atomically persist against the expected revision and publish the same live
   generation;
4. on any failure retain the previous durable and live selection exactly.

Opening, status, cancellation and `off` perform no provider request. A live
turn snapshots one advisor generation; route changes are unavailable while that
turn is active rather than producing mixed-route state.

### A main-model tool, not another human turn

When enabled, the next main request exposes one parameterless client tool named
`advisor`. Its description tells the main model to orient with its ordinary
tools first, call the advisor for difficult judgment, and then resume. When
disabled, the tool is absent rather than present-but-failing.

One accepted call:

1. reads the live advisor generation, which cannot change during the active
   parent turn, and enters the existing durable descendant admission path;
2. starts a one-shot native child with `SubagentSeed::ForkParent` so it receives
   the projected conversation, including the main model's orientation and tool
   results;
3. uses the exact advisor backend owner, model and effort from the snapshot;
4. exposes no child tools, no delegation and no advisor tool, preventing
   recursive escalation and additional unowned work;
5. returns the concise guidance as the original tool call's `ToolResult`;
6. lets the main model continue the same turn and produce the user-facing
   answer.

Keep the child session/task identity for audit and recovery, but distinguish it
as an advisor consultation in task metadata and UI. Cancellation, provider
failure and malformed output settle the original tool call explicitly; they do
not manufacture a successful assistant answer or retry on a different route.

Slash-command updates are rejected while a parent turn is active, and ambient
settings changes are restart-applied. Those two constraints make the live
generation stable for the whole request/tool cycle without a second route
ledger.

### Existing descendant admission and output guardrail

Advisor consultations use the existing `SubagentBudgetLimits` and durable
reservation path. This preserves one session-wide ceiling across every native
descendant instead of creating a second allowance that could disagree with the
actual dispatcher. `/advisor status` and `/advisor details` show the existing
reserved request count, configured session request limit and configured
per-request output cap; the normal picker keeps those technical facts out of
its primary choice hierarchy.

A zero request limit means unlimited descendant requests; a zero output limit
means the provider/model default. Those states are reported truthfully, not
presented as advisor-specific safety defaults. Operators that require an
explicit consultation output bound configure the existing descendant output
guardrail. Cancellation, failures and restart retain the existing budget's
admission semantics.

### Command and UI grammar

```text
/advisor                         load connected native catalogs and open picker
/advisor off                     disable persistently
/advisor status|details          textual route/generation/budget details
/advisor <owner> <model> [effort]
/ask-advisor [instructions...]   retain today's verified one-shot inspector
```

The picker mirrors the source information hierarchy while retaining dshx route
truth: an unboxed bottom surface, concise explanation, numbered
provider-qualified model rows, an explicit current mark, No advisor, and an
Enter/Escape footer. When disabled, No advisor is the selected default row.
Bare `/advisor` concurrently loads every connected native provider catalog with
`PreferCache`; this is catalog GET/discovery work, never inference. Loading,
stale fallback and provider-specific catalog failure remain visible. Escape
cancels the caller's wait and a cancelled completion cannot reopen the picker.
The exact committed route remains a row even if a refreshed catalog omits it,
so opening the control never silently changes selection.

### Anthropic server-tool coexistence

dshx already has a provider-owned Anthropic `advisor_20260301` definition with
model-pair validation and `max_uses`. It is restart-applied, Anthropic-only and
stores only an advisor model, so it cannot own the portable explicit-route
contract above.

Do not advertise both tools named `advisor` in one request. Portable advisor
enablement and the provider-owned Anthropic advisor setting must be mutually
exclusive at composition/selection with a precise remediation error. A future
optimization may map a proven same-Anthropic route to the server tool, but only
if it preserves the same route tuple, budget ledger, durable receipts and
failure semantics. It is not required for the first portable implementation.

## Implementation map

The completed implementation owns:

- `dshx-agent::advisor_plugin`: settings/control, exact native routing,
  conditional request exposure, provider-owned name conflict refusal and
  tool-backed one-shot child execution through the existing descendant budget;
- `dshx-tui::advisor_panel`: an isolated bridge, searchable provider-qualified
  picker, cancellable loading projection, semantic lines and unboxed bottom
  renderer;
- `human_commands::commands_with_advisor`: persistent `/advisor` grammar plus
  the `/ask-advisor` migration while preserving the old `commands` API;
- root CLI composition for `advisor_plugin` and `SERVICE_ADVISOR`, plus exact
  plugin/service/tool/settings/interception inventory coverage;
- TUI handle/AppState routing, render and screen-reader integration through
  `AdvisorPanelBridge`, with a bounded viewport that keeps deep selections
  visible at 80×24;
- concurrent connected-provider catalog refresh with explicit stale/error
  warnings, configured-default-only fallback, and committed-row preservation;
- focused unit and integration tests for strict settings/owner grammar,
  no-mutation precedence conflict, provider-owned advisor preservation and
  conflict, exact route, current-turn projection, same-turn continuation,
  no-child-tools, failure, cancellation, existing descendant budget, settings
  persistence and restart;
- the localhost-only production lifecycle driver and retained evidence above.

## Acceptance matrix

The requested implementation and controlled-validation slice passed:

- settings schema/default/CAS, exact owner-model-effort persistence, restart and
  failed-validation rollback;
- picker grouping, off/status/direct grammar, migration error and
  `/ask-advisor` preservation, plus status/details separation from the picker;
- disabled tool omission and enabled parameterless tool exposure;
- exact-route/no-fallback execution with parent projection and prior tool
  results visible to the advisor;
- child tool/delegation/advisor recursion denial;
- advisor result returned as a tool result and main-model continuation in the
  same turn;
- existing descendant request/output limits, restart persistence, queued
  cancel without spend, and post-dispatch cancel/failure without refund;
- settings-change exclusion during a live turn;
- Anthropic provider-tool name-conflict refusal;
- controlled production picker/off/enable/restart and same-turn native
  captures, paired with the Claude picker/off/enable/restart evidence and the
  80×24 viewport regression;
- full connected-catalog loading, partial failure, cancellation, committed-row
  preservation, bottom/unboxed visual structure and zero-inference picker
  regressions.

Provider failure, cancellation and budget exhaustion are deterministic
provider-seam integration tests rather than live commercial-provider tests.
Commercial subscriptions, their current catalogs, billing and provider-side
availability remain live-unverified. Provider-owned Anthropic advisor
serialization remains a separate provider capability and is not counted as
evidence for the portable slash-command contract.
