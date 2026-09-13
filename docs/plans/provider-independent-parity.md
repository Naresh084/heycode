# Current coding-agent feature and provider parity

Requested 2026-09-08. This supplements the 37-finding multiagent audit. A feature counts as implemented only when the heycode native path and its user controls work, with tests across the relevant protocol families. A Claude/Codex subscription adapter, unsupported error, or unmounted service is not completion evidence.

## Source baseline

Official pages fetched on 2026-09-08: [Claude Code changelog](https://code.claude.com/docs/en/changelog) lists 2.1.263 (September 6); [Codex changelog](https://learn.chatgpt.com/docs/changelog) lists CLI 0.153.4 (September 4). The broader feature inventory uses their [Claude documentation index](https://code.claude.com/docs/llms.txt) and [Codex documentation index](https://learn.chatgpt.com/docs/llms.txt), followed by specific feature pages. Search snippets alone are not the baseline. This is a dated functional inventory, not a promise of equivalence to inaccessible private releases or proprietary hosted services.

## Additional implementation and verification tasks

| ID | Feature family | Owner | Status / completion evidence required |
|---|---|---|---|
| L01 | Cross-provider native tools and orchestration | Provider parity + all streams | Implemented: 22 production adapter routes execute read/result continuation, a second turn and portable compaction, including five AWS/Google cloud routes. See audit-provider-parity-implementation.md; synthetic account fixtures are not live-model quality evidence. No unimplemented Bedrock InvokeModel route is claimed. |
| L02 | Programmatic JavaScript tool orchestration and tool discovery | Root integration | Implemented: bounded QuickJS, guarded native subcalls, intermediate values, async fan-out, schema discovery, saved scripts and live pause/resume/stop. Child ownership and checkpoint integration tested. |
| L03 | Manual and automatic recaps | Current features | Implemented: /recap and session-persisted automatic recap opt-out, using the selected native provider. |
| L04 | Conversation/file checkpoints and rewind | Current features | Implemented: durable conversation branches and conflict-aware restoration of confirmed native write/edit changes; unrelated edits survive. |
| L05 | Side questions and asynchronous user questions | Current features | Implemented: /btw, optional question IDs and exactly-once answer delivery, including the actual child owner; main conversation/draft retained. |
| L06 | Skill usage/context diagnostics | Current features | Implemented: /skill-doctor reports real activation/use/context evidence and unused skills. |
| L07 | Configurable output styles | Current features | Implemented: durable predefined/custom prompt styles across native provider requests. |
| L08 | Detached/remote session control | Removed by request | Removed 2026-09-08: no HTTP host, browser dashboard, remote CLI, reconnect transport or detached bootstrap ships. The product is terminal-focused. |
| L09 | Schedules and event triggers | Native runtime | Native terminal schedules and inbox delivery remain. The removed remote host's durable routines, HTTP/API/webhook ingress and detached restart behavior are no longer available. |
| L10 | Cross-session messages | Native runtime | Native exact-recipient messaging and distinct queued/delivered states remain. Remote channels and webhook credentials were removed with the HTTP feature. |
| L11 | Browser interaction | Interactive integrations | Implemented: isolated Chromium DOM controls, brokered network policy, screenshot and owned lifecycle. Real child/sibling isolation and private-directory cleanup verified. On macOS this Chrome build cannot nest inside WorkspaceWrite Seatbelt; it fails explicitly and requires a separately selected compatible host policy. |
| L12 | Artifact preview and notebook editing | Interactive integrations | Implemented: local handles/freshness, inert text/image preview and metadata-preserving notebook edits. Rebound child filesystem and handle isolation verified; HTML rendering shares the browser platform boundary in L11. |
| L13 | Computer-use and voice integration | Interactive integrations + current features | Implemented: native computer tools and human /voice start, stop, cancel, status and insert. Real generated speech at 16/48 kHz reaches the retained composer through local recognition; default-sandbox macOS status passes without recording. Actual microphone and visible-window input QA remain paused until the user reports readiness. Installed recognizer/model and OS permissions are explicit dependencies. |
| L14 | Workflow save/reuse and programmatic execution | Orchestration + root | Implemented: graph tool/agent executor, durable definition library, dependencies/results/retries, cooperative pause/resume and saved JS source with live controls. |
| L15 | Task/approval visibility | Task UI + Plan | Native terminal task/child controls, provider readiness, owner-aware approvals, retained drafts and complete three-choice Plan review remain. The remote browser and its reconnect semantics were removed. |
| L16 | Native review/advisor and security workflow presets | Custom agents | Implemented: /review, /advisor and /security-review launch read-only native children with durable intent/result. User overrides use separate fallback ownership; minimal profiles report unavailable when prerequisites are absent. |
| L17 | Model fallback and usage-limit continuation | Provider parity + root | Implemented: explicitly authorized fallback, independently activated target, one attempt per user turn, safe definitive pre-output failures only; command-line pins and provider replay veto enforced. |
| L18 | Hooks, MCP, plugins and permissions across native routes | Root integration | Implemented and covered by existing/full regression suites, including Plan hook guards and scoped native tools. Shared subprocess lifetime regression repaired; final combined workspace suite passed. |
| L19 | Context, session lifecycle, CLI/editor and accessibility | Root integration | Existing implementations retained; context, compaction, session persistence, keyboard and accessibility regressions run in the full suite. Real terminal task and Plan journeys pass. |
| L20 | Platform and provider capability report | Root integration | The [native implementation report](../audits/native-agent-implementation-2026-09-08.md) records historical integration evidence before the HTTP removal. Its 4,479-test count is not current acceptance evidence. No claim of live account entitlement, intrinsic model modality support or machine-off hosting. |

## Provider contract

Use the selected native provider for reasoning and portable heycode services for tools, planning, agents, workflows, monitoring, memory and controls. Hosted web/compaction features may accelerate a supported route but cannot be the only implementation. Test actual protocol behavior for OpenAI Responses, Chat Completions/gateways, Anthropic, Google and local routes, including DeepSeek/GLM reasoning continuity. Unknown capability is not proof of support; intrinsic model limits remain explicit. No credentials are to be requested in chat or embedded into fixtures.

## Relevant current functional references

- [Dynamic workflows](https://code.claude.com/docs/en/workflows): script-held orchestration and intermediate results, reusable runs, progress controls and bounded lifecycle are the comparison target.
- [Remote Control](https://code.claude.com/docs/en/remote-control) and [Codex remote connections](https://learn.chatgpt.com/docs/remote-connections): competitor references only. The user explicitly removed this product scope on 2026-09-08.
- [Routines](https://code.claude.com/docs/en/routines) and [channels](https://code.claude.com/docs/en/channels-reference): distinguish a persistent host executing triggers from a prompt scheduled only while an interactive session is open.
- [Feature availability](https://code.claude.com/docs/en/feature-availability): competitors themselves distinguish local features from hosted/account-specific services. heycode must implement portable equivalents or report a dependency instead of inheriting a subscription claim.

Do not mark the original audit or this matrix complete until runtime, interface, failure behavior and relevant integration evidence all agree.
