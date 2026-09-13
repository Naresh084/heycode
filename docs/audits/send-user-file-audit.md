# Phase 2 `SendUserFile` native completion evidence

Date: 2026-09-11 (Australia/Melbourne)

## Disposition

`P2-T-SendUserFile` now has a real local tool, ATT01-backed rich-result path,
durable replay card, and explicit exact-byte export action. The implementation
and controlled real-terminal journey pass. This is **Implemented; UI parity
gate open**, not complete Claude parity.

The contract is deliberately local: it makes files available in the current
conversation and records `local_only: true` and `remote_sent: false`. It does
not send to a phone, account, cloud service, remote client, or other user.

## Model contract and authority

The built-in `interactive-tools` plugin exposes the exact case-sensitive tool
name `SendUserFile` in both its declared inventory and runtime registry. Its
strict object schema requires:

- `files`: one through eight workspace paths;
- `status`: `normal` or `proactive`;
- optional `caption`: one through 512 bytes; and
- optional `display`: `render` or `attach`, defaulting to `attach`.

Unknown fields, empty files, duplicate resolved files, control-bearing or
non-representable paths, files above 8 MiB, aggregate content above 16 MiB,
cancelled operations, and unavailable files are refused before a successful
receipt exists.

The model cannot nominate a workspace or filesystem owner. `SendUserFile`
uses the `FileSystemService` injected by the current composition and supports
the standard workspace rebind contract. For each call it selects the longest
authorized root containing the current cwd and refuses paths outside that
active root, even if another root is authorized in the same filesystem
policy. Receipt paths are normalized relative to that root; ambient absolute
source paths are not disclosed.

## Exact bytes, durable settlement, and replay

Every file is read once through the bounded workspace filesystem. The tool
computes its SHA-256 revision and `sha256-...` content id over those exact
bytes, emits ordered user-audience `EmbeddedBlob` blocks, and pairs them with
ordered receipt rows carrying the path, index, revision, content id, byte
length, and resource URI.

The existing Agent rich-result barrier admits every blob to the attachment
store before appending the durable `tool/rich-result` event or presenting
success. Admission failure settles as an ordinary error `tool/result`; it
does not emit a partial rich result or successful delivery card. This preserves
the ATT01 invariant that a receipt alone is never authority for bytes.

The TUI recognizes a delivery only when the complete durable rich result
validates and every receipt row matches its ordered embedded attachment
reference, URI, index, content id, revision, byte length, limits, and user
audience. It renders explicit pending/working/failed or `stored` local-only
states. Expanded cards show provenance and attachment identity.

With a delivery card focused, `w` reads the admitted attachment objects, not
the source paths, and writes uniquely numbered files into a newly created
private temporary directory. On POSIX the directory is mode `0700` and files
are mode `0600`. Missing or corrupt current attachment storage produces an
explicit save failure and no false export. Durable receipt metadata describes
historical admission; current object availability is rechecked at save time.

## Controlled verification

All commands ran from `/Users/naresh/Work/Personal/dshx` against the
concurrent working tree.

- `cargo test -p dshx-tools send_user_file -- --nocapture`: 8 passed. Coverage
  includes the schema, ordered exact bytes, user-only resources, default
  display, active-root isolation, workspace rebind, duplicates, empty and
  oversized inputs, cancellation, aggregate bounds, and malformed/control
  input. Log: `tmp/terminal-evidence/send-user-file-tools.log`.
- `cargo clippy -p dshx-tools --all-targets -- -D warnings`: passed. Log:
  `tmp/terminal-evidence/send-user-file-tools-clippy.log`.
- `cargo test -p dshx-tools --all-targets --no-fail-fast`: 96 passed and 7
  explicitly ignored. Log:
  `tmp/terminal-evidence/send-user-file-tools-full.log`.
- `cargo test -p dshx-attachments --test main --no-fail-fast`: 6 passed,
  retaining exact admission, duplicate concurrency, race, tamper, unsafe-root,
  media, and cancellation coverage. Log:
  `tmp/terminal-evidence/send-user-file-attachments.log`.
- Focused TUI delivery-card coverage passed 2/2. It proves complete-receipt
  matching, local-only accessible rendering, source-independent exact-byte
  save, and explicit storage failure. The integrated TUI/CLI check also passed.
- The production CLI inventory test passed with `SendUserFile` present, and
  the shared immutable CLI build passed in 16.89 seconds. Logs:
  `tmp/local-delivery-card-test-20260911T103532Z-687fe8.log`,
  `tmp/copy-delivery-integration-check-20260911T103338Z-3965c6.log`, and
  `tmp/copy-delivery-shared-build-20260911T103740Z-e7c214.log`. Binary:
  `tmp/cli-snapshots/373bb8ce5429e5d0/dshx-20260911T103901Z-42d011`; SHA-256
  `373bb8ce5429e5d0a63d555511546b2726928ad9b193e8126a96f111483ce806`.

The controlled real-terminal journey
`scripts/send_user_file_pty.py --binary
tmp/cli-snapshots/373bb8ce5429e5d0/dshx-20260911T103901Z-42d011 --output
tmp/terminal-evidence/send-user-file-pty-20260911T104154Z-591a15` passed with eleven retained PNG/text
states (`00` through `10`). It made four inference requests, all to the
localhost deterministic fixture, no external provider request, and zero
remote-delivery attempts. It admitted two attachments, committed one rich
delivery, refused one ATT01-incompatible delivery, and made zero inference
requests during either replay.

The journey additionally proved:

- the ordered two-file card appears only after rich attachment settlement;
- after both source files change, `w` still saves their original exact bytes
  with private permissions;
- an ATT01-rejected file settles failed with no `attachment/added` event and no
  successful delivery card;
- journal attachment media types, content ids, receipt rows, embedded blobs,
  and event order all match the source bytes;
- after both source files are deleted, durable replay still renders and saves
  the original exact attachment bytes without inference; and
- after one disposable attachment object is deleted, replay retains honest
  historical metadata while `w` fails explicitly and creates no new export.

Machine-readable results are in
`tmp/terminal-evidence/send-user-file-pty-20260911T104154Z-591a15/result.json`, with the copied journal
and decoded event evidence beside the terminal captures. The pending,
expanded, refused, and missing-object failure captures were also inspected
visually and matched the asserted states.

Two earlier harness runs were discarded before this passing run: the first
fixture omitted a provider-required reasoning delta, and the second expected
an overly narrow failed-card label. Neither exposed a product defect or made
an external request.

No commit, push, deployment, migration, provider credential use, network call
beyond localhost, or external publication was performed by this work.

## Remaining gate

The real-terminal evidence proves the local contract and presentation path,
but it is not a commercial-provider validation and does not prove remote or
mobile delivery (which this implementation explicitly does not claim). The
mandatory paired current-Claude comparison remains open for every applicable
state in the Phase 2 acceptance matrix, including viewport/theme and
keyboard/mouse coverage. Do not close the overall parity tracker row from
these controlled results alone.
