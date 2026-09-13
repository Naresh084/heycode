# heycode parser fuzzing

This standalone cargo-fuzz package exercises only public heycode package
boundaries. It is intentionally outside the root workspace so fuzz-only
dependencies and profiles cannot change the shipping dependency graph.

Targets and asserted invariants:

- `session_event_parser`: a successfully opened bounded JSONL stream has
  contiguous sequence numbers, deterministic projections, and a semantic
  serialize/open round trip.
- `config_parser`: classification and full parsing are deterministic, accepted
  schema evidence agrees with the parsed document, plugin order is preserved,
  every MCP transport resolves deterministically, and migration planning is
  deterministic and read-only.
- `provider_stream_parser`: OpenAI-compatible provider output is invariant
  across successful raw-byte fragmentations; usage/finish markers are unique,
  ordered, and terminal.
- `mcp_protocol_parser`: handshake, notification, and rich tool-result parsing
  are deterministic; successful UI projection preserves block/error/null
  structure while remaining bounded.
- `render_parser`: the public TUI draw path parses bounded Markdown into a
  deterministic fixed-size buffer with no control-bearing cells; the shared
  screen-reader projection remains control-free and within its line caps.

Each target also enforces its own input ceiling. LibFuzzer enforces a five-second
per-input deadline except for `render_parser`, whose bounded 20-second deadline
covers the one-time Syntect regex initialization observed under ASan; steady
inputs remain subject to that same hard ceiling. Use the wrapper so both the
writable corpus copy and crash inputs remain in an ephemeral private directory
and are never retained or uploaded:

```sh
fuzz/run-target.sh session_event_parser runs 256 12648430
fuzz/run-target.sh provider_stream_parser seconds 60 195936478
```

The wrapper caps one invocation at 100,000 runs or 900 seconds and accepts only
an unsigned 32-bit seed. CI adds a job-level deadline and 2-GiB RSS / 1-GiB
allocation limits.

The checked-in corpus under `corpora/` is synthetic and credential-free. A
short smoke is evidence only for those bounded runs. Q12's crash-free
continuous-run acceptance requires observed scheduled/hosted runs; the target
and workflow definitions alone do not satisfy it.

## Render boundary result

On 2026-08-31 the checked-in `ansi-controls.txt` seed reached the public
`heycode_tui::render::draw` path and initially left ESC/OSC bytes in terminal
cells. The production Markdown boundary now preserves LF/CRLF structure and
visible text while replacing every other control before pulldown-cmark or
syntect can create a terminal span. The seed and a focused TUI unit regression
remain permanent controls.

The complete local PR smoke is green after that repair: all four structural
targets completed 128 fixed-seed libFuzzer/ASan runs and render completed its
8-run high-cost seed set; the five deterministic chaos scenarios and both
isolated warnings-denied clippy gates also pass. This is still not Q12's
continuous hosted evidence—the scheduled matrix has not run.
