# Phase 2 current-conversation export completion audit

Date: 2026-09-11 (Australia/Melbourne)

## Verdict

The source-backed current-conversation export implementation and its
all-applicable-state UI gate are **complete**. `P2-C-export` is ready to move to
`complete`.

The production filesystem is too fast locally to retain a meaningful working
frame. Pending, terminal and environmental states are therefore proven through
the real production binary, while working/cancelling and teardown are proven
through the production panel and app state with a deliberately slow,
replaceable `FileSystemBackend`. This is the only controlled seam in the gate;
it exercises the same typed request, cancellation token and completion path as
the real binary. The distinction is retained rather than representing a
synthetic delay as a production-terminal observation.

This verdict is deliberately narrower than whole-session export. The existing
lossless JSONL, Markdown and redacted-support formats remain separate durable
session exports and were not replaced.

## Current source contract

The installed Claude Code 2.1.268 reference is retained at
`tmp/terminal-evidence/claude-export-reference-20260911T110611Z-7a3a2a/`. Its inspected contract and ten
captures establish:

- bare `/export` opens `Export conversation` / `Select export method`;
- `Copy to clipboard` is the first and default method;
- `Save to file` opens an editable generated `.txt` filename;
- Escape from the filename form returns to the chooser, and Escape from the
  chooser reports `Export cancelled`;
- explicit `/export fixture-export.txt` writes directly;
- extensionless names gain `.txt`, missing parents are created, and a directory
  target fails; and
- the payload is the rendered plain-text conversation, including the product
  header and prior visible local-command rows rather than Markdown source or a
  lossless journal.

The source contract makes no overwrite, arbitrary-absolute-path, mouse,
Unicode, long-export or host-clipboard assertion. Those are not inferred.

## Implemented boundary

- Bare `/export` takes one immutable rendered-conversation snapshot, then opens
  the two-method chooser. The current export command is excluded from that
  snapshot; earlier visible command/result rows remain eligible.
- The chooser supports keyboard selection, Enter, Escape/back and an editable
  filename form. Clipboard is the default.
- `/export <path-like-name>` writes current plain text directly.
  `/export file <path...>` is the explicit unambiguous form for extensionless
  names or paths containing spaces. A single safe non-path token remains a
  legacy session identifier so old `/export <session>` use is not silently
  reinterpreted.
- `/export jsonl|markdown|support` and `/export <session> [format]` retain their
  existing durable structured-export behavior.
- File save uses the active `FileSystemService`, `PathRequest`, and
  `CheckedWriteSpec::create`. It creates missing parents within the admitted
  root, refuses existing files and directories atomically, rejects destinations
  outside allowed roots, and never routes through a shell or ambient
  `std::fs` write.
- Rendered export input is bounded at 16 MiB. The terminal clipboard adapter
  independently retains its existing 64 KiB transport bound; an oversized
  clipboard request reports failure rather than truncating or claiming success.

## Controlled verification

The following current checks pass:

- `cargo test -p dshx-tui --test export_panel_standalone`: 11/11;
- `cargo test -p dshx-tui --lib export_integration_tests`: 3/3;
- `cargo test -p dshx-tui --lib session_browser::tests`: 3/3; and
- `cargo check -p dshx-tui --all-targets`.

These tests cover chooser state, Escape ownership, multiline-paste refusal,
`.txt` normalization, quoted nested paths, exact Unicode bytes, parent creation,
create-only refusal, directory/outside-root refusal, the 16 MiB bound, parser
compatibility, immutable snapshot ownership, active-command exclusion and draft
isolation. The production app-state tests additionally prove that an unsettled
export consumes paste and keyboard input, cannot be replaced by another export,
and renders the cancelling state after Escape.

`slow_filesystem_cancel_settles_without_a_late_file` wraps the production
filesystem interface with a controlled delayed backend. It proves that the
working panel is rendered, Escape changes it to a visible cancelling state,
the shared cancellation token reaches the actual create-only operation, the
typed outcome settles as `Cancelled` within two seconds, and no file appears
after teardown.

## Production-binary PTY evidence

`tmp/terminal-evidence/export-panel-pty-20260911T115450Z-1a6af8/` was produced from immutable CLI
`tmp/cli-snapshots/6b23742beb414b34/dshx` (SHA-256
`6b23742beb414b34527412e652ba1147adbd05e5f9558442964a057b996d1e60`)
with two loopback OpenRouter-shaped model responses in disposable
home/workspaces. It contains 20 PNG/text states, raw ANSI streams, request and
session-event records, and exact file/clipboard payloads.

The retained result proves:

- exactly two provider requests and two durable user messages seed the normal
  and large-transcript states;
- every file, clipboard, replay, responsive and display-mode resume performs no
  new provider request;
- direct file save produced 226 exact bytes;
- the intercepted OSC 52 clipboard payload has the same SHA-256
  (`a1afb9961fe292d2abcd15823a161e7c0c2be26e480ebc308824a6ca843fadbd`)
  as the direct file payload;
- the current `/export` command is absent from both payloads;
- chooser default/copy, Save selection, generated filename, Escape back,
  Escape cancel, direct save, extensionless form save, missing-parent creation,
  existing-file/directory refusal and absolute outside-root refusal all settle
  correctly;
- real mouse selection followed by Enter activates Save without composer
  leakage;
- a 52 × 30 viewport remains operable, the light theme remains legible, and a
  `NO_COLOR` run contains no color SGR sequences;
- a 185-character destination visibly wraps and commits successfully;
- a 73,026-byte rendered conversation exports exactly in the current session
  and after restart with matching SHA-256
  (`ee83130543bbbeda5dde0119cfde139ca10375bcf6fe21964f0f59159333e166`);
- the same payload's clipboard branch fails explicitly above the 64 KiB
  transport bound and emits no OSC 52 payload; and
- the host clipboard was not touched.

The Claude and dshx chooser/form captures were visually inspected at the same
110 × 42 terminal-cell viewport. Labels, ordering, selection hierarchy,
filename input and keyboard hints correspond. No P0, P1 or P2 issue was found
within those paired states.

## Intentional differences

- dshx keeps its own application header, composer, model/status footer and
  color tokens rather than copying Claude's shell chrome.
- dshx generates a timestamped `dshx-conversation.txt` suggestion. It does not
  reproduce Claude's fixture-specific accidental local-command-derived title.
- Existing targets are rejected with the stable create-only message
  `the destination already exists or changed`; a directory is never replaced.
  Claude's captured directory target exposes raw `EISDIR` text, but the source
  evidence did not establish an overwrite contract.
- dshx confines file export to the active allowed filesystem roots. The Claude
  source did not test arbitrary absolute destinations, so weakening dshx's
  authority boundary would be unsupported.

## Acceptance disposition

- **Pending:** the real terminal retains the chooser and editable filename form.
- **Working/cancelling:** the production panel/app lifecycle is exercised with
  the controlled slow filesystem backend; cancellation settles and leaves no
  late file.
- **Completed/failed/cancelled:** the real terminal retains successful file and
  clipboard completion, create-only/path/directory/oversize failures, and user
  cancellation.
- **Grouped/expanded:** not applicable to this modal command.
- **Long output/path:** the real terminal retains a wrapping 185-character path,
  an exact 73,026-byte current/replayed file, and an explicit oversized-clipboard
  failure without truncation or transport emission.
- **Keyboard/mouse:** the real terminal retains both interaction paths.
- **Narrow/light/no-color:** the real terminal retains all three environments;
  Claude source modes not exposed by the installed product are not fabricated.

No applicable export state remains unverified. The retained source comparison,
production-binary journeys and controlled slow-backend lifecycle jointly close
the mandatory UI gate.
