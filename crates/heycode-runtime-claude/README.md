# heycode-runtime-claude

`heycode-runtime-claude` is the delegated Claude Code process Provider. It keeps
Claude's own agent loop separate from native inference adapters and registers
runtime id `claude` through the shared `runtimes` service.

## R07 boundary

- Supported CLI interval: `>=2.1.241,<3.0.0`.
- Executable discovery happens when an operation starts, so an optional
  missing installation never breaks composition.
- A compound probe resolves one canonical executable identity. Version,
  credential-blind auth status and the fixed compatibility query revalidate
  that same identity immediately before every spawn.
- The subprocess environment is an explicit non-credential allowlist. API
  keys, OAuth-token variables, proxies, hooks, arbitrary Claude controls and
  ambient `PATH` are excluded. A constructed system-only `PATH` keeps standard
  operating-system commands available without inheriting user or workspace
  executable directories.
- The compatibility query is tool-free, uses `dontAsk`, clears MCP/tools, and
  enables both `--no-session-persistence` and
  `CLAUDE_CODE_SKIP_PROMPT_HISTORY=1`.
- Stream parsing is fail-closed: only the expected init, text-only assistant
  canary and exact successful result phases are accepted. Unknown event or
  content discriminators invalidate the probe.
- Stdout/stderr and parsing are bounded; ordinary errors and `Debug` retain no
  path, argv, environment, account field, token or provider body.
- Cancellation and close settle through the common subprocess containment
  capability before returning.

R07 implements version, safe account state and the no-persistence handshake.

## R08 delegated subagents and current stream protocol

The primary session boundary now supports stream-JSON sessions, resume/fork,
steer/follow-up, compaction and permission/question callbacks. New sessions use
the official SDK control initialize frame and lazy init because current Claude
does not emit `system/init` until the first query. User frames use the current
SDK content-array shape; the initial query carries an empty session id and does
not send `shouldQuery`, while non-query delivery sends `shouldQuery:false`.
TurnStarted publishes before stdin write so a fast response cannot overtake
the lifecycle event.

Initial configuration is launch-exact: system prompt, model and reasoning
effort use their dedicated CLI flags. Non-empty host tools are exposed only as
the SDK-owned `heycode` MCP server advertised during control initialization; list
and call requests are allowlisted against the supplied definitions, run through
the durable host executor, and publish correlated runtime tool events. Every
session uses safe mode, an exact empty ambient MCP configuration, disabled
skills and Chrome integration, and current manual host permission prompts bound
to the stream-JSON callback. Allowed vendor calls return the exact original
input unchanged, while the operator sees that input before deciding. An
explicit host tool catalog also disables Claude's built-in tools so calls cannot
bypass heycode approval, sandboxing or durable logging. Claude pre-authorizes only
the exact configured SDK MCP names; heycode remains the single approval boundary
for those host calls. Turn
cancel retracts an active MCP call. Between turns Claude's current control
surface can change the model and reasoning effort through their official SDK
controls; prompt and tools are launch-only and are rejected by name rather than
silently ignored.

Events publish through the shared `heycode_runtime::RuntimeEventHub`, which
validates each emission at its call site and bounds retention by eviction, so a
turn that streams more deltas than the retention window still settles and later
subscribers still get a valid replay from session-ready. A `success` result
with absent, empty or non-string `result` text is still a stopped turn, so the
adapter publishes the empty final message R02 requires rather than emitting a
settlement the normalizer must reject.

Claude context occupancy comes from the main-thread `message_start` usage,
including ordinary input, cache reads and cache creation. The adapter joins
that current-request count to exactly one matching `result.modelUsage` row for
its structured `contextWindow`; missing or ambiguous capacity evidence leaves
the context measurement absent. Turn-aggregate usage remains separate and is
never substituted for the live context size.

The R08 subagent adapter deliberately uses only fresh one-shot sessions. It
mints the session id, enables no-session-persistence and prompt-history
suppression, supplies exact empty MCP configuration `{"mcpServers":{}}`, and
disables Chrome/slash-command surfaces. Parent approval decisions become
AllowOnce/Deny; interactive questions are refused; cancellation calls the
runtime and close joins the owned process. Deterministic plan/tool/permission
fixtures and a tool-free installed subscription canary pass against Claude Code
2.1.251 without inspecting credential values.

## Verification

```sh
cargo fmt -p heycode-runtime-claude -- --check
cargo test -p heycode-runtime-claude
cargo clippy -p heycode-runtime-claude --all-targets -- -D warnings
python3 scripts/claude_tool_smoke.py --live
HEYCODE_E2E=1 cargo test -p heycode-runtime-claude --test runtime_contract \
  live_installed_claude_probe_is_explicitly_gated -- --exact
HEYCODE_E2E=1 cargo test -p heycode-agent \
  live_installed_claude_ephemeral_subagent_is_explicitly_gated -- --exact
```

The optional live tool smoke uses an isolated heycode home and temporary workspace
with the existing Claude subscription, then proves host `bash` exits zero,
`read` returns an exact marker, `todo_write` completes, and exactly four ask-mode
callbacks round-trip. The gated probe invokes the installed CLI's official
status and one fixed, tool-free, no-persistence query. Neither check inspects
credential files or token values.

## Connection setup update — 2026-09-05

Plugin-owned account/version discovery runs in an owned temporary workspace. Session start uses the caller’s working directory. An explicitly enabled installed tool-free subscription turn passed on 2026-09-05.

Runtime descriptors may carry bounded provider-owned connection recovery instructions. The subscription wizard displays them after account or installation checks fail and keeps Enter available for retry; sign-in is performed through the official app.
