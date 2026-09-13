# Session v2

The session log is the authority for model-visible history, replay, durable
turn settlement, continuation state, and operational inbox accounting. New
model-visible input requires a session event first; live UI state is not a
substitute.

[Open the session-v2 sequence diagram](diagrams/durable-sessions.html). It uses the
default editorial skin, `doc-wide` size, simplified detail, and an engineer
audience. It follows one verified request and omits repair, lineage, query, and
export branches, which are described below.

## Continue, search, and rename conversations

`heycode --continue` (or `-c`) opens the last active conversation in the current
folder. Reopening an older conversation makes it the continuation target even
when no new message is sent. Other folders, archived sessions, and unused
startup logs do not replace that target. If this folder has no previous
conversation, the command explains how to start one.

`heycode --resume` (or `-r`) opens the session picker with search focused. Type to
search names and conversation text, use Up/Down to choose, and press Enter to
resume with the full saved history. The list starts in the current folder;
Escape exposes the picker actions, including `w` to change folder scope.
`heycode --resume <session-id>` and `heycode --resume <path>` select a session directly.
The historical `-c <existing-path>` spelling remains supported.

Session names use an explicit saved title when present. Otherwise the list
derives a bounded name from the opening user message, including for older
conversations. `/rename My new name` saves a replacement name for the current
conversation. That name survives restart and remains searchable; a pending
optional automatic titler cannot overwrite it.

Opening a used conversation records `session/activated`. This updates activity
ordering without adding model-visible history. Renaming a different saved
conversation does not make it the continuation target. Empty picker sessions
are discarded on normal exit or selection.

## Physical format

Each physical session owns `<sessions-root>/<session-id>/session.jsonl`. A root
session stores its complete stream. A fork stores only its constructor-owned
lineage event and local suffix, while logical open verifies and stitches the
parent prefix.

Every line is one flattened JSON envelope:

```json
{"v":2,"seq":7,"time_ms":1730000000000,"kind":"tool/result","data":{}}
```

The fields mean:

- `v`: source envelope version for this exact line;
- `seq`: zero-based logical sequence, contiguous across inherited and local
  events;
- `time_ms`: wall-clock commit timestamp;
- `kind` plus `data`: one member of the closed event vocabulary.

The generated [session event reference](../reference/session-events.md) is the
source-synchronized list of readable v1/v2 kinds and turn-end reasons. It is
generated from `KNOWN_KINDS_V1`, `KNOWN_KINDS_V2`, and `SessionEventKind`, not
copied from this guide.

## Read and migration contract

Opening a session validates in this order:

1. directory/log are non-symlink directory/regular-file objects;
2. log size and terminated JSONL tail are bounded;
3. JSON envelope head parses;
4. version is within the exported readable range;
5. kind exists in that version's closed set;
6. logical sequence is contiguous;
7. kind payload validates for its source version;
8. cross-event inbox, attachment, request, compaction, and lineage invariants
   validate.

Historical bytes are never rewritten on resume. The valid mixed shape is
`v1* v2*`: original v1 lines remain byte-identical and every later append uses
v2. A v1 line after v2 is a version regression. Unknown kinds, newer versions,
sequence gaps, incomplete tails, and invalid cross-event relationships fail
loud rather than being skipped.

Current hard bounds in the implementation are 64 MiB per physical log,
1,000,000 logical events, and lineage depth 64. These are storage safety bounds,
not performance claims.

Source: [session open/append](../../crates/heycode-session/src/session.rs) and
[closed event vocabulary](../../crates/heycode-session/src/event.rs).

## Append is the commit point

For an ordinary event, `Session::append`:

1. validates the payload and stateful projections;
2. assigns the next sequence and timestamp;
3. serializes one line;
4. writes and flushes it;
5. updates the in-memory validated projection;
6. emits `SessionEvent` on the session-owned bus.

Nothing publishes if serialization, write, or flush fails. Listeners therefore
observe durable facts, not intentions.

Two model-input transitions commit as atomic two-event batches:

- `user/attachments` immediately followed by its `user/message`;
- one inbox removal claim immediately followed by the admitted `user/message`.

The batch validates each stateful event against the candidate state produced by
its predecessor, writes/flushed both together, then swaps the projection and
emits in order. Replay cannot show consumed-but-unadmitted or
admitted-but-unconsumed inbox text.

## Neutral and strict projection

The compatibility projection folds only provider-neutral conversation facts:

- `user/message` → User;
- `assistant/message` → Assistant, with its declared tool calls;
- `tool/result` or `tool/rich-result` → Tool, with durable error and untrusted
  provenance;
- the attachment selection immediately preceding a user message;
- the winning portable compaction summary.

Chunks, titles, request snapshots, lifecycle bookkeeping, inbox mutations,
server-tool inspection events, and provider state are not additional neutral
messages.

Strict adapters use `project_requests` and `project_inputs_for_route`. A
`request/header` and `request/context` pair identifies one complete durable
request. Projection validates unique correlation, route/turn/step identity,
contiguous provider output indexes, response metadata, and server-tool
call/result/aggregate relationships.

Same-route provider state is included only when provider, canonical model, and
protocol all match. A complete provider item can replace its duplicate neutral
assistant representation; partial reasoning remains additive. Incompatible
opaque state is excluded and the neutral assistant fallback stays. Thus a
provider switch cannot silently feed one provider's encrypted or signed state
to another.

Source: [neutral projection](../../crates/heycode-session/src/projection.rs) and
[route-aware request projection](../../crates/heycode-session/src/request_projection.rs).

## Verified dispatch

The Agent keeps the live `ResolvedCall` while it commits derived
`request/header` and `request/context` snapshots. It then re-reads the detached
session projection and compares route, prompt and hash, tools, defaults,
options, context/catalog evidence, authentication binding, and ordered inputs.

Only `VerifiedResolvedCall<'adapter>` can enter strict transport. A mismatch
consumes the unverified call and starts no request. This is independent
reconstruction from durable state, not comparison of two aliases to one live
object.

## Provider state and inspection events

Exact provider continuation state and normalized inspection data are separate:

- `assistant/provider-item` retains lossless tagged state for later model
  requests.
- `server-tool/call`, `server-tool/result`, `server-tool/usage`, citations, and
  response metadata retain bounded provider-neutral facts for UI, usage, and
  diagnostics.

Normalized inspection events never substitute for exact replay state. Failed
or cancelled provider output reaches neither successful Finish nor a later
request as valid continuation.

## Compaction

Portable `compaction/applied` replaces its covered prefix with a user-visible
summary for every route. Provider-native `compaction/native` replaces its
covered prefix only for an exact provider/model/protocol match and carries
opaque continuation items.

For another route, the native marker is ignored and original append-only
history remains. Crossing a winning opaque checkpoint requires an explicit
portable re-compaction, pre-checkpoint fork, or cancel choice before route
Settings change. A marker cannot shadow itself or any future sequence.

Later settlement wins when two markers cover the same boundary. The registry,
not an individual strategy, owns the one durable append.

## Crash repair is a projection, not a rewrite

The repair projector reports open turns, steps, calls, and their outcome class.
`derive_messages_repaired` can synthesize an interrupted Tool result for an
unanswered call so a provider receives a structurally valid request that does
not assume whether the side effect ran.

The log is not mutated. The synthetic message explicitly says the outcome is
unknown and must not be treated as tool output or retried on the assumption
that nothing happened.

Source: [repair projection](../../crates/heycode-session/src/repair.rs).

## Shared-prefix forks and lifecycle

A fork boundary must end outside an open turn. The child records parent id,
exact seed event count, and SHA-256 over the parent's persisted JSON-line leaf
hashes. Open recursively verifies the parent, count, digest, cycle/depth bounds,
then exposes one logical stream.

Parent storage remains required until a separate materialization/export owner
makes the lineage standalone. Archive keeps stable paths; destructive lifecycle
operations use a cross-process lineage lock and refuse unsafe/current/open/
ancestor targets according to the query service contract.

Exports are explicit and distinct:

- exact JSONL lineage retains source content;
- bounded Markdown is for human reading;
- structural support output has no arbitrary prompt, answer, provider, path,
  title, or id field.

## Adding a kind

A new kind needs all of the following in the same change:

1. enum variant and exact serde rename;
2. exhaustive `name()` arm;
3. `KNOWN_KINDS_V2` entry, never v1;
4. raw v1 rejection and v2 fixture/drift coverage;
5. exhaustive neutral/request/repair handling;
6. documentation regeneration via `python3 scripts/verify_docs.py`.

Only the enum matches are compiler-enforced. The generated reference and drift
tests catch the plain-list touchpoints.

## Evidence boundary

Session v2 documents current on-disk and projection contracts. It does not
claim cross-version downgrade rewriting, latency at large histories, a complete
fresh-machine migration matrix, SQLite as the log authority, or compatibility
with a future unknown envelope. Those remain explicit release tasks rather than
being inferred from the reader's current bounds.
