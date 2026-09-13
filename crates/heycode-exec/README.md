# heycode-exec

Provider-neutral filesystem, sandbox, subprocess and shell boundaries. Process
Consumers supply exact resolved specs; the local Provider applies the composed
sandbox immediately before processkit launch and owns the complete process tree.

## Filesystem authority

`filesystem-local` opens explicit canonical directory capabilities, selects
the most-specific grant before checking read/write authority, and rechecks
root/parent/target identity at atomic mutation commit. Traversal, canonical
aliases, symlink escapes, post-resolution swaps and stale observations fail;
full-file replacement breaks a pre-existing hard-link alias rather than writing
through it.

On Windows, canonical relative components additionally reject alternate-stream
colons, reserved device aliases (including extension and superscript COM/LPT
forms), control/reserved characters and trailing dots/spaces before an open.
That lexical rule and foreign-target compilation are local evidence only:
native junction, mount-point, 8.3-alias and `\\?\` namespace cells remain
unobserved, and Windows still has no restrictive process sandbox backend.

## Retained output

Plugin `retained-output-local` publishes service `retained-output` at a
composition-root-supplied absolute storage root. Each service activation owns a
unique generation directory and removes only that generation on shutdown;
holding a cloned service after `Context::shutdown()` cannot keep objects live or
readable.

`RetainedOutputService::retain` commits non-empty bytes under a SHA-256
`RetainedOutputId`. Publication writes and syncs a `0600` temporary, hard-links
it without clobbering the final content address, removes the temporary and then
verifies exact bytes/identity before returning a receipt. Directories are
`0700`; files must be regular, single-link `0600` objects. This audited backend
is Unix-only and fails closed elsewhere until an equivalent owner-security
implementation exists.

Logical `RetainedOutputOwner` scopes every lookup independently of the content
address. A foreign owner and an unknown id return the same `unknown_output`
class. Reads stream-hash the complete object while returning only the requested
range, capped at `MAX_RETAINED_OUTPUT_READ_BYTES` (64 KiB); a replacement,
mode/link/identity change or digest mismatch returns `corrupt`, never mutated
bytes.

The service also owns the preview envelope. The caller's complete-output cap
includes the wrapper, content id, total-byte metadata, escaped preview and
footer—not merely the body. If metadata cannot fit, retention fails before a
file is published. Object/generation byte and entry caps independently bound
disk use. Cancellation before the hard-link commit publishes nothing; once the
link commits, final verification and receipt publication finish rather than
pretending the durable write rolled back.

E06 is product-wired on Unix. The root supplies an isolated absolute storage
root and default `lsp-tools` is its first Consumer: it passes a host-bound owner
and returns `RetainedOutputReceipt::rendered_preview()` unchanged. Other large
shell/terminal/MCP paths may adopt the same service later; they must not wrap an
already bounded preview or accept a model-supplied owner.

## Language servers

Plugin `lsp-registry` publishes replaceable service `lsp` and injects both
`filesystem` and `subprocess`. Plugin `lsp-stdio` contributes exact external
process definitions into that service as context effects. Each
`LspServerDefinition` binds a validated server id/language id, one
provider-resolved workspace and an interactive `ProcessSpec` whose cwd must be
that workspace. There is no shell resolution or inherited environment at this
layer.

The local Provider starts a server lazily through
`SubprocessService::spawn_interactive_raw`, so every server receives the common
sandbox transform and process-tree watchdog. One owned connection serializes
JSON-RPC requests and uses byte-exact `Content-Length` framing with 8 KiB header
and 8 MiB frame bounds. The implemented normalized operation set is deliberately
E08's acceptance surface:

- `textDocument/definition` → bounded, sorted/deduplicated `LspLocation`s;
- `textDocument/references` → the same location boundary;
- bounded file read plus `didOpen|didChange`, then
  `textDocument/diagnostic` → bounded `LspDiagnostic`s.

Every returned file URI is re-resolved through `FileSystemService` and must stay
inside the server's resolved workspace. Diagnostic/provider text is bounded;
paths and messages are absent from ordinary Debug output. Unknown JSON-RPC
server requests receive a method-not-supported response rather than hanging the
server.

Caller cancellation cancels only the active request read and sends
`$/cancelRequest`; a later request can reuse the same server. Registry close or
definition disposal cancels the session token threaded into the subprocess
Provider, clears registrations and drops the sole process handle. No driver task
is detached, so a server or descendant cannot outlive the effect owner.

E08 is product-wired: root composes `lsp-registry` after filesystem/subprocess,
and `heycode-tools` contributes server listing, definition, references and
diagnostics with E06 spill. A zero-definition default world lists empty and
starts no process. A trusted configuration/plugin owner must still construct
exact definitions—project-supplied language servers remain inert until K12
trust. UI warnings name language-server output as untrusted; app-server/IDE
proof remains X07.

## Interactive stdout modes

The legacy text API remains unchanged:

```text
SubprocessService::spawn_interactive
  → InteractiveProcess::into_parts
  → ProcessLines::next_line
```

Byte-framed protocols use the raw API:

```text
SubprocessService::spawn_interactive_raw
  → RawInteractiveProcess::into_raw_parts
  → ProcessOutputReader::read_chunk(cancellation)
  → ProcessOutputChunk::Data(bytes) | Eof
```

Raw stdout is attached through processkit's `stdout_raw_tee` before encoding,
line splitting or capture policy. Invalid UTF-8, CR/LF shape and unterminated
tails therefore remain byte-exact. Each returned data chunk is non-empty and at
most `MAX_PROCESS_OUTPUT_CHUNK_BYTES` (64 KiB).

The raw path has no cumulative stdout capture ceiling or drop policy. A bounded
eight-chunk channel applies backpressure instead of growing memory or discarding
old protocol bytes. The decoded line plane is drained privately with bounded
drop-oldest retention only to drive processkit's raw tee; it is never exposed to
the raw Consumer and cannot fail the process for cumulative stdout volume.

Each raw read receives one caller `CancellationToken`. Canceling a read neither
consumes pending bytes nor cancels the process, so a later read can continue.
Natural stream completion is explicit `Eof`; cancellation, premature provider
closure and I/O failures remain typed errors. EOF is idempotent.

Lifecycle, sandbox and containment are identical in both modes. Explicit
cancel/terminate/kill and plugin shutdown close the raw receiver before waiting
for the tree, which unblocks a backpressured tee. Terminal operations still
consume the one `ManagedProcess` owner, and dropping it retains processkit's
whole-tree kill-on-drop guarantee.

## Exact executable images

`SubprocessService::spawn_exact_interactive_raw` is the PL09 execution-owner
seam. `ExactExecutable` admits non-empty bounded bytes only when they match one
canonical `sha256:` identity. The Unix local Provider accepts native ELF,
Mach-O and universal Mach-O images, writes the bytes into one private `0700`
generation as a single-link `0700` file, syncs and rereads the complete image,
then substitutes that path immediately before the ordinary sandbox/raw-process
launch. Scripts are refused because hashing a script without independently
binding its shebang interpreter would not bind the executed chain.

The exact-image lease is owned by `ManagedProcess` through terminal settlement.
This is necessary even after the spawn future returns: process creation does
not prove the child has completed `exec`, so deleting the private image at that
earlier point creates a real launch race. Wait/cancel/terminate/kill and drop
all retain the lease until the process-tree owner has settled.

`ProcessAuthority` is an explicit four-axis ceiling for filesystem read,
filesystem write, network and descendant creation. The complete child
environment is supplied separately and replaces any environment already on the
resolved `ProcessSpec`. Before staging, the active `SandboxService` compares its
actual read/write/network/process behavior with that ceiling and refuses any
overgrant. Current native backends retain broad host reads and cannot prove
descendant-creation denial, so `deny_all` correctly fails closed; a root owner
must not advertise a native code plugin whose grants are narrower than the
effective sandbox. WASI uses its separate capability-native boundary.

## Persistent terminals

Plugin `terminal-registry` publishes service `terminal`. It injects
`subprocess`, so every terminal is launched by the same Provider, through the
same composed sandbox, into the same private process group as any other child.

```text
TerminalService::open(owner, TerminalSpec)  → TerminalId
  → write / read / resize / kill / list (owner, id, …)
```

The registry — never the caller — owns each session: its process tree, its
lifecycle token, and the one task draining its output. `open` returns only an
opaque `TerminalId`, so there is no handle a Consumer can drop and strand a
process group with. `TerminalSpec` requires an interactive `ProcessSpec`; the
local Provider adds `use_pty`, so `isatty` holds and `resize` issues a real
`TIOCSWINSZ`/`ResizePseudoConsole` on the live terminal. A pseudo-terminal
merges stdout and stderr onto one master, and the Provider supplies the child's
`TERM`/`COLUMNS`/`LINES` on top of the spec's otherwise-complete environment.

**Owner scoping.** Every operation names the owner that opened the session. A
foreign owner receives `unknown_terminal` — the same answer as a genuinely
unknown id — so it cannot probe for the existence of sessions it does not own,
and `list` returns only that owner's sessions. Bounds are per owner
(`MAX_TERMINAL_SESSIONS_PER_OWNER`) and registry-wide
(`MAX_TERMINAL_SESSIONS`); a slot is reserved in the same critical section that
admits the opener, so concurrent openers cannot exceed either bound, and a
failed launch returns its slot.

**Output retention.** A terminal produces unbounded output, so a session's
drain runs continuously — an unread terminal never blocks its child — into a
ring of at most `TerminalSpec::retained_bytes` (`DEFAULT_TERMINAL_RETAINED_BYTES`
= 256 KiB, at most `MAX_TERMINAL_RETAINED_BYTES` = 1 MiB). At the bound the
**oldest** bytes are discarded and counted in `dropped_bytes`, so a live
terminal always shows its newest output and a reader can always see what it
lost. `read` drains what it returns and yields at most
`MAX_TERMINAL_READ_BYTES` (64 KiB) per call, so one Consumer result stays
bounded independently of retention. Bytes are byte-exact: VT escapes and CR
framing survive, because the drain reads the same raw plane as
`spawn_interactive_raw`, never the line decoder.

**Lifetime.** Each session's token is a child of the registry token. The plugin
disposer is `TerminalService::close`: it is runtime-free, refuses further work,
and cancels that one token, which closes each raw receiver before the Provider
waits on the tree and then cancels the Provider's operation token, whose armed
watchdog kills the contained tree. `kill` retires the id first, so a second kill
cannot race it, and keeps stdin open across the hard kill so a well-behaved
child cannot turn it into an implicit graceful shutdown. Dropping the last
service handle drops the sessions, and the Provider's whole-tree kill-on-drop
reaps them; no task retains a session, so nothing can keep one alive.

**Background settlement.** `TerminalService::wait(owner, id, cancellation)`
keeps the owner-scoped status row visible while consuming the one process
handle. Natural exit retires the row only after the Provider and output drain
settle. Caller cancellation is forwarded into that exact process operation and
returns only after tree settlement. A concurrent `kill` retires the id, cancels
the waiter, and joins its stored body-free result rather than racing a second
process owner or returning while descendants survive. `ShellSpec::into_process`
is the explicit E09 bridge: defaults remain resolved exactly once before a
background Consumer adds interactive stdio/PTY mode.

## Verification

```sh
cargo fmt -p heycode-exec -- --check
cargo test -p heycode-exec
cargo clippy -p heycode-exec --all-targets -- -D warnings
```

The service unit tests plus `tests/subprocess_raw.rs` cover invalid UTF-8, split
frames, multi-megabyte cumulative output under a tiny legacy limit, per-read
cancellation, raw-provider overflow, explicit EOF/error, stalled-consumer
teardown, descendant cleanup and plugin shutdown.

`tests/terminal_registry.rs` covers owner scoping across every operation, live
`stty size` geometry after a resize, retention/drop accounting, the bounded
read, reservation release on launch failure, natural/cancelled wait, joined
kill-vs-wait ownership, and real descendant survival markers for kill, registry
close, context shutdown and registry drop.
`resize_changes_the_live_terminal_geometry_the_child_observes` needs a POSIX
shell and is `#[cfg(unix)]`.

`tests/exact_process.rs` proves digest mismatch fails before staging, ambient
path replacement cannot change the admitted image, the explicit environment
replaces prior values, an unenforceable empty authority fails before spawn, and
the image lease survives until the child has crossed the launch boundary.

`tests/retained_output.rs` covers content-addressed no-clobber publication,
logical owner isolation, exact `0700/0600` modes, complete-envelope caps,
bounded range reads, content/mode identity verification, pre-commit
cancellation and effect-owned cleanup visible through a held service clone.

`tests/lsp_service.rs` drives one real contained stdio fixture through
initialize, definition, references, document synchronization and diagnostics;
proves cancellation emits `$/cancelRequest` without poisoning the server; and
uses a release-gated descendant marker to prove context shutdown reaps the
whole server tree.

## Streaming execution and PTY launch authority

`ProcessOutputSink` observes raw stdout/stderr bytes before process settlement.
The local `output_streaming` path keeps bounded decoded capture and tees raw
bytes into a separate observer; unsupported providers fail before launch.
`ShellService::with_executor` retains resolver defaults and delegates both
ordinary and streaming execution to the rebound subprocess service.
`ShellService::subprocess` supplies the same authority for PTY launches.

Terminal output observers are independent of destructive `terminal_read`
cursors. `open_cancellable` releases admission reservations if opening is
cancelled or withdrawn. `open_with_subprocess` lets a host-authorized worktree
use its restricted executor while retaining the shared terminal registry and
caller owner. Process lifetime and tree cancellation stay with the provider.
