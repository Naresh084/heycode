# Current command/session features implementation

Owner: session-controls. Baseline: `174740a54b664ae2df2150ea2373f0718ea4201b`.
Review cut-off: **2026-09-08**. This extends the 37-finding audit; these additions
are CF01–CF06 rather than fabricated closures of A01–A37.

## Dated source review

Official sources read on 2026-09-08:

- [Codex changelog](https://learn.chatgpt.com/docs/changelog): the requested
  CLI 0.153.4 entry is dated September 4, including an async-question guidance
  fix conditioned on tool availability. This is evidence for the feature's
  conditional semantics, not proof that every Codex surface has that tool.
- [Claude changelog](https://code.claude.com/docs/en/changelog) and
  [what's new](https://code.claude.com/docs/en/whats-new): reviewed through the
  requested 2.1.263 cut-off. No later-release capabilities are claimed here.
- [Interactive mode](https://code.claude.com/docs/en/interactive-mode): `/btw`
  uses current context without growing main history and can run during work;
  recaps are manual or automatic on return after inactivity, with an opt-out.
- [Checkpointing](https://code.claude.com/docs/en/checkpointing): distinguishes
  conversation and code restoration, durable prompt checkpoints, and tracked
  editing-tool changes. Shell/external edits require separate treatment.
- [Output styles](https://code.claude.com/docs/en/output-styles): persistent
  response instructions change communication style. heycode always retains its
  base coding and permission instructions.

These are portable heycode implementations inspired by the documented behavior,
not declarations of provider-native protocol parity or subscription access.

## Shipping scope

| ID | Surface | Implementation |
|---|---|---|
| CF01 | `/recap [on\|off]` | Bounded tool-free one-sentence summary; default-on, per-session persisted automatic return recap after three completed turns and three idle minutes, debounced by durable turn end identity. Interactive native TUI only for auto recap. |
| CF02 | `/rewind [turn [files]]` | Lists the newest 50 prompt boundaries; creates a verified shared-prefix fork before the selected prompt, then switches through the existing resume bridge. Optional confirmed native write/edit content restore rejects conflicts. Original conversation remains accessible. |
| CF03 | `/btw <question>` | Independent tool-free inference over a bounded current text excerpt, available during main work. No main log messages or turn events. Ctrl+P preserves the displaced full composer and cursor while obtaining/submitting the question. |
| CF04 | `/skill-doctor` | Current discovered/skipped skills, invocation eligibility, instruction size and approximate context cost, durable delivered-body counts and bodies retained after compaction. Does not claim instruction compliance. |
| CF05 | `/output-style` | Default, concise, explanatory, learning and bounded custom instructions. Session preferences persist and append to the next native request's system prompt. |
| CF06 | `ask_user_question_async`, `/questions`, `/answer` | Optional questions durably admitted without waiting. No implicit answer. Answers are deduplicated through a stable question/inbox ID, then delivered using the existing FollowUp queue; dismissal sends no invented answer. |

## APIs and integration

New Agent methods: `side_question`, `rewind_points`, `rewind`, `output_style`,
`automatic_recap_enabled`, `async_questions`, `answer_async`,
`take_rewind_draft`. New public values:
`OutputStyle`, `RewindPoint`, `AsyncQuestion`.

`heycode_session::checkpoints::Checkpoints` provides bounded durable preimage
capture, confirmation, verified prefix copying and conflict-checked restoration.
Native `Agent::execute_call` calls
`session_control::prepare_edit(&Mutex<Session>, &Path, &ToolCallInput)` before
the approved execution and confirms its returned `PendingEdit` only on success.
Root's CodeMode executor must reuse this pair with its **current child's**
session/cwd; no second journal should be introduced.

`QuestionOwner { session, bus }.scope(future)` binds inherited question tools to
the actual executing child. Root's CodeMode ToolExecutionContext must apply this
scope too. The weak default tool owner is only a fallback for direct host calls.

Auxiliary inference shares `CompactionContext::auxiliary_text`: prepared exact
provider adapter, current model catalog, P10 request interception, no native or
client tools, bounded output and cooperative cancellation. Opaque response state
is discarded for an aside; compaction's existing stricter summary rules remain.
The main request's system render moved outside its session lock to avoid mutex
re-entry when reading persisted style.

No `SessionEventKind` variant was added. Preferences and pending optional
questions are in bounded session-owned `session-controls.json`; actual answer
admission is authoritative JSONL. A crash between inbox commit and state cleanup
cannot duplicate an answer. Native child scope is separate from its parent's.

TUI edits are isolated mostly in `session_control.rs`, with narrow command
registration, focus events, draft retention and active-route checks. Delegated
TUI sessions refuse these native conversation commands rather than mutate the
dormant native Agent. Optional questions are omitted from delegated runtime
configuration because provider-native answer delivery is not implemented here.
The task-ui workstream confirmed selected-child routing for `/btw`, `/recap`,
`/questions`, `/answer`, and `/output-style`, using the selected native child;
other child slash commands explicitly require returning to the parent. That
routing ships in the separate task-ui commit and needs integration validation.

## Restore safety and boundaries

- Captures only local native `write`/`edit` text paths inside the Agent workspace;
  shell, MCP, external-runtime and outside-workspace changes are not rewound.
- Paths are traversed through directory capabilities without symlink following.
  Records have strict schema/version/path/size checks and SHA-256 confirmation.
- Before changing files, restoration validates **every** current postimage and
  the full reverse edit chain. User changes between or after agent edits cause
  refusal. Unknown interrupted edits refuse restoration when files changed.
- Restore preserves the current inode under `.heycode-rewind-<uuid>` before an
  exclusive installation of the preimage. Concurrent writers cannot be silently
  overwritten. Recovery copies remain beside restored files, including on a
  partial multi-file I/O failure; such failure is reported, never called atomic
  multi-file success.
- 2 MiB per text file, 64 MiB per session journal, bounded record count. Binary,
  oversized or unsafe inside-workspace edits fail checkpoint admission before
  executing. File restore is content restoration, not Git index/branch reset.
- Rewind requires an idle Agent and no pending inputs. Inherited queue entries
  excluded by a follow-up rewind are canceled in the fork so the old prompt
  cannot silently run again. Pending optional questions are not inherited.
- Verified earlier edit records follow a rewind fork for later earlier rewinds.
  Restoring an earlier file state after a conversation-only rewind may refuse
  because the file state intentionally diverged from that conversation.
- A corrupt file journal does not block a conversation-only rewind. The child
  records a restoration floor so unavailable earlier file history cannot be
  mistaken for an empty journal. Ordinary forks also enforce their own floor.
- The selected prompt is restored to the resumed TUI composer as a one-shot
  draft, without submitting it automatically.

Asides answer a single bounded question; they do not implement a separate
multi-turn side conversation. Automatic recap generation begins on return,
not as a background prefetch. Full vendor UX parity is not claimed.

## Validation status

Complete for this workstream: **48 distinct focused tests passed**.

- `cargo test -p heycode-agent session_control`: 2 unit and 10 integration tests.
- `cargo test -p heycode-agent rewind --test main`: 3 passed, including one
  additional corrupt-journal conversation-rewind recovery test.
- `cargo test -p heycode-agent compact --test main`: 13 passed, protecting prepared
  provider/P10 and portable compaction behavior.
- `cargo test -p heycode-session checkpoints`: 5 passed.
- `cargo test -p heycode-skills`: 15 integration tests passed.
- `cargo test -p heycode-tui session_control --lib`: 2 passed.
- `cargo clippy -p heycode-agent -p heycode-session -p heycode-skills -p heycode-tui
  --all-targets -- -D warnings`: passed. Agent all-targets Clippy passed again
  after the final rewind recovery change.
- Scoped formatting and `git diff --check`: passed.

Coverage includes strict prepared adapters, concurrent tool-free asides without
main-history mutation, persisted request styles, optional question child
ownership, durable answer delivery and crash-cleanup deduplication, exact
prompt/follow-up rewind, native write checkpoints, user-edit conflicts,
symlink/tamper/interrupted-edit refusal, copied checkpoint history, composer
text/cursor restoration, and recap eligibility/debounce/opt-out.

Full workspace integration, real PTY flows, and paid-provider end-to-end tests
remain with the main audit task. CodeMode nested execution and selected-child
TUI routing must retain the integration hooks described above.

## CodeMode integration follow-up

After coordinating with the execution workstream's `1887bdd`, edit checkpoint
capture/confirmation and `QuestionOwner` binding now live centrally in
`ToolExecutionContext::execute_preapproved_observed`. Direct agent execution,
observed/background execution, and nested JavaScript calls use that same path.
`ToolExecutionContext::execute` retains its plain-JSON result checks and routes
through `execute_observed` for approval and guarded dispatch.

The context captures the Agent's existing shared `PlanHandle`. Its pre-admission
check uses `PlanMode::tool_refusal` before generic approval; the execution guard
still rechecks Plan after approval. Blocking/optional questions and
`exit_plan_mode` retain their dedicated human-input admission behavior. Optional
questions are included in Plan's allowed tools.

New composed regressions prove:

- `run_code` performs native write and edit calls that can be rewound together;
  later user edits cause a non-destructive refusal. This test failed before the
  shared checkpoint hook was installed.
- Parent-to-child-to-JavaScript optional questions persist in the child. A
  subsequent invocation from a fresh Tokio task, without inherited task-local
  state, still writes to the child's session and emits only on its UI bus.
- Plan rejects workflow writes and `run_code` before invoking generic approval,
  while optional questions return immediately and leave Plan active.

Prerequisite commits were cherry-picked only to validate this follow-up locally;
the integration task should cherry-pick only the new follow-up delta after its
existing session-controls, execution, CodeMode, runtime, and Plan commits.

Follow-up validation: **35 distinct focused tests passed** (`run_code_`: 2;
`code_mode`: 8; `session_control`: 2 unit + 12 integration; `it::execution_jobs`:
12, with one overlapping file-rewind test). Formatting of changed Rust files
and `git diff --check` passed. All-target agent Clippy with `-D warnings` reports
13 inherited findings in `execution_jobs.rs`, `execution_foreground.rs`,
`execution_output.rs`, `jobs.rs`, `subagent.rs`, `subagent_provider.rs`, and
`runtime_subagent.rs`; no findings target this follow-up's dispatch code. Those
prerequisite findings remain for their owning workstreams/main integration.
