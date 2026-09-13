# Phase 2 durable scheduler terminal validation

Date: 2026-09-11 (Australia/Melbourne)

This is bounded controlled evidence for the local durable scheduler behind
`schedule_create`, `schedule_list`, `schedule_delete`, `schedule_wakeup`, and
the local `/schedule` command. It supplements the dispositions in
[`tool-audit.md`](tool-audit.md); it does not modify the
central tracker or close the mandatory paired-Claude UI gate.

## Immutable runtime and isolation

The completed journey ran the real macOS CLI/TUI binary
`tmp/cli-snapshots/ec95c4bbc96501ca/dshx`, SHA256
`ec95c4bbc96501ca627e470f0126915387fee9513cdf14f57cd0b419cea8e903`.
The coordinator's build log (local-only evidence: `tmp/reviewer-build-20260911T092115Z-955a37.log`) records
the successful `dshx-cli` build.

The fixture used a loopback OpenRouter-shaped HTTP server, a dummy environment
credential, and fresh disposable dshx home/workspace directories. It made 18
requests to that local fixture and zero external provider requests. No real
credential, user session, hosted schedule, notification, or external send was
used.

Reproduce from the repository root with the existing PTY Python environment:

```sh
PYTHONUNBUFFERED=1 tmp/subagent-comparison/venv/bin/python \
  scripts/scheduler_terminal_check.py \
  --binary tmp/cli-snapshots/ec95c4bbc96501ca/dshx \
  --output tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24
```

## Observed result

The final run passed with this exact durable schedule mutation sequence:

```text
create, dispatch, create, delete, create, reschedule, delete
```

| Check | Observed result |
| --- | --- |
| Published tool schemas | The actual provider request contained `schedule_create`, `schedule_list`, `schedule_delete`, and `schedule_wakeup`. Cron/recurring fields and the 60–3,600 second wakeup bounds matched the current implementation. |
| Cron rejection | `*/0 * * * *` failed through the actual `schedule_create` tool with `schedule request is invalid`; no schedule create event was admitted. |
| One-shot timer fire | `after_seconds: 1` created UUID `3745da6a-0413-44a9-89d2-8290e9183533`, due at `1789118994435` ms. The running scheduler emitted exactly one `follow_up` inbox splice 7 ms later (sequence 41, message `17738fb8-6cfc-4733-91aa-c27bdaadf297`) and one durable dispatch receipt at sequence 42. The next real CLI/provider turn consumed that exact reminder envelope and produced exactly one response. |
| One-shot restart settlement | Two later CLI resumes produced no second dispatch, follow-up, or provider response for the settled UUID. The journal retained the create, inbox message identity, accepted timestamp, and dispatch receipt. |
| Cron create/list | `7 * * * *` created one recurring eight-character schedule with local timezone, deterministic positive jitter, seven-day expiry, session ownership, and the preserved `session_local` response value. |
| Command presentation | `/schedule list` showed the same ID, cron expression, recurring state, local-time interpretation, next timestamp, and prompt without adding a provider request. |
| Restart restoration | After graceful CLI exit and `--continue`, both `/schedule list` and the actual `schedule_list` tool returned the same schedule ID and timing metadata. |
| Durable delete | The actual `schedule_delete` tool removed the restored cron. Tool and command lists were empty, and a later restart remained empty. |
| Self-paced wakeup | `wakeup_seconds: 60` created a non-restored wakeup; `schedule_wakeup` replaced it with `delay_seconds: 120`, then `stop: true` deleted it. The list was empty immediately and after another restart. |
| Durable journal | The session journal retained three version-two creates, two deletes, one reschedule, and the one-shot dispatch, including schedule and inbox-message IDs. |

The machine-readable result is result.json (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/result.json`).
The exact binary/source hashes and semantic probes are in
manifest.json (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/manifest.json`). The
tool results are in state.json (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/state.json`),
the full provider envelopes and schemas in
requests.json (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/requests.json`), and the
durable journal in events.json (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/events.json`).
Raw terminal bytes are retained in
terminal.ansi (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/terminal.ansi`).

Eleven matching PNG/text captures cover invalid cron, the actual one-shot fire,
cron tool create/list, command list before and after restart, tool list after
restart, cron deletion, empty state, wakeup reschedule/stop, and empty state
after the second restart.
Representative screens are
one-shot fire (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/02-one-shot-fired.png`),
cron after restart (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/05-command-list-after-restart.png`),
and
wakeup reschedule/stop (local-only evidence: `tmp/terminal-evidence/scheduler-pty-20260911T093019Z-285a24/09-wakeup-reschedule-stop.png`).

## Isolated Claude command reference

Claude Code 2.1.268 was opened separately over a PTY with a disposable `HOME`,
`CLAUDE_CONFIG_DIR`, workspace, and session ID. `--bare`, `--safe-mode`,
`--strict-mcp-config`, `--no-chrome`, and manual permission mode were active.
The only credential was a deliberately invalid dummy API key, keychain reads
were disabled by bare mode, and `--remote-control` was not passed.

The harness accepted the dummy-key and security-note onboarding screens to
reach the local composer. It then typed `/schedule` and `/cron` without ever
pressing Enter. Therefore no slash command, conversation prompt, hosted
schedule create/list, or paid model turn was executed.

The isolated built-in picker reported `No commands match "/schedule"`.
`/cron` produced only fuzzy alternatives such as `/scroll-speed`, `/context`,
`/config`, and `/update-config`; it exposed no exact `/cron` command. The UI
also reported `Remote managed settings failed to load (authentication rejected
(401))` and `no remote policy applied`. The result is deliberately narrow:
neither command exists in this unauthenticated, safe/bare built-in surface, but
the capture does not establish what an authenticated account or remotely
managed policy might expose. Because Claude attempted and rejected its startup
remote-policy load, this audit does not claim zero Claude startup network
traffic; it claims only that no paid prompt or hosted scheduling action ran.

Reproduce that capture with:

```sh
PYTHONUNBUFFERED=1 tmp/subagent-comparison/venv/bin/python \
  scripts/claude_schedule_terminal_reference.py \
  --output tmp/terminal-evidence/claude-schedule-reference-20260911T093703Z-e68f9e
```

Evidence is retained in the Claude result (local-only evidence: `tmp/terminal-evidence/claude-schedule-reference-20260911T093703Z-e68f9e/result.json`),
schedule prefix screen (local-only evidence: `tmp/terminal-evidence/claude-schedule-reference-20260911T093703Z-e68f9e/01-schedule-prefix.png`),
cron prefix screen (local-only evidence: `tmp/terminal-evidence/claude-schedule-reference-20260911T093703Z-e68f9e/02-cron-prefix.png`),
and raw terminal bytes (local-only evidence: `tmp/terminal-evidence/claude-schedule-reference-20260911T093703Z-e68f9e/terminal.ansi`).

## Focused source verification

The terminal result complements the scheduler-owned focused checks:

- `cargo test -p dshx-session --test main schedule_domain` — 9 passed, 0 failed, 149 filtered.
- `cargo test -p dshx-agent --test main schedules --no-fail-fast` — 8 passed, 0 failed, 309 filtered.
- `cargo test -p dshx-session --test main version_migration --no-fail-fast` — 11 passed, 0 failed, 147 filtered.
- `cargo check -p dshx-agent --all-targets` — passed.
- `cargo clippy -p dshx-session --all-targets -- -D warnings` — passed.
- Targeted rustfmt and `git diff --check` — passed.

The focused agent tests include due timer dispatch, durable restart replay, no
duplicate enqueue, the 50-record cap, deletion settlement for an already
admitted recurring occurrence, and wakeup stop settlement. The PTY run added a
real one-second one-shot fire; it did not wait for a one-minute wakeup to fire.

## Remaining boundaries

- This proves local deterministic transport and real CLI/tool composition, not
  behavior against a paid or production model provider.
- The Claude reference covers only prompt-free command discovery in one
  isolated safe/bare terminal. It is not authenticated provider validation,
  mouse interaction, a viewport/theme matrix, or evidence for every
  pending/working/failure/cancelled presentation state.
- dshx has no Claude-style automatic roughly 20-minute fallback when a
  self-paced iteration omits both reschedule and stop. Its explicit local
  contract leaves the wakeup awaiting a model decision and does not restore it
  on resume.
- Local session scheduling is not hosted, remote, unattended, or independent
  of the running session. No such capability is advertised by this evidence.
- The bounded PTY journey observed one one-shot timer fire. Recurring due-fire,
  one-minute wakeup fire, and already-admitted cancellation races remain
  controlled-test evidence rather than terminal-timing evidence.

## Live C1 comparison (12 September 2026)

A user-authorized live run compared real Claude Code 2.1.269 (`claude-opus-5`) with real dshx on its saved OpenRouter route for the bounded create/list/delete exercise. Both made exactly seven scheduling calls with the requested arguments, deleted both schedules and finished with an empty list; nothing fired. See the run audit (local-only evidence: `tmp/terminal-evidence/schedule-live-20260912T040531Z-authorized/c1-live-audit.md`) and its 18/18 machine-checked assertions. The remaining gap is presentation: dshx's schedule tool cards and approval box do not yet match Claude's `⏺ CronCreate(…)` / `⎿  Scheduled …` form.
