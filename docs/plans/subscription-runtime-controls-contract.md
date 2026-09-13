# Subscription runtime controls contract

Status: implemented (2026-09-06)

## Goal

Make the system prompt, heycode tool registry, model, and reasoning effort explicit
inputs to every native or delegated runtime session. A runtime must either apply
each requested field or reject that field before a turn is sent. It must never
silently drop a requested control.

## Shared contract

`heycode-runtime` owns a validated `RuntimeConfiguration` value with four optional
controls: `system_prompt`, `tools`, `model`, and `reasoning_effort`. Start,
resume, and fork requests carry the complete initial value. Live sessions expose
an atomic `configure` operation that treats a `RuntimeConfiguration` as a
partial update; an update is rejected while a turn is active and unsupported
fields are named in a safe `Unsupported` diagnostic.

Tool definitions contain a stable name, bounded description, and JSON Schema
input. A `RuntimeToolExecutor` supplied by the heycode Agent is the only execution
bridge. The bridge records the call, runs the existing approval and pre-tool
policy seams, executes through `ToolRegistry`, records the result, and only then
returns bounded model-visible content to the delegated runtime. Provider-owned
tool calls never bypass that path.

Runtime discovery reports configuration support separately from the existing
session-operation capability struct so existing descriptor literals remain
source compatible. Model discovery also retains advertised effort values and a
default per model; clients use those exact provider values rather than a global
enum.

## Adapter mapping

| Runtime | Initial prompt | Custom tools | Model | Effort | Live update |
| --- | --- | --- | --- | --- | --- |
| Native heycode | Agent prompt renderer | existing registry | route selection | provider request | model/effort between turns |
| Codex app-server | `baseInstructions` | `dynamicTools` + `item/tool/call` | `model` | `config.model_reasoning_effort` | `thread/settings/update` between turns |
| Claude Code | `--system-prompt` | SDK-hosted MCP bridge | `--model` | `--effort` | provider-supported SDK controls between turns |
| OpenCode ACP | unsupported | unsupported | model config option | thought-level config option | `session/set_config_option` between turns |
| DeepSeek Harness / Grok | only fields proved by their concrete protocol | only fields proved by their concrete protocol | current proven model path | rejected unless advertised | provider-specific, between turns |

Codex dynamic tools are experimental, so initialization opts into
`capabilities.experimentalApi` only when a tool bridge is requested. Resume and
fork use only fields present in the pinned app-server schema; if dynamic tools
cannot be re-established by the protocol, those requests fail before opening a
session instead of silently resuming without them.

## App-server and SDK surface

The app-server accepts configuration on session open and adds an explicit
configure control. Responses return the effective configuration and per-field
support. Rust and TypeScript clients expose the same shapes. Current clients
also decode responses from older v1 hosts that omit these additive fields,
using empty configuration and Unknown support rather than inventing evidence.

## Ordering and durability

Configuration is validated and recorded as attempted in the heycode session before
the child runtime receives it. The runtime acknowledgement then records the
same snapshot as committed or failed, and replay applies only committed rows. A
delegated tool call is durably appended before approval or execution; its
terminal result is durably appended before the protocol reply. Cancellation
settles the same lifecycle and cannot leave a reply claiming a result that the
session log has not committed.

## Verification

Contract tests cover validation, capability rejection, active-turn conflicts,
and atomic updates. Adapter fixtures assert exact outbound wire/argv values,
dynamic-tool request/response correlation, permission denial, cancellation, and
durable call/result ordering. App-server and SDK fixtures prove optional-field
compatibility and round trips. Focused tests run before workspace formatting and
warnings-denied Clippy.
