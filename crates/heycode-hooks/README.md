# heycode-hooks

Effect-owned lifecycle hooks. The `hooks` plugin publishes `HookService` after
the shared shell service and keeps project hooks inert until the workspace is
affirmatively trusted.

## O08 command hooks

Command hooks run sequentially with one fixed ten-second budget and bounded
output. A deliberate non-zero `Pre` exit may refuse; the same `Post` exit is a
fault because the operation already happened. Timeout, launch failure,
cancellation and untrusted project scope are faults and do not suppress later
hooks. Hook and handler registrations exist only as exact context effects.

## O09 typed providers

`HookAction` adds prompt, subagent and MCP-tool actions behind one
`HookHandler` registry. All handlers use `HookService::run_with`, so trust,
cancellation, time budget, ordering and phase policy are applied once.

- `PromptHookHandler` delegates to `PromptHookRunner`. A product adapter must
  obtain `HookDecision` through a structured output channel, never by matching
  model prose.
- `SubagentHookHandler` delegates to `SubagentHookLauncher`. Unsupported,
  unknown and authority/depth refusals are launch faults—the child never ran
  and therefore never vetoed the surrounded operation.
- `McpToolHookHandler` delegates to `McpToolHookCaller`. A server-reported tool
  error is a hook fault, while a successful body becomes a contribution whose
  MCP untrusted-content boundary is assigned by `HookService`, not the server.
  MCP handlers cannot refuse: server-authored data cannot become authorization.

Payload provenance is projected into the bytes a handler reads. Contributions
leave `HookService` as a pending, non-renderable value carrying owner, phase,
event, handler kind and untrusted-source provenance. Only a successful
`HookDurableEventBridge` commit mints `CommittedHookContribution`, the sole type
with a model renderer. Debug output carries lengths/classes, never prompt,
subagent or MCP bodies.

This crate intentionally depends on neither Agent, Session nor MCP. The product
adapter now lives in `heycode-tui`: it maps provenance into v2
`hook/contribution`, appends, flushes, physically reopens and verifies the exact
row before `CommittedHookContribution` can render. Effect-owned Agent,
SubagentRegistry and MCP ports invoke UserPrompt/Subagent/McpServer hooks at
their actual operations; cancellation/panic/commit faults remain non-vetoing
unless an entitled Pre hook deliberately refused.

Central composition now mounts `product-hook-attachments` and selects the
product MCP/TUI constructors. O09 remains active only for concrete structured
`PromptHookRunner`, `SubagentHookLauncher` and `McpToolHookCaller`
implementations. Nothing parses model prose into a decision, invents subagent
authority or bypasses the ordinary MCP approval/tool path.

## Verification

```sh
cargo fmt -p heycode-hooks -- --check
cargo clippy -p heycode-hooks --all-targets -- -D warnings
cargo test -p heycode-hooks --no-fail-fast
```
