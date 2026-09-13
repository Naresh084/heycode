# Whole-session local background lifecycle

Date: 2026-09-11. This audit covers local session hosting and its native-runtime process behavior. It does not close Claude visual parity or live-provider acceptance.

## Implemented behavior

Normal interactive macOS/Linux invocations start one local PTY host and one TUI/runtime owner. `/background` (`/bg`) or Ctrl+] detaches the terminal without replacing that runtime. `--resume` and `/resume` attach to a live owner. Native `/fork` creates a durable conversation copy and a separate owner while preserving the parent. `/sessions`, `/tasks sessions`, `/stop session <id>` and `dshx sessions list|attach|stop` expose the explicit whole-session domain. Existing operation-level jobs and agents retain their own controls. The [user guide](../guides/background-sessions.md) documents invocation and recovery semantics.

The host has one input owner, bounded IPC and display queues, input sequence validation, private socket/registry files, kernel peer validation, and child-authenticated control messages. Session log writer leases remain authoritative. A fork inherits conversation history while its runtime link belongs to its own physical log. Pending approvals require the existing approval policy and cannot be answered by the host. Stale records are never used as authority to signal a PID.

## Controlled process evidence

The complete real-process/PTY journey passed **12/12** on macOS against the shared immutable binary `tmp/cli-snapshots/4ec9c987abef1c64/dshx`, SHA256 `4ec9c987abef1c64ebf8ac24c22ed7dba771e5bc4f1314cdcae573ba25a29a2b`. It used a loopback HTTP provider fixture and temporary homes/workspaces; there were eight fixture requests and zero live fixture hosts after cleanup.

Reproduce from the repository root with a Python environment containing `pyte`:

```sh
PYTHONUNBUFFERED=1 tmp/subagent-comparison/venv/bin/python scripts/session_background_pty.py --binary tmp/cli-snapshots/4ec9c987abef1c64/dshx --out tmp/terminal-evidence/session-background-recheck
```

| Check | Observed result |
| --- | --- |
| Active-turn detach | Same runtime continues; detach submits no new request. |
| Accepted follow-up | A durably queued follow-up executes exactly once while detached. |
| Exclusive attachment | A second terminal is rejected without composing another runtime. |
| Approval across detach | The fixture write remains absent until the human reattaches and approves. |
| Draft and resize | Unsubmitted draft and resized terminal presentation survive reconnect. |
| Independent fork | Child has a distinct runtime writer; parent remains alive until explicitly stopped. |
| Live `/resume` | Terminal moves to the child owner without reopening its live session. |
| Fork permission state | The current default approval policy overrides the parent's stale full-access launch flag. |
| Slow fork startup | Holding catalog composition for 3.5 seconds preserves parent status responses and health checks. |
| Host failure | No inference auto-replay; explicit recovery reacquires the session writer lease. |
| Input sequencing | A duplicate sequence and input after detach are refused before another request. |
| Graceful stop | A pending approval is cancelled; CLI confirms child exit and socket removal. |

Evidence is in evidence.json (local-only evidence: `tmp/terminal-evidence/session-background-20260911T090218Z-d30d55/evidence.json`), requests.json (local-only evidence: `tmp/terminal-evidence/session-background-20260911T090218Z-d30d55/requests.json`), host-records.json (local-only evidence: `tmp/terminal-evidence/session-background-20260911T090218Z-d30d55/host-records.json`), and the captured session logs and ANSI/text screens alongside them. The run log (local-only evidence: `tmp/session-background-pty-20260911T090218Z-ec6125.log`) lists every passing check, and the evidence records the executed binary hash.

The delayed-start check first reproduced a failure on the earlier immutable `dshx`: synchronous fork startup blocked the parent's control loop beyond its ordinary two-second timeout. The before-fix log (local-only evidence: `tmp/session-background-slow-start-before-fix.log`) and captures (local-only evidence: `tmp/terminal-evidence/session-background-slow-start-before-fix`) preserve that result. Fork startup now runs on one bounded worker while the host continues servicing controls. Its final response can outlive the short handshake deadline; ordinary health requests retain their short timeout.

Seven protocol/session tests passed and cover frame bounds, partial frames, private directories/files, symlink refusal, local peer identity, stale sockets, and fork/child/grandchild runtime ownership (log (local-only evidence: `tmp/session-background-protocol-tests-20260911T090002Z-1f4720.log`)). Four CLI unit regressions passed: final handoff-frame flushing after a long attachment, pending startup beyond the short handshake deadline, resume-target identity within a session store, and display reconstruction without historical clipboard side effects (log (local-only evidence: `tmp/session-background-cli-tests-20260911T085737Z-c78c08.log`)). The shared CLI build passed (local-only evidence: `tmp/session-background-build-20260911T090101Z-13627f.log`).

## Remaining acceptance limits

- The process journey used the native runtime and a local fixture. Real providers and delegated runtime detach/reattach were not exercised here.
- Native conversation forking is supported. Delegated runtime forks and custom approval policy copies are explicitly unavailable until their ownership/policy contracts can be preserved.
- macOS was exercised. Linux uses the supported platform path but has not received this process test on a Linux machine.
- A separate [isolated idle Claude comparison](background-idle-ui.md) covers command discovery and reachable empty-session/restricted-launch states, including a verified palette-placement correction. Successful Claude background/fork lifecycle parity, mouse interaction and the color/viewport matrix remain open.
- Hosting survives terminal detach, not machine restart. A host failure requests child shutdown. Later explicit durable resume retains the existing repair semantics; process-local drafts, approval dialogs and in-flight provider streams are not recreated as live processes.
- The host does not provide remote/cloud execution, machine-start services or an automatic retry of an uncertain inference/tool operation.
