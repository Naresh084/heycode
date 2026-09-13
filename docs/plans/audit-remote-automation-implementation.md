# Remote automation implementation

> Superseded scope — 2026-09-08: at the user's request, the entire HTTP remote feature was removed, including the browser dashboard, remote host CLI, HTTP transport and webhook ingress. Historical remote commands, screenshots and test counts below describe the former implementation and are not shipping capabilities or current acceptance evidence. The terminal TUI, headless CLI and stdio AppServer remain, along with native schedules, inbox delivery, workflows and Plan review.

Workstream: remote-automation. Shared snapshot: `174740a54b664ae2df2150ea2373f0718ea4201b`.
Branch: `codex/remote-automation-0908`. Native Plan bridge depends on the Plan workstream's `794a061`, `71bd661`, and `b9727fb`; their local cherry-pick equivalents are prerequisites, not remote-workstream deltas.

## Shipping behavior

`heycode remote start` uses the local `heycode-exec::launch_detached_host` bootstrap to start a standalone heycode process with its own process group and no inherited terminal pipes. That process composes the normal AppServer, owns one durable session and runs one turn at a time. Closing a browser, losing a TCP connection, or exiting the starting CLI does not cancel admitted work. `remote stop` uses authenticated cancellation and waits for the host's file lease to release, rather than signalling an unverified PID. `remote resume` reuses the exact stored session identity and configuration.

The CLI never creates an OS process directly. The exec startup guard owns polling, failed-start kill/reap and cleanup on early errors; successful authenticated readiness explicitly detaches that guard. This current-executable-only bootstrap inherits environment/cwd while the host composes its own policy.

The new supervisor is an admission/lifetime owner, not a model loop. Fresh prompts traverse existing `AppServer::request("turn/start")`, RuntimeSession, Agent, tools, policy and durable session log. Existing native inbox inputs (including `schedule_create` reminders) traverse the additive `turn/follow-up` → `RuntimeSession::send_pending` → `Agent::send_follow_up_id_cancellable` path. That path atomically claims the original exact inbox occurrence; it does not cancel/reinsert or append a duplicate user message. Third-party runtimes default to Unsupported for that new operation.

A private atomic state file retains queued, running, cancelling, completed, failed, cancelled and interrupted runs. Admission precedes starting the turn. Input delivery changes from `queued` to `delivered` only after the AppServer projects committed user input. A crash after admission but before a settlement receipt produces `interrupted`, even if an external side effect may already have happened. Such work is never automatically retried. A new explicit retry key is required. Still-queued work resumes automatically; reviewing/discarding it before resume is appropriate if the intent has changed.

A trigger key is bound to its exact payload and recipient session. Identical retries return the existing row; a changed payload under the same key fails. Keys are retained for the host's entire bounded history, including after restart. `inbox:`, `schedule:` and `hook:` are reserved internal namespaces. Cross-session ingress targets the recipient host with its own token and exact session id. Optional source-session metadata is attribution, not authority. It is not an implementation of the team mailbox dispatcher, which is owned by the workflow workstream.

## Local and remote-device use

Create a parent directory, then use absolute canonical paths (including `/private/tmp` rather than a symlinked `/tmp` on macOS):

```sh
mkdir -p "$HOME/.heycode/remote-hosts"
heycode remote start --state "$HOME/.heycode/remote-hosts/project" --workspace /absolute/project --port 8765 -- --trust-workspace
# The normal config/model/provider is used. Omit --trust-workspace to retain restricted workspace authority.
heycode remote token --state "$HOME/.heycode/remote-hosts/project"
heycode remote list --root "$HOME/.heycode/remote-hosts"
heycode remote status --state "$HOME/.heycode/remote-hosts/project"
heycode remote send --state "$HOME/.heycode/remote-hosts/project" --key first-task --text 'Inspect the project and report the next change'
heycode remote stop --state "$HOME/.heycode/remote-hosts/project"
heycode remote resume --state "$HOME/.heycode/remote-hosts/project"
```

Open the printed loopback URL and paste the separately displayed token into the password field. The token stays in page memory, is cleared from the form, and is sent only as an Authorization header. It is never put in a URL, localStorage, a cookie, an access log or the launch command. The browser supports inspect, send, stop, approval, questions, native Plan entry, the complete saved proposal/feedback and all three explicit Plan decisions. Polling preserves review feedback and document scroll position. Only 100 run cards render at once, with active runs first; API/CLI status retains the full bounded history.

For another device, establish an SSH tunnel yourself and browse the forwarded loopback URL on that device:

```sh
ssh -N -L 8765:127.0.0.1:8765 user@host
```

The listener always binds `127.0.0.1`; there is no public-bind option. Browser Host/Origin checks require localhost/127.0.0.1 with the matching HTTP origin. No CORS is enabled. The static HTML/JS/CSS contain no session data or token. API access requires the private host token. The directory is `0700`, files are `0600`, final symlinks/insecure paths are refused, and the host holds an OS file lease. Two 122-bit UUID-v4 random values supply a 244-bit per-host token. All HTTP connections, request bodies and replay storage are bounded.

The foreground diagnostic/service-manager equivalent is:

```sh
heycode --trust-workspace app-server --http-state /absolute/private/host --http-port 8765 --workspace /absolute/project --resume <stored-session-id>
```

Keep the normal global provider/config flags before `app-server`. The host has no auto-start-at-login installation. It stops when its process or machine stops; this does not provide laptop-off cloud execution. To rotate a host token, stop the host, remove its `token.json`, then resume and retrieve the new token.

## Routines, API and webhooks

`POST /api` accepts JSON operations under `Authorization: Bearer <host-token>`. `remote api --state <directory> --file <operation.json>` reads the token privately and sends an operation without placing it in shell arguments. Examples below are request documents for that command.

Create a recurring routine using the existing session schedule record/rule schema:

```json
{"op":"routine_put","schedule":{"id":"00000000-0000-4000-8000-000000000001","prompt":"Check the build status and report meaningful failures.","scheduled_at_ms":1800000000000,"rule":{"kind":"every","every_ms":3600000}}}
```

`kind: "at"` makes a one-shot. `kind: "after"` also requires a validated `delay_ms`, with the absolute target supplied in `scheduled_at_ms`. Existing validation sets a one-second minimum recurrence. The supervisor commits occurrence admission and advancement together, coalesces downtime to one overdue occurrence, and queues occurrences while another turn runs. It does not replay an unbounded backlog. Set top-level `"schedule_enabled": false` to create an API/webhook-only routine. A routine id is immutable and retained as a tombstone after deletion; create a new id to change its definition. `routine_delete` disables its schedule and revokes its webhook token; already-admitted runs remain explicit, cancellable work.

Trigger a saved routine through the host API:

```json
{"op":"routine_fire","id":"00000000-0000-4000-8000-000000000001","key":"ci-build-123","text":"Build 123 failed in the unit-test step."}
```

Create/rotate its restricted webhook token:

```json
{"op":"routine_token","id":"00000000-0000-4000-8000-000000000001"}
```

This explicit operation returns the token once and a `/webhook/<routine-id>` path. Only its SHA-256 digest is retained. Configure the external sender's secret store to send that token as a bearer header to the private/tunnelled endpoint, with `{"key":"stable-event-id","text":"event payload"}`. A routine token cannot inspect sessions, send arbitrary host commands, stop the host or approve anything. `routine_token` with `"revoke": true` revokes it. Token rotation takes effect immediately.

Webhook/API routine text is wrapped as untrusted JSON data after the saved routine prompt. Literal `<` is escaped to prevent payload text from forging wrapper delimiters. The saved prompt must authorize how to use the event data. This is generic authenticated event ingress; no GitHub App, SaaS relay, public webhook deployment, vendor login passthrough or chat-platform adapter is installed.

Other host operations:

| Operation | Behavior |
|---|---|
| `snapshot`, optional `after` cursor | Durable run/routine state, authoritative pending decisions, native inbox counts, Plan state, and newer retained events. |
| `enqueue` with `trigger: {key, session_id, text, source_session_id?}` | Admit direct human/API input; source attribution is optional. |
| `cancel` with `key` | Cancel queued work or request cancellation of active work; preserve cancelling until settlement. |
| `retry` with `key`, `new_key` | Explicit retry of interrupted/failed/cancelled work. May repeat external effects. Native-inbox retries keep the original occurrence identity and cannot claim another message. |
| `respond` with `method`, `params` | Exact session permission/question response, or separate native `session/plan/respond`. |
| `plan_enter` | Native-only safe Plan entry; does not silently claim delegated enforcement. |
| `stop` | Cancel and settle the standalone host. |

Plan `params` are `{sessionId, requestId, decision, feedback?}`. Decisions are exactly `accepted_edits`, `default`, and `stay_in_plan`. Generic permission responses cannot resolve these randomly namespaced review ids. The response acknowledges delivery to the typed plan owner, not a completed permission transition; subsequent Plan snapshots reflect the authoritative atomic transition. A restarted host has a new review namespace, retains the saved full proposal and rejects stale approval ids.

## Bounds and failure semantics

- One active turn, 64 pending/active run admissions, 128 retained routine definitions/tombstones, 4,096 run rows and 64 MiB total host state. There is no eviction that would silently forget idempotency keys; a full history refuses new admissions. Stop/archive the directory and start a new host/session when full. Existing session tools/background workers have their own root-integrated budgets; this host limit is not a global budget across all hosts/providers.
- Replay retains 2,048 events / 4 MiB. Cursor values never reset on restart. A missing retained prefix or cursor from the future returns `gap: true`; the complete conversation remains in the normal session log. Pending review documents are retained separately so event eviction cannot erase an actionable review.
- AppServer events are committed in batches of up to 64 or approximately 50 ms before clients see them. A crash can lose the last uncommitted transport deltas; the session log remains authoritative. Durable write failure stops the host's admission/execution rather than publishing uncommitted state.
- 32 simultaneous HTTP connections, 16 KiB header buffer, 128 KiB request bodies and 30-second connection lifetime. API prompts are limited to 64 KiB; webhook payloads to 32 KiB. Large native inbox messages use a clearly marked preview in host history while their complete original text is claimed from the session inbox.
- Normal shutdown cancels and joins the turn with a five-second deadline. A worker that will not settle is aborted and remains interrupted/uncertain; the host does not claim that arbitrary external child processes survive or that all uncooperative descendants were killed. The runtime workstream owns those descendant guarantees.
- Native idle next-turn inbox work wakes through the existing engine. Idle next-step steer/inject semantics remain owned by the runtime workstream; this host exposes counts without converting those delivery modes into new user turns.
- Native full-plan review requires the normal interactive/proxied approval service (`ask` or `accepted_edits`). Delegated primary runtimes and profiles without that service explicitly report Plan unavailable. Provider credentials/readiness and external runtime executables remain normal heycode configuration requirements.

## Audit linkage and ownership

This supplies detached-session and durable automation functionality extending A11/A12, session/event ingress related to A22/A33, and the remote surface for A37. It does not independently close all 37 findings or replace the runtime, team-mail, background process or task-panel implementations.

Public additions:

- `heycode_exec::{launch_detached_host, DetachedHostStartup}` for current-host bootstrap only, with explicit startup ownership transfer.

- `heycode_app_server::remote::{serve_http_transport, serve_http_transport_with_native, NativePlanReview, Endpoint, Trigger, prepare_directory, check_private, host_token, read_host_token, write_private_json}`.
- `RuntimeSession::send_pending(message_id, cancellation)` with a compatibility default of Unsupported.
- `Agent::send_follow_up_id_cancellable(&InboxMessageId, CancellationToken)`; existing follow-up API is unchanged. Missing/already-consumed ids return `FollowUpError::Empty`; a later still-queued id cannot skip the queue.
- AppServer JSON-RPC `turn/follow-up {sessionId,messageId}`. Native implements it through the same turn pump and operation/cancellation ownership; delegated backends reject it.

Real-process testing also exposed and fixed native AppServer permission routing: native runtime bus notifications could display a permission request whose responder was unsupported. Native host tools now use the same AppServer-owned typed approval waiter as delegated host tools, with the duplicate native bus notification suppressed. Default permission mode is verified by an actual denied-until-approved file write.

Neighboring changes are limited to that inbox/runtime/AppServer bridge, CLI mode parsing/wiring and focused regressions. Shared SDK event enums and TUI code are not changed by this workstream. Root should preserve the exact-id helper while integrating the runtime task's NextStep/steer extension.

## Validation

Process-ownership integration follow-up: the unchanged
`production_process_spawn_is_owned_only_by_heycode_exec` source law passes after
moving bootstrap into the local exec backend. Two focused exec tests pass for
private-group/null-stdin startup, successful reap, failed-start kill/reap and
guard-drop cleanup (two ignored helpers execute only as child fixtures).
`cargo clippy -p heycode-exec -p heycode-cli --all-targets -- -D warnings` passes.
The rebuilt binary again passes all nine real-process scenarios and the browser
checks below, with 24 local fixture provider requests.

- Full AppServer suite: 35 passed, including eight host tests for private paths and read-only token access, lease ownership, capacity rollback, replay gaps, restart/idempotency, live HTTP disconnect/approval/cancellation and scoped/revocable webhook credentials.
- CLI AppServer parser compatibility: two passed.
- Native runtime focused suite: ten passed, including cancellation, resume/configuration, event sequencing, replay eviction and exact pending-claim coverage; wrong id, duplicate id and cancelled call leave the intended queue unchanged, and the two valid inputs each append one user message.
- Standalone Agent exact-claim test: passed; also asserts typed Empty for an already-consumed id.
- `cargo clippy -p heycode-app-server -p heycode-cli --all-targets -- -D warnings`: passed.
- Targeted `rustfmt --check`, JavaScript syntax check and `git diff --check`: passed.
- Shipping binary built successfully. Real-process suite: nine scenarios passed using 24 local fixture provider requests, including detach/disconnect, bounded concurrent admission, exact-session SIGKILL/restart, active provider cancellation, cross-session attribution, native schedule wake, scoped/revocable webhook admission, all three native Plan choices and pending-review crash recovery.
- Headless Chrome: complete long plan, three decision buttons, feedback and scroll retention during polling, disconnect/reconnect without duplicated activity, no page errors, and a 390-pixel mobile viewport without horizontal overflow. Screenshots: desktop (local-only evidence: `docs/audits/assets/remote-automation-2026-09-08/remote-plan-desktop.png`), mobile (local-only evidence: `docs/audits/assets/remote-automation-2026-09-08/remote-plan-mobile.png`).

The self-contained real-process suite is `python3 scripts/remote_automation_smoke.py --binary target/debug/heycode`. It uses a localhost Chat Completions fixture, temporary home/workspaces, real detached binary processes and SIGKILL/restart; no paid provider is required. Optional `--browser-check scripts/remote_browser_smoke.mjs --browser-output <directory>` uses Playwright (or set `HEYCODE_PLAYWRIGHT_MODULE` to an installed module and `HEYCODE_BROWSER_EXECUTABLE` to Chrome). It checks the actual browser's full document, three choices, feedback/scroll retention through polling, reconnect and mobile viewport.

## Current official reference semantics

Reviewed 2026-09-08: [Remote Control](https://code.claude.com/docs/en/remote-control) describes a remote window onto a locally running session and reconnect behavior; [routines](https://code.claude.com/docs/en/routines) describes saved instructions triggered by schedules/API/events, including separate execution infrastructure; [channels](https://code.claude.com/docs/en/channels) distinguishes pushed session events from remote control. This implementation owns those local control/trigger semantics in heycode rather than forwarding vendor subscription authentication. It makes no equivalence claim about their cloud execution, platform integrations or account eligibility.
