# Local background sessions

On macOS and Linux, a normal interactive `heycode` starts a local terminal host and one TUI/runtime process. The host owns a private PTY. Your terminal attaches to that PTY; detaching leaves the same runtime, pending work, drafts and approval dialogs alive.

| Action | Behavior |
| --- | --- |
| `/background`, `/bg` | Detach the terminal while the current session continues. Works during a turn. |
| **Ctrl+]** | Detach from any screen, including a pending approval dialog. |
| `heycode --resume <id>` | Attach to that session's live owner, or recover the durable conversation if no live host remains. |
| `/resume <id>` | Move the terminal to another live owner, leaving the previous session detached. Saved sessions use the existing resume flow. |
| `/fork` | For the native runtime, create a durable conversation copy at a completed-turn boundary and start its separate background owner. The parent stays active. Delegated runtime copies require a real provider fork operation and are explicitly unavailable. |
| `/branch [id]` | Create and switch into a conversation branch using the existing session lifecycle. |
| `/sessions`, `/tasks sessions` | List whole-session owners and their attached, detached or stopping state. |
| `/stop session <id>` | Request that owner's normal TUI cancellation and shutdown path. The message confirms the request, not completion. |
| `heycode sessions list` | Inspect local whole-session owners without composing a model runtime. |
| `heycode sessions attach <id>` | Attach by durable session ID or host ID. |
| `heycode sessions stop <id>` | Request graceful stop and wait up to ten seconds for confirmed child exit. A timeout is explicitly unconfirmed. |
| `heycode --no-background` | Run the TUI directly. Whole-session hosting commands are shown as unavailable. Useful for terminals or fixtures that need a single directly owned process. |

Background jobs, shell terminals, agents and structured work remain separate domains. `/tasks` and `/stop <job-id|terminal-id|all>` retain their operation-level behavior. They do not silently stop a whole session.

## Ownership and recovery

One host admits one terminal input owner. A second attachment is refused. Input frames carry monotonically increasing connection-local sequence numbers; duplicates are refused before writing to the PTY. Reconnecting never replays old input. Display reconstruction uses terminal state, excluding historical clipboard writes and terminal queries.

The existing session log writer lease remains authoritative across processes. A terminal attachment does not open that log or start an inference loop. Starting two recovery attempts cannot create two writers. A fork inherits conversation history, not its parent's runtime ownership: runtime links belong to the physical child log. Registry records are observations; no control operation signals a PID taken from a stale record.

An input transport receipt means bytes were written to the PTY. Model input is accepted only through the existing durable session/inbox path. Accepted follow-ups survive terminal detach and use the existing durable recovery semantics. Unsubmitted drafts and partial terminal keystrokes are retained in the live TUI; they are not durable across a process or machine crash.

The host never answers a permission request. With the default interactive policy, an approval stays pending until a human reattaches and answers or explicitly stops the session. An already-selected permission policy continues to govern tool execution.

If the host dies, the TUI's control watcher requests normal shutdown. A later explicit resume must reacquire the session writer lease. Interrupted operations retain the existing session repair semantics; the host does not automatically retry an uncertain inference or tool mutation. After a machine restart, use normal durable resume. Process-local drafts, open approval dialogs and an in-flight provider stream cannot be reconstructed as live processes.

The IPC directory is user-owned with mode `0700`; socket and registry files use `0600`. The kernel peer UID is checked. Child-only registration, fork and handoff also require the per-generation credential and the actual PTY child's kernel-reported PID on macOS and Linux. Requests, display dimensions, connection count and output backlog are bounded. A slow/disconnected terminal loses attachment while the session remains owned.

## Scope and evidence

This is local process hosting. It does not provide cloud execution, remote control, system-start services, mobile handoff or automatic restart after an OS reboot. Attaching keeps the live owner's current model, permissions and configuration. Use its normal commands to change those settings. `--no-background` and noninteractive invocations retain direct process behavior.
