# Phase 2 native-tool completion audit

Date: 2026-09-11 (Australia/Melbourne)

## Result and evidence boundary

This pass completes the local implementation work for `P2-T-EnterPlanMode`, `P2-T-ListMcpResourcesTool`, `P2-T-ReadMcpResourceTool`, `P2-T-WaitForMcpServers`, and the additional operations required by `P2-T-LSP`. It also verifies the existing local artifact, notebook, file, shell, monitor, question, skill, schedule, web, workflow, and plan-mode contracts. It does **not** mark the Phase 2 tool programme, the mandatory Claude UI comparison, or live third-party/provider coverage complete.

The comparison source was the current official [Claude Code tools reference](https://code.claude.com/docs/en/tools-reference), read on 2026-09-11, together with the acceptance boundaries already recorded in `docs/terminal-compatibility-requirements.md`. Reference names do not by themselves justify a fake local alias: hosted delivery, account, phone, feedback, remote-trigger, and onboarding products remain integrations until dshx owns a real lifecycle for them.

Disposition meanings:

- **Implemented; UI open**: a real model-facing dshx tool is composed and controlled behavior passed, but the applicable Claude/dshx interaction and visual-state gate remains open.
- **Implementation present; controlled verification pending**: a coordinated owner added the model-facing contract in the current tree, but this pass could not complete its package-level test gate.
- **Implemented local equivalent; semantic/UI open**: the local capability is real, but Claude's contract or product boundary differs.
- **Partial; applicable follow-up**: an adjacent implementation exists, but required semantics are still missing.
- **Applicable gap**: no adequate model-facing local counterpart exists yet.
- **Deferred integration**: adopting the reference behavior requires a separately designed hosted, account, delivery, or remote service and explicit authorization.
- **Not applicable on this host**: the platform-specific implementation is intentionally absent from the current macOS build.
- **Excluded scope**: owned by the coordinated agent/work-task or context/cache streams, not this tool pass.

No live Anthropic inference, credentialed search provider, external send, hosted routine, or Claude UI operation was run in this pass. A later isolated compatibility canary exercised Apple clangd 17 and the installed open-source XcodeBuildMCP 2.7.0 package through public dshx services; see `docs/audits/real-compatibility.md`. The remaining stdio MCP and LSP tests use real child processes and real wire protocols but controlled fixtures. None of this package/service evidence is visual parity.

## New model-facing contracts

### Durable plan entry

`enter_plan_mode` is now an effect-owned model tool over the same durable `PlanMode` service as the human command. It accepts no arguments, is idempotent, commits the real mode transition, and causes later mutating tools in the same model batch to be refused. Shutdown removes both model-facing plan tools from a separately held registry. The existing `exit_plan_mode` review/approval path is unchanged.

### MCP resource discovery, reading, and readiness

The MCP connection owner now registers three session-local tools even when no servers are configured:

- `list_mcp_resources` lists bounded server summaries or one exposed server's committed, generation-checked resource page.
- `read_mcp_resource` requires a valid configured server id, a ready connection, explicit resource exposure, and an exact resource URI. Text returned to the model has one 64 KiB preview budget; binary bodies are represented by metadata and omitted.
- `wait_for_mcp_servers` performs a cancellation-aware readiness wait capped at 30 seconds and distinguishes `no_servers`, `all_ready`, `settled`, and `timed_out`. Authentication-required, degraded, and failed servers are never reported as ready.

The tools observe the live connection/resource registries rather than creating a second client. Bindings and tool registrations are token/effect-owned and disappear at connection or composition teardown. Server ids, page sizes, continuation generations, selections, URI payloads, and model-visible output are bounded. MCP-derived data is marked untrusted. Discovery/wait remain truthfully available with zero servers; resource reading advertises an unmet prerequisite until at least one configured server exposes resources, while readiness and URI authorization are still checked at invocation.

### Unified LSP navigation

The canonical `lsp` tool now supports these operations over one configured server registry:

- `servers`
- `definition`
- `references`
- `hover`
- `implementations`
- `document_symbols`
- `workspace_symbols`
- `incoming_calls`
- `outgoing_calls`
- `diagnostics`

The four earlier tools (`lsp_servers`, `lsp_definition`, `lsp_references`, and `lsp_diagnostics`) remain registered for compatibility. The canonical tool is unavailable when no server is configured; server listing remains discoverable separately. All path-taking operations stay inside the configured workspace, use zero-based UTF-16 positions, and launch exact stdio definitions through the common subprocess/sandbox owner. Locations, symbols, hover text, call edges, documents, frames, and result counts have explicit limits. Out-of-workspace or malformed server URIs fail as protocol errors, unsafe controls are refused, large model output uses the retained-output service, and server content is marked untrusted.

The exact stdio integration fixture exercises initialize plus every added request (`textDocument/hover`, `textDocument/implementation`, `textDocument/documentSymbol`, `workspace/symbol`, `textDocument/prepareCallHierarchy`, `callHierarchy/incomingCalls`, and `callHierarchy/outgoingCalls`). It also retains cancellation and descendant-process teardown checks. A production Apple clangd 17 canary additionally found and closed document-synchronization, empty optional-field, and pushed-diagnostic compatibility gaps. Definition, references, hover, implementations, document/workspace symbols, incoming calls, and diagnostics pass; that Apple build returns method-not-found for outgoing calls despite its broad call-hierarchy capability and dshx recovers after the fixed protocol error. Other production servers remain untested.

## Per-tool disposition

| Tracker row | Disposition | Current evidence and remaining boundary |
|---|---|---|
| `P2-T-Artifact` | Implemented local equivalent; semantic/UI open | `artifact` performs bounded, revision-checked local register/list/preview/remove and never uploads or deploys. Controlled mutation/revision/retirement tests pass. Hosted artifact publishing and paired UI remain separate. |
| `P2-T-AskUserQuestion` | Implemented; UI open | Blocking and asynchronous question infrastructure is real; reopen, cancellation, exact-once answer, and pending-state tests pass. Pending/answered/cancelled/resumed Claude UI comparison remains open. |
| `P2-T-Bash` | Implemented; UI open | Foreground shell, timeout/tree teardown, non-zero results, bounded visible tail, retained output, background promotion, and terminal ownership have controlled coverage. Paired approval, interruption, streaming, long-output, and background UI remain open. |
| `P2-T-CronCreate` | Implemented local contract; UI open | Local five-field cron is implemented with local timezone, recurring/one-shot rules, deterministic jitter, seven-day recurring expiry, bounded admission and durable replay. Scheduler owner reports 9 domain, 8 agent schedule and 11 migration tests passing; actual CLI and live Claude presentation remain open. |
| `P2-T-CronDelete` | Implemented local contract; UI open | Deletion now settles an already admitted unclaimed recurring inbox occurrence. Focused schedule regression covers this race; actual CLI and paired terminal comparison remain open. |
| `P2-T-CronList` | Implemented local contract; UI open | Composed list receipts now expose cron expression, rule, timezone, expiry, jitter and resume behavior. Native composed create/list/delete test passes; actual CLI and paired terminal comparison remain open. |
| `P2-T-Edit` | Implemented; UI open | Read-before-write, stale-file refusal, exact/replace-all edits, multi-line diff, provider dispatch, and atomic filesystem behavior pass. Final Claude/dshx card, approval, error, cancellation, narrow/wide and theme evidence remains open. |
| `P2-T-EndConversation` | Deferred adoption | The report makes this low priority and conditional on adoption. No model-facing session-end contract is adopted in this phase. Turn completion, foreground-owner shutdown and resumable-conversation termination remain distinct; human quit/cancellation do not silently discard other work. |
| `P2-T-EnterPlanMode` | Implemented; UI open | New durable, idempotent, effect-owned tool passes direct and same-batch mutation-guard tests. Live model invocation and plan-mode presentation comparison remain open. |
| `P2-T-EnterWorktree` | Implemented; UI open | The no-argument mutating model tool creates a retained detached worktree, preserves the source checkout/index, and rebinds current-session file roots, shell cwd, and instruction scope. The production transition suite passes 7/7, the service suite passes 12/12, and the bounded snapshot test passes 1/1. Paired UI and recovery-path presentation comparison remain open. |
| `P2-T-ExitPlanMode` | Implemented; UI open | Existing proposal/review/feedback/accept/reject/cancel/reopen and mutation guard suite passes. Paired live presentation and interaction remain open. |
| `P2-T-ExitWorktree` | Implemented; UI open | The no-argument mutating model tool restores the saved cwd/root authority and explicitly retains the worktree without merging, deleting, or discarding it. The production transition suite passes 7/7, the service suite passes 12/12, and the bounded snapshot test passes 1/1. Paired UI and retained-result recovery comparison remain open. |
| `P2-T-Glob` | Implemented; UI open | Recursive bounded traversal now publishes globally sorted results before applying the limit, skips ignored/vendor paths, reports partial traversal, and passes wide/cancellation/symlink policy coverage. Dedicated Claude Glob UI was not available in the captured reference session. |
| `P2-T-Grep` | Implemented local surface; semantic/UI open | Regex/include filtering, bounded excerpts, binary/oversized-line handling, stable ordering, partial-search notice, cancellation, and provider dispatch pass. Richer reference-specific context/output flags and dedicated Claude UI remain unverified. |
| `P2-T-ListMcpResourcesTool` | Implemented; UI open | New bounded server/resource listing passes an actual MCP stdio fixture, multi-page local continuation, stale-generation refusal, identifier bounds, exposure, untrusted provenance, empty-configuration availability, and teardown ownership. An isolated XcodeBuildMCP 2.7.0 canary reached ready and listed its four actual resources. Broader third-party servers, pagination UI, and visual states remain open. |
| `P2-T-LSP` | Implemented; broader production-server/UI open | Canonical consolidated tool and all ten operations pass the model-tool boundary and exact stdio fixture. Apple clangd 17 passes eight applicable navigation/diagnostic operations through the public service; its outgoing-call method is server-unsupported and returns a fixed recoverable protocol error. Other production servers and Claude/dshx UI evidence remain open. |
| `P2-T-Monitor` | Implemented local equivalent; semantic/UI open | Existing `monitor` passes filtered, deduplicated live-event delivery and cancellation/source-ownership tests. Its supported source families differ from any hosted reference monitoring surface; paired UI remains open. |
| `P2-T-NotebookEdit` | Implemented; UI open | Actual nbformat 4 replace/insert/delete, metadata preservation, code-output clearing, revision/cell-id conflict, cancellation, invalid-cell, and external-change tests pass. It does not execute cells. Live notebook card/error comparison remains open. |
| `P2-T-PowerShell` | Not applicable on this host | The current product target is macOS and uses the generic shell/terminal contracts. Do not advertise a native PowerShell adapter until Windows is a supported and tested target. |
| `P2-T-PushNotification` | Deferred integration | No phone/host notification delivery service or consented destination lifecycle is composed. Local durable reminders do not prove push delivery. |
| `P2-T-Read` | Implemented; UI open | Default and explicit paged reads, continuation revisions, byte/line caps, UTF-8 long-line reconstruction, binary refusal, batch reads, provider dispatch, and observation logging pass. Rich-file discoverability and paired UI remain open. |
| `P2-T-ReadMcpResourceTool` | Implemented; UI open | New ready/exposure-gated read passes actual stdio resource reads with a 64 KiB aggregate UTF-8 preview, binary omission, hostile identifier refusal, cancellation settlement, body-free transport errors, untrusted provenance, and owned teardown. The XcodeBuildMCP 2.7.0 canary reads inert JSON session status. It also exposed and drove a fix for exact committed-list URI enforcement before dispatch. Other resource types and UI remain open. |
| `P2-T-RemoteTrigger` | Deferred integration | Durable local schedules are not authenticated hosted triggers. This needs remote identity, authorization, availability, replay, rate-limit, and revocation design. |
| `P2-T-ReportFindings` | Implemented; live UI parity gate open | `report_findings` now rechecks read-tool revisions and maximum referenced lines in one pinned workspace generation, derives session/workspace/root/cwd identity from the live owner, and durably commits bounded severity/location/trigger/failure/impact records before a typed UI event. Unsafe paths, malformed/stale revisions, controls, exact duplicates, foreign cwd, and cancellation are refused without publication. The coordinated TUI passes live/replay, compact/expanded, keyboard/mouse, and accessibility tests; paired current-Claude captures and a live model invocation remain open. See `report-findings-audit.md`. |
| `P2-T-ScheduleWakeup` | Implemented local contract; UI open | Explicit local wakeup admission plus schedule_wakeup reschedule/stop is implemented with bounded delays, expiry and cancellation of admitted unclaimed occurrences. Wakeups do not restore on resume and no implicit Claude twenty-minute fallback is claimed; actual terminal evidence remains open. |
| `P2-T-SendFeedback` | Deferred integration | No consented draft/review/send destination is composed. Nothing was sent externally in this pass. |
| `P2-T-SendUserFile` | Partial; applicable follow-up | Local `artifact` handles and attachment infrastructure are adjacent, but there is no explicit user-delivery card/lifecycle. Remote delivery is a separate integration. |
| `P2-T-ShareOnboardingGuide` | Deferred integration | Hosted guide generation/sharing is not an agent-runtime blocker and has no local product owner or authorized destination. |
| `P2-T-Skill` | Implemented; UI open | `load_skill` remains independently discovered, bounded, and model-facing with registry tests. Cross-product naming, errors, and live presentation remain open. |
| `P2-T-TodoWrite` | Migrated; UI open | Current composition no longer advertises todo_write; builtins registry asserts its absence. Session work projection migrates successful historical todo_write/mcp__dshx__todo_write results without rewriting the journal, with legacy and Unicode/control-input regressions. Structured-work presentation remains under the task tool UI gates. |
| `P2-T-ToolSearch` | Existing capability; expansion deferred | Existing tool_search remains in the unchanged dshx-code-mode crate. New search-only/custom/MCP discovery architecture is explicitly deferred by the latest report scope (lines 345 and 478); no new expansion is claimed. |
| `P2-T-WaitForMcpServers` | Implemented; UI open | New bounded wait passes ready, zero-server, timeout, terminal-settlement, disappearing-server, selected-id validation, cancellation, and owned teardown behavior. The XcodeBuildMCP 2.7.0 process reaches `all_ready` through the public model tool. Authentication/failure are non-ready by construction; real reconnect/auth journeys and UI remain open. |
| `P2-T-WebFetch` | Implemented; provider/UI open | Registry dispatch, bounded HTML/PDF extraction, citations/source metadata, redirect limits, SSRF/DNS rebinding policy, cancellation, and UTF-8-safe output limits pass controlled suites. No credentialed production-provider or paired Claude UI canary was run here. |
| `P2-T-WebSearch` | Implemented routing; provider/UI open | Portable/provider registry selection, domain policy, private-address filtering, normalization, and selected native-route tests exist and pass in their scoped suites. This pass did not establish current live native execution, citation quality, or all-provider capability gating. |
| `P2-T-Workflow` | Implemented local equivalent; semantic/UI open | Opt-in `workflow`/`run_code` workers pass progress, cancellation, durable checkpoint, and resume tests. Workflow, agent, and structured Work identities remain deliberately distinct. Paired live UI remains open. |
| `P2-T-Write` | Implemented; UI open | Create/parent creation, observed overwrite, stale/new-target refusal, provider dispatch, line receipts, and atomic filesystem behavior pass. Approval, exact visual diff/card, cancellation, and theme/size comparison remain open. |

The following rows were explicitly outside this pass and must be read from their coordinated owners: `P2-T-Agent`, `P2-T-ListAgents`, `P2-T-SendMessage`, `P2-T-TaskCreate`, `P2-T-TaskGet`, `P2-T-TaskList`, `P2-T-TaskOutput`, `P2-T-TaskStop`, and `P2-T-TaskUpdate`. Context reduction, recovery, provider caching, and cache-cost claims were also excluded. No conclusion here overrides their separate evidence or status.

## Controlled verification completed

All commands ran from `/Users/naresh/Work/Personal/dshx` against the concurrent working tree; no commit, push, deployment, migration, external send, or destructive reset was performed.

- `cargo test -p dshx-exec -p dshx-tools -p dshx-mcp -p dshx-web --all-targets`
  - `dshx-exec`: 22 unit + 83 integration passed (the opt-in production canary remains ignored by default).
  - `dshx-tools`: 89 unit passed, 7 explicitly environment-gated tests ignored, 14 integration passed.
  - `dshx-mcp`: 11 unit + 360 integration passed.
  - `dshx-web`: 31 unit + 20 integration passed.
- `cargo test -p dshx-exec --test main lsp_service -- --nocapture`: 5 passed, including the all-operation exact stdio fixture and hostile-shape/body-free error coverage.
- `cargo test -p dshx-tools --test main lsp -- --nocapture`: 1 passed, covering unified model dispatch and retained output.
- `cargo test -p dshx-mcp --test main model_resource_tools -- --nocapture`: 4 passed against actual MCP stdio fixtures, including pagination, truncation, binary omission, cancellation, teardown, invalid input, and zero-server composition.
- `cargo test -p dshx-mcp model_tools -- --nocapture`: 2 passed for timeout, cancellation, failed settlement, ready transition, disappearing-server classification, and non-exposed resource refusal.
- `cargo test -p dshx-agent --test main plan -- --nocapture`: 27 passed.
- `cargo test -p dshx-agent --test main schedules -- --nocapture`: 3 passed.
- `cargo test -p dshx-agent --test main questions -- --nocapture`: 2 passed.
- `cargo test -p dshx-agent --test main workflows -- --nocapture`: 3 passed.
- `cargo test -p dshx-session --test main review_domain --no-fail-fast`: 6 passed, including direct-report replay and malformed source/finding refusal.
- `cargo test -p dshx-agent --test main reviews --no-fail-fast`: 5 passed, including revision/source ownership, commit-before-presentation, and zero-publication refusal paths.
- `cargo test -p dshx-cli --test main default_world_reports_exact_live_inventory -- --nocapture`: 1 passed with `report_findings` in the actual model tool registry.
- `cargo test -p dshx-tui finding_report -- --nocapture`: 3 passed for typed live/replay cards, bounded compact/expanded rendering, interaction, and accessibility.
- `cargo test -p dshx-session --all-targets --no-fail-fast`: 240 tests passed across unit, background, and integration targets.
- `cargo test -p dshx-agent --all-targets --no-fail-fast`: 435 tests passed; one explicitly ignored two-minute live timing test was not run.
- `cargo clippy -p dshx-session --all-targets -- -D warnings`: passed. Strict affected-crate clippy remains blocked by unrelated warnings in concurrently owned agent files; the agent target passes after isolating those four pre-existing warning classes, and the review implementation/tests add no remaining warnings.
- `cargo test -p dshx-agent --test main monitor_delivers_filtered_deduplicated_events_while_source_is_alive -- --nocapture`: 1 passed.
- `cargo clippy -p dshx-exec -p dshx-tools -p dshx-mcp -p dshx-web --all-targets -- -D warnings`: passed.
- `DSHX_REAL_LSP_CLANGD=/usr/bin/clangd cargo test -p dshx-exec --test main installed_clangd_serves_navigation_through_the_public_service -- --ignored --nocapture`: 1 passed against Apple clangd 17.0.0.
- `DSHX_REAL_MCP_NODE=/opt/homebrew/bin/node DSHX_REAL_MCP_XCODEBUILDMCP=<installed-cli.js> cargo test -p dshx-mcp --test main installed_xcodebuildmcp_resources_cross_the_public_model_tools -- --ignored --nocapture`: 1 passed against XcodeBuildMCP 2.7.0.
- `cargo test -p dshx-cli --test workspace_transitions --no-fail-fast`: 7 production-composition transition tests passed.
- `cargo test -p dshx-agent --lib workspace_transition --no-fail-fast`: 12 service/unit transition tests passed.
- `target/debug/deps/dshx_agent-af8736dada464cb3 worktree_snapshot --nocapture`: 1 bounded-snapshot test passed using the executable built by the successful agent test target.

## Mandatory gates still open

The per-row UI matrix remains open. For every applicable implemented or partial row, capture the real model request and tool transcript, then compare summary, arguments, pending, working, success, empty, error, permission, cancellation, grouping, long-output expansion, keyboard, mouse, narrow/wide, dark/light, and no-color behavior against current Claude. Additional production LSP and third-party MCP servers require a compatibility matrix beyond the two isolated canaries. Credentialed web-provider selection and citations require provider-specific canaries. Hosted/deferred integrations require explicit product design and authorization before any external operation.

Therefore this audit closes the owned local contract-and-controlled-test pass only. It is not evidence that dshx has complete Claude parity or that Phase 2 is engineering.
