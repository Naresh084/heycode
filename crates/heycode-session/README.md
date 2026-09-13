# heycode-session

Append-only durable session truth, neutral request projection, inbox state and
shared-prefix lineage. The crate deliberately knows no provider implementation;
Consumers map its neutral messages/items into the selected protocol.

## Log and projection

Each JSONL line is a versioned envelope with contiguous logical sequence and a
closed event kind. V1 remains readable; new writes use v2. Unknown kinds,
version regression, incomplete tails, unsafe paths and projection violations
fail loud. Model-visible inputs are rebuilt from the log rather than retained
only in memory.

Fresh v2 sessions commit constructor-owned `session/created` metadata before
publication: safe canonical-shaped cwd, runtime id and source. Legacy streams
report those facts as unknown rather than inventing them. Creation, titles and
other human/lifecycle metadata never become provider messages.

`runtime/configured` durably records each complete system-prompt, ordered-tool,
model and reasoning-effort snapshot before it crosses a child-runtime boundary,
then records whether the runtime committed or failed that attempt. Older rows
without the additive state field decode as committed. The event is v2-only,
validates the same bounds as `RuntimeConfiguration`, and is deliberately
excluded from neutral/provider message projection. Replay consumers apply only
committed rows, while failed attempts remain visible in the audit log without
being presented as effective state.

`project_inputs_for_route` is the pre-resolution input boundary for a proposed
provider/model/protocol tuple. It validates every existing request correlation,
then applies the same same-route provider-state replacement and incompatible
neutral fallback as `project_requests`; Consumers never duplicate that fold.

`request/header.options.provider_options` retains bounded schema-tagged,
provider-owned request policy such as OpenRouter routing and transforms. Older v2 headers
default the list empty. New options validate owner/kind uniqueness and C05
compares their exact object data with the live resolved call before dispatch.

`request/header.options.native_tool_routes` records the sorted logical →
implementation choices made by the effect-owned native-tool registry. Older v2
headers default the list empty. Each route retains provider/client/MCP kind and
provider ownership where applicable; duplicate or unsorted logical ids fail on
read, and C05 compares the exact live route set before transport.

N02/N06 add v2-only `server-tool/call`, `server-tool/result`,
`server-tool/usage` and `assistant/citation`. `project_requests` attaches their safe normalized forms to
the producing request while exact `assistant/provider-item` blocks remain the
only provider replay path. Calls may settle in a later same-route request after
provider pause; orphan, duplicate, cross-route, step-mismatched and regressing
events fail projection. Aggregate usage is unique per request/logical id and
never fabricates call identities. Legacy `derive_messages` ignores all four
kinds.

Bedrock continuation uses the same v2-only `assistant/provider-item` envelope
with core state kind `bedrock_converse_message`. Append/open revalidate the
complete assistant content union. Same-route request projection treats it as
the authoritative assistant turn and suppresses the neutral duplicate;
incompatible routes retain only neutral history. Physical JSONL reopen keeps
opaque reasoning and ordered tool blocks exactly.

TEL01 usage projection groups local exact calls, provider exact calls/results
and provider aggregate requests separately. Exact outcome/unsettled counts come
only from correlated ids; duplicate invalid rows cannot inflate the view.
Aggregate cost remains Unknown unless the durable row carries a validated
published non-zero pico-unit total.
PZA04 adds optional bounded web metadata inside each durable result source:
site name, public icon URL, provider reference and publication string survive a
real append/reopen, while older source rows deserialize with metadata absent.

ATT02 adds v2-only `user/attachments`. A nonempty selection must reference one
to sixteen exact prior `attachment/added` records and sit immediately before
the associated `user/message`. `append_user_message_with_attachments` writes
both lines in one buffered append/flush and publishes both bus events only
after that commit. Open, fork boundaries, request projection, query and
compaction all validate or preserve the pair. Neutral `WireMessage` carries
metadata only; the Agent rereads and verifies immutable bytes at dispatch.

ATT03 extends that same backward-compatible event payload with optional
`document_routes`. Native requires exact identical PDF source/selected records;
extracted requires an exact prior PDF/HTML source plus distinct prior derived
`text/plain`. Every non-image selection has exactly one route. The route is
validated during append/open/request projection and travels on `WireMessage`,
so catalog changes cannot silently switch replay between native and extraction.

ATT04 lets the adjacent `user/attachments` association select validated audio
metadata with no document route. Closed v2-only `assistant/audio` correlates
one to four exact prior audio admissions with their request/turn/step after
terminal provider success. Append and reopen require the prior
`attachment/added`, `request/header` and `request/context`; V1 rejects the kind.
Neutral text projection ignores assistant audio for now, while TUI/app-server
replay consumes metadata and exports remain body-free.

WEB05/E08 use an optional v2-only field on existing tool results:
`untrusted_content:{source:web|mcp|lsp}`. Ordinary v1/v2 results remain
byte-compatible; v1 cannot claim the field. Neutral messages retain the typed
marker and Agent renders a source-specific data-only warning from projection,
so replay/C05/TUI cannot lose or mislabel provenance.

MCP12 adds the closed v2-only `tool/rich-result` kind rather than an optional
field an older reader could silently ignore. Its schema-v1 durable object keeps
ordered text/link/image/audio/embedded-resource blocks, annotations, extension
members, presence-preserving structured JSON, output-schema evidence and the
remote error bit. Media bytes commit as prior `attachment/added` records and the
rich blocks reference their exact metadata. Neutral projection renders provider
text from that durable object, and repair treats plain/rich results as the same
call-settlement class. V1 rejects the new kind as unknown.

O09 adds closed v2-only `hook/contribution`. Each row retains a bounded safe
owner, pre/post phase, lifecycle event, handler family, optional typed
untrusted-content boundary and at most 64 KiB of text. V1 rejects the kind.
Neutral and exact-route projections both emit the same User-role model context,
including the source warning when present; native runtime output deliberately
does not echo the input on a second transient plane. Query/search and human
Markdown use the durable row, while support export keeps only closed provenance,
text length and boundary presence—not owner or text.

Fresh product composition can pre-mint one `SessionId` through
`Session::create_with_id[_and_metadata]` and
`session_with_id_and_metadata_plugin`. The child directory and JSONL are
create-new, so root can bind that exact id into every MCP client route before
plugin application without risking reuse or truncation.

A09 extends the closed `turn/end.reason` vocabulary with `max_steps`,
`max_elapsed`, `max_tool_calls`, `unreported_token_usage` and
`clock_unavailable`; `max_tokens` remains the token-budget reason. The budget
layer reconstructs counters from prior JSONL before each step, so restart keeps
the exact terminal class rather than recovering a generic `error` string.

C12 adds v2-only `compaction/native` beside legacy portable
`compaction/applied`. The native event stores a validated strategy id, one
bounded exact provider/model/protocol item set and optional normalized usage.
Its replaced boundary must precede the settlement event. Route-aware projection
shadows the prefix and injects the checkpoint only for that exact route;
incompatible and neutral projections ignore the checkpoint and retain the
original history. V1 rejects the kind, and self/future-shadowing markers fail on
append, open and independent request projection.

C15 exercises that contract through a real 1,000-turn, 9,011-event file with
correlated request/provider state and ten alternating portable/native
checkpoints. Reopen preserves contiguous seq and all 1,000 request projections;
the exact route receives the final checkpoint plus retained state, while an
incompatible route receives neutral history only. This is a structural stress,
not a wall-clock benchmark.

C14's `opaque_compaction_barrier` inspects the winning settlement using
last-write-wins for equal boundaries. A mismatched native checkpoint returns
its safe route and `EventCount(settlement_seq)` fork point; a later portable
settlement at the same boundary clears it. This is a projection/plan only—the
Agent owns portable/fork/cancel effects and routing owns Settings.

## Unused sessions and structured tool results

`RequestOptionsSnapshot.retry` records the resolved
`RequestRetrySnapshot { max_attempts, safety }` for each dispatch. The field is
optional and defaulted, so a log written before it existed still reads and
reports "not recorded" rather than a policy it never ran under.

`is_unused_session` / `discard_unused_session`: a log holding only startup
housekeeping (`session/created`, `runtime/linked`) is a session nobody used.
The composition root removes such a directory when an interactive shell
closes (or switches session/profile) without a turn, so the picker is not
littered with identical empty rows; anything with a turn, title or lineage is
never touched here and goes through the recoverable delete. `tool_result_value`
hands a committed `ToolResult.content` back structured when it is a JSON
object or array — an `edit` card needs `{diff, message}`, not a quoted blob —
and leaves prose or truncated text as the string it is; replay and the native
runtime stream both use it.

## Inbox admission

- `agent/inbox/splice` carries the two ordered pending lists plus insert-once
  identity and claim/cancel accounting. Insertion, cancellation and claiming are
  all splices; only cancellation carries an `InboxSpliceOutcome`.
- `Session::append_inbox_claim` is the single path from the operational inbox to
  the model. It appends the removal splice and its `user/message` as one atomic
  pair, one message at a time, so replay can never show a message consumed
  without being admitted or admitted without being consumed, and an interrupted
  drain leaves a consistent claimed prefix.
- The batch append path threads the stateful inbox projection through every
  event, validating each against the state its predecessors produce and
  committing the projection only after the whole batch is durable.
- Queue *placement* lives here; the live wake rules that decide when a queued
  input reaches a turn belong to `heycode-agent` (A03).

`InboxMessage::source` is an omitted-by-default durable attribution. Human
input remains wire-compatible; job, goal and schedule sources retain only
validated opaque ids plus revision/round/occurrence counters. The source stays
on the insertion/claim ledger while the admitted `user/message` keeps its exact
legacy text shape. Domain projections can therefore distinguish automation
from human work without inventing a second model-message path.

## Orchestration domains

O10 adds v2-only `goal/change`. Every non-clear event carries the complete
version-one snapshot, admitted-round count and create/update times; clear is a
revisioned tombstone. `project_goal` rejects id reuse, stale/non-contiguous CAS,
illegal phase edges, timestamp/counter regression and goal-source rounds that
are not sequential or not immediately admitted by the atomic claim pair. Live
activation is deliberately absent from the durable type.

O12 adds v2-only `workflow/change`. Definitions are immutable, versioned,
bounded JSON workflows with explicit host capabilities and unique ordered
steps. Replay validates start uniqueness, contiguous progress, one-step
checkpoint advancement, exact resume attempts and terminal outcomes matching
the completed prefix. A start/checkpoint prefix with no end is valid resumable
state, not fabricated success or corruption.

O13 adds v2-only `schedule/change`. Create records retain one-shot delay,
absolute, or fixed-rate timing. Delete and dispatch require an active id; ids
never reuse. A dispatch must name a prior local-suffix inbox message whose
`Schedule` source matches the exact id and occurrence. Recurrence advances by
checked anchor arithmetic directly past missed intervals. The projector takes
`local_start_seq`, so a fork ignores inherited schedule/enqueue events until a
child explicitly appends copied records.

All three kinds are log-only for neutral model projection, v1 rejects them, and
the saved-version/current-kind fixtures cover their exact v2 round trips.

O07 adds v2-only `team/change`. A team starts at revision zero with one lead;
every roster, task-DAG and mailbox mutation advances one contiguous global
revision, while tasks additionally use their own CAS revision. Projection
checks actor role, assignee membership, transition legality, dependency
existence/acyclicity, insert-once message ids and recipient-only claims. A task
cannot enter `in_progress` until every dependency is durably complete. A
crash-left in-progress task remains exactly that until the Agent owner appends
an explicit blocked recovery change—projection never invents completion.

O14 adds v2-only `review/change`. `started` retains the exact runtime id, full
Git commit, bounded patch and additional instructions before the reviewer sees
them. Exactly one `completed` or `failed` may settle a run. Completed results
contain a bounded summary and unique structured severity/path/line findings;
portable paths cannot be absolute, traverse, or use platform separators.
Mutation detection is a closed failure reason, so output from a reviewer that
changed its isolated checkout never becomes accepted findings. Team/review
events are log-only in neutral provider/runtime projection and content-redacted
in support export; their focused projectors are the only semantic readers.

## Crash repair

`project_repair` reads what a killed process left open — `turn/start` without
`turn/end`, `step/start` without `step/end`, a model-requested tool call with no
`tool/result`, a `server-tool/call` with no result — and names each missing
outcome `Unknown` or `Interrupted`. Neither is success, and neither record is
dropped: a silently discarded call and an invented success are the same lie.

Repair is a read-time projection over an untouched log, never a write-back. A
`turn/end` appended at resume time would carry the resume clock and an ordinary
`TurnEndReason`, so no later reader could tell it from one the agent wrote as
the turn really ended; that is a fabricated durable fact, not a repair. Writing
would also rewrite every crashed log merely to draw a picker, collide with a
live writer's sequence, and turn `ForkError::OpenTurn` into an accepted fork at
an invented boundary. Because the projection is a pure function of the event
slice, idempotence is structural: two crashes leave two open records, not four.

`Interrupted` is claimed only where the log proves it — a later `turn/end` for
the record's own turn. A later `turn/start` for a *different* turn is not
evidence, because the envelope permits interleaved turns; reading it as proof
would assert an abandonment the log never recorded.

A torn final line is a different thing entirely and is not repaired here. Bytes
after the last newline belong to an append that never returned to its caller and
never reached the bus, so `Session::open` refuses them as `UnterminatedTail`,
which the query path treats as possibly transient and retries — a live writer
produces byte-identical evidence. Classifying that tear as `InvalidEvent` would
condemn a session the next read would have opened.

`derive_messages_repaired` is the replay-side counterpart: identical to
`derive_messages` on a healthy log, and on a crashed one it answers each
unanswered call with an error-flagged result naming the outcome unknown, in the
model's own call order, before the block ends. `derive_messages` stays the
faithful fold; `project_repair` is how a consumer learns a repair happened.

## Query and forks

`SessionQueryService` is a replaceable `session-query` service. The local JSONL
Provider supports bounded deterministic keyset pagination, latest/resume/fork,
new/rename/archive/delete/export, text/provider/runtime/source/status/cwd/
parent/storage/lineage filters and safe summaries. The ordinary filter selects
active sessions; archived and all-state scans are explicit. Store integrity
stays fail-loud only for what is about the store: a symlinked log or an
unreadable directory fails the whole scan rather than guessing past it. A
directory that is not a session — not named like one, or without a
`session.jsonl` — is skipped, so a review root or a stray folder cannot deny
the listing. Damage to ONE log — a corrupt line or an unterminated tail from a
crash mid-append, or well-formed JSONL this build cannot project (outgrown
bounds, a newer schema, a broken semantic postcondition, a duplicated seq) — is
one damaged session and not a damaged store: it is listed as an unreadable row
(`SessionSummary::is_readable()`)
carrying no invented facts, sorts last so `--continue` still lands on the
newest openable session, and still refuses loudly when it is itself the target
of resume, fork, rename, archive or export. `delete` refuses outright while any
unreadable row exists, because its descendant scan can no longer prove a
complete lineage.

A fork stores only one local `session/created` lineage event plus its suffix.
Replay loads the parent, verifies an exact shared prefix, then stitches logical
events once. Prefix proofs hash the exact persisted JSON line leaves under the
`raw-leaves-v1` domain; reserializing events would make lineage depend on future
serde formatting. Fork publication is staged and durable before rename.
Nested, empty and historical-v1 prefixes work; missing, mutated, cyclic,
over-depth or open-turn boundaries fail. `ForkBoundary::EventCount(0)` safely
represents an empty prefix before a first open turn. Parent deletion must be prevented
while descendants exist, or descendants must be materialized first.

Archive is a durable zero-byte marker inside the session directory. JSONL and
its directory never move, so archiving a root or fork cannot invalidate a
shared-prefix path; restoring removes that exact marker. Rename remains a
normal append-only `session/title` commit. New lifecycle titles use a bounded,
trimmed, terminal-safe type even though historical logs retain their original
bytes for compatibility.

On Unix every live `Session` holds a shared advisory lock on its physical
JSONL. Closed-session rename/archive/delete upgrades a newly verified handle to
an exclusive nonblocking lock; an open writer therefore refuses the mutation.
One owner-only root lineage lock also serializes every fork with delete,
restore and export across processes, closing the scan→rename race in which a
new child could otherwise appear after the descendant proof. Delete
additionally rejects the host-protected current id and any direct descendant,
then atomically moves only a safe leaf into owner-controlled recoverable trash,
checkpoints both directories and returns an opaque restore receipt. Cross-process
destructive mutation is explicitly unsupported off Unix until an equivalent
audited lock backend exists; current-session rename still uses the host-owned
`Mutex<Session>` append path.

One writer per log. A writable handle additionally holds an exclusive
nonblocking advisory lock on the session DIRECTORY: `Session::open_for_writing`
(the product's `session-resume` path), `Session::create*` and `Session::fork`
take that lease, while `Session::open` deliberately takes none, so listing,
export and repair neither block on nor disturb a live session. The lease is
never placed on `session.jsonl`, because readers hold a blocking shared lock
there and rename/archive/delete upgrade that same descriptor; an exclusive log
lock would deadlock the picker against a live writer. Across processes a second
writer is refused with `OpenError::AlreadyOpen` before it writes anything —
`seq` is derived from each handle's own replay, so two writers mint the same
numbers and the log stops opening forever. Within one process the lease is
shared and the newest holder wins, because composition legitimately rebuilds
the world on a session whose previous handle is still referenced; an older
handle's next append then fails loud with `AppendError::Superseded` instead of
colliding. A lease becomes current only once the handle owning it exists, so a
resume that fails to replay supersedes nobody. Like destructive mutation, this
exclusion is Unix-only and never silently faked elsewhere.

The writer refuses exactly what the reader refuses, because the durable log is
never truncated or rewritten and a line `Session::open` would reject makes its
session unopenable forever. `append` folds the same pairing automaton the
reader folds over the whole log, so a lone `user/attachments` selection cannot
commit outside its atomic pair, and it reserves against the reader's own bounds
before writing, answering `AppendError::LogFull` or `AppendError::TooManyEvents`
rather than growing a session nothing can ever reopen.

`LosslessJsonl` export is intentionally a directory bundle: it copies the
target and every required ancestor `session.jsonl` byte-for-byte only after
each physical suffix still matches the hashes validated at open, so a racing
append fails rather than entering an unverified bundle. A fork's
suffix never masquerades as a standalone log. Opening the exported target
re-verifies the same lineage. `Markdown` is a bounded human projection of the
complete durable transcript and compaction facts. `RedactedSupport` is a
schema-v1 structural trace containing only static kind names, seq/time, closed
outcomes, counts/booleans and numeric usage/cache/edit facts. It has no value
slot for prompts, answers, reasoning, tool/provider data, URLs, titles, paths
or ids. All three commit beneath the session store's private `.exports`
directory.

U16 adds v2-only `assistant/response-metadata`, correlated to the producing
request/turn/step. It stores only core-validated neutral cache/context-edit
facts, never provider bodies or cache keys. `project_requests` rejects orphan,
duplicate, mismatched or invalid rows and exposes the successful fact for
restart-safe inspectors; message/provider input projection ignores it.

## Rebuildable SQLite projection

C09 adds `SqliteSessionIndex` as a disposable summary projection at
`.session-index.sqlite3`. Every rebuild first opens all active and archived
root/fork JSONL streams through the ordinary bounded lineage-validating reader.
Rows are sorted by opaque session id and bind the safe summary to a SHA-256 over
the exact persisted JSON line hashes, including inherited prefix leaves. The
database has a fixed application id/schema, an internally checked manifest and
deterministic SQLite bytes for the same JSONL generation.

SQLite is never a fallback truth source. `compare` always re-projects JSONL
before reading the database and returns `missing`, `stale`, `corrupt`,
`unfinished rebuild` or `incompatible schema` as rebuild reasons. Invalid JSONL
is instead a hard source failure and leaves the last-good index byte-exactly
untouched. Rebuild serializes with the existing root lineage lock, clears only
validated index-owned staging residue, writes a private staging database,
re-projects JSONL to detect concurrent change, then atomically publishes an
owner-only replacement. A crash can therefore lose or stale only the derived
index; it cannot change a session log.

## Saved-version compatibility

Q17 fixtures cover a pure v1 log, pure v2 log and the supported v1-prefix/v2-
suffix form. Opening any of them is byte-idempotent. Upgrading v1 means only
appending a v2 line; historical bytes are never rewritten.
`Session::downgrade_guidance` reports compatibility for a selected reader or identifies
the first incompatible sequence and required version, directing the user to a
pre-append copy or newer binary rather than suggesting truncation. Future
versions fail before mutation with the supported range and explicit safe-reader
guidance.

## Verification

```sh
cargo fmt -p heycode-session -- --check
cargo test -p heycode-session
cargo clippy -p heycode-session --all-targets -- -D warnings
```

## Native edit checkpoints

`checkpoints::Checkpoints` persists bounded text preimages before native
`write`/`edit`, then confirms exact postimages after successful execution.
Restore verifies record integrity and every reverse edit chain before touching
files. User edits cause a conflict; symlink paths are refused. The original
postimage is retained in a sibling recovery file during installation, including
on partial I/O failure. Limits are 2 MiB per file and 64 MiB per session journal.
Shell, external-tool, binary and outside-workspace edits are not covered.

Conversation rewind uses the ordinary validated shared-prefix fork, never log
truncation. Pending inputs excluded by its boundary are canceled in the child,
so a rewound follow-up cannot replay itself. Checkpoints for earlier retained
edits are copied with a verified rewind fork; ordinary forks without copied
checkpoints explicitly refuse file restoration into the unavailable prefix.

## Orchestration replay

`workflow/change` retains version-one prefix checkpoints and adds graph-node
intent/settlement rows, pause/resume and a bounded saved-definition library.
`WorkflowRunProjection::nodes` exposes durable attempts/results; graph replay
rejects unknown nodes, premature dependencies, duplicate settlement and retries
beyond the immutable definition's budget. Legacy prefix rows cannot be mixed
into graph execution. Paused runs resume only from their committed node state.
Saved definitions can be replaced without changing definitions of existing runs.

`team/change` adds atomic `members_bootstrapped` and `message_delivered` rows.
`TeamView::mail` returns message, delivered and claimed state independently;
claim is the recipient's explicit acknowledgement. Replay checks lead delivery
ownership and recipient claim ownership. Destination inbox insertion uses a
stable SHA-256 delivery identity and `InboxSource::Team`; the inbox's existing
insert-once history prevents cross-log delivery retry from replaying a message.
Task replay enforces one active task per assignee, including concurrent dispatch.
