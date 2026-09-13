# Session event reference


Source digest: `43020f2185aa4ddcff842fdb45e1aced0ca37ee628701361c4727da74302a1bb`

Sources:

- `crates/heycode-session/src/event.rs`

This reader accepts envelope versions **1 through 2**. New appends always use the current version; opening a historical log preserves its original bytes. Unknown kinds fail loud.

| Kind | Readable envelope versions | Payload role |
|---|---|---|
| `session/created` | v2 only | Immutable creation metadata for one physical session stream. |
| `session/activated` | v2 only | A human opened this conversation; never included in model requests. |
| `runtime/linked` | v2 only | Bind this durable heycode stream to one provider-native runtime session. |
| `runtime/configured` | v2 only | Exact model-visible controls recorded before a child runtime receives them, followed by the runtime's durable acknowledgement outcome. |
| `turn/start` | v1, v2 | A turn began. |
| `turn/end` | v1, v2 | A turn ended. |
| `step/start` | v1, v2 | One loop iteration inside a turn began. |
| `step/end` | v1, v2 | One loop iteration inside a turn ended. |
| `agent/inbox/splice` | v2 only | One normalized mutation of an agent's durable pending-input lists. |
| `goal/change` | v2 only | One revisioned full-snapshot goal mutation or clear tombstone. |
| `workflow/change` | v2 only | One versioned workflow lifecycle/progress/checkpoint mutation. |
| `schedule/change` | v2 only | One versioned session-local schedule mutation. |
| `team/change` | v2 only | One revisioned roster/task-DAG/mailbox mutation. |
| `work/change` | v2 only | One revision-safe structured work mutation. |
| `review/change` | v2 only | One exact review input or structured terminal settlement. |
| `code-mode/change` | v2 only | Durable JavaScript orchestration and nested tool evidence. |
| `hook/contribution` | v2 only | Bounded hook-produced context after its durable bridge committed. |
| `request/header` | v2 only | Complete resolved route/prompt/tool/options snapshot before dispatch. |
| `request/context` | v2 only | Correctness-sensitive context/catalog evidence for a request. |
| `user/message` | v1, v2 | The user submitted a message. |
| `user/attachments` | v2 only | Attachments selected for the immediately following user message. |
| `attachment/added` | v2 only | One content-addressed attachment became durable for this session. |
| `assistant/chunk` | v1, v2 | Presentational streaming fragment; ignorable on replay. |
| `assistant/message` | v1, v2 | One complete assistant message (possibly tool-call-only). |
| `assistant/audio` | v2 only | Audio output committed through ATT01 after terminal provider success. |
| `assistant/provider-item` | v2 only | Lossless provider-owned continuation item emitted by one request. |
| `assistant/response-metadata` | v2 only | Neutral detailed usage/context-edit facts from one successful request. |
| `server-tool/call` | v2 only | One provider-executed tool call normalized for replay/UI inspection. |
| `server-tool/result` | v2 only | One provider-executed tool result normalized without raw response data. |
| `server-tool/usage` | v2 only | Provider-reported aggregate server-tool usage without synthetic calls. |
| `assistant/citation` | v2 only | One public URL citation attached to assistant output. |
| `tool/call` | v1, v2 | The agent dispatched a tool call for execution. |
| `tool/result` | v1, v2 | A tool execution outcome. |
| `tool/rich-result` | v2 only | A typed rich tool result whose media committed through ATT01 first. |
| `compaction/applied` | v1, v2 | Context compaction replaced earlier history with a summary. |
| `compaction/native` | v2 only | Provider-native compaction replaced a prefix with exact opaque state. |
| `plan/mode` | v1, v2 | Plan-mode switch; last-wins fold, enforced by guard layers. |
| `plan/review` | v2 only | Full proposal and explicit review decision; accepted decisions also commit plan exit and target policy. |
| `session/title` | v1, v2 | Log-only session title (never provider-visible). |

## Turn settlement reasons

`stop`, `max_tokens`, `max_steps`, `max_elapsed`, `max_tool_calls`, `unreported_token_usage`, `clock_unavailable`, `error`, `aborted`.

This page enumerates wire vocabulary, not model visibility. Route-aware request projection decides which neutral messages, exact provider state, attachments, and compaction checkpoint form a particular request.
