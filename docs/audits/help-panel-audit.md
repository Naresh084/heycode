# Phase 2 help panel verification

Date: 2026-09-11 (Australia/Melbourne)

## Result

The native `/help` surface now has the same three-part information structure as
the current Claude reference: General, Commands, and Custom commands. The
bounded implementation and terminal QA pass in dark, light, and color-disabled
modes, including a 45 × 24 responsive state.

This closes the prior missing-Custom-tab structural gap. It does not claim that
the two products are pixel-identical: dshx intentionally adds typed search,
counts, source attribution, aliases, live unavailable reasons, and explicit
keyboard/scroll guidance. Those differences remain part of the broader
`P2-UI03` product-parity assessment.

## Implemented behavior

- General reports the active runtime/model and current resolved key bindings.
- Commands renders the one real typed command-catalog snapshot. Synopsis,
  description, aliases, timing, and unavailable reason stay aligned with the
  command that can actually execute.
- Custom commands contains only current package-backed registered plugin
  commands and admitted skill records. A skill is shown through the supported
  `/skill <name>` invocation; dshx does not invent a `/<skill-name>` alias.
- Built-in command sources are not relabeled as custom unless the current
  installed-package index proves that source is package-backed.
- Each custom row includes bounded, terminal-safe description and source
  attribution. Empty descriptions remain honest. A failed skill snapshot is
  called unavailable instead of being presented as an empty inventory.
- An actually empty custom inventory has an explicit empty state.
- Tab and Right cycle forward through all three tabs; BackTab/Shift+Tab and Left
  cycle backward. Mouse tab selection, keyboard and mouse-wheel scrolling,
  bounded search, empty search, Home/End, and narrow rendering remain local and
  read-only.
- Closing help preserves the composer draft. Paste and typing search the panel;
  they cannot run a command or create a model request.

## Final evidence

The final implementation captures use immutable binary
`tmp/cli-snapshots/eabd2225c5d5c94e/dshx`, SHA-256
`eabd2225c5d5c94e2d74c67e96d61c1d203937ea97d27f0cd3d993326fa9ff0d`.

- `tmp/terminal-evidence/help-theme-dark-20260911T100015Z-1fb407/`
- `tmp/terminal-evidence/help-theme-light-20260911T100015Z-c9a625/`
- `tmp/terminal-evidence/help-theme-no-color-20260911T100015Z-adac89/`

Each directory contains ten PNG/text states, the raw ANSI stream, durable
events, and `result.json`. The fixture discovers a real trusted project skill,
then verifies its `/skill audit-terminal` invocation and `project skill` source,
all three tabs, the complete 85-command catalog, alias search, an unavailable
command reason, mouse and keyboard navigation, narrow word-aware wrapping, and
zero inference requests. The color-disabled run emits no color SGR.

`tmp/terminal-evidence/help-theme-dark-custom-empty-20260911T100140Z-6cbdce/` repeats the same
journey without a skill or installed custom command and verifies the real empty
state. It also creates no inference request.

The corrected prompt-free Claude Code 2.1.268 source is
`tmp/terminal-evidence/command-reference-claude-help-20260911T095439Z-7c1337/`. Its seven states include
General, Commands, and Custom commands and send no inference prompt. This v4
capture uses `TerminalByteStream`: the older ANSI contained `CSI > 4 m`, which
plain pyte misread as underline even though a real terminal treats it as a
private keyboard-mode sequence.

`tmp/terminal-evidence/help-comparisons-20260911T100153Z-fc2952/` contains same-size 110 × 42 source and
dshx General, Commands, and empty-Custom comparison rasters. The populated dshx
Custom state is retained separately because the safe-mode Claude source had no
custom command to pair with it. Wide captures are 1132 × 914 pixels at 10 × 21
pixels per terminal cell plus 16 pixels of padding. Narrow captures are 45 × 24
cells and 482 × 536 pixels. No CSS or browser-density normalization applies.

## Verification and limits

- Five focused Rust help tests pass for live identity, aliases and unavailable
  reasons, draft preservation, three-tab keyboard/mouse behavior, custom search,
  real invocation/source rows, empty and partial-inventory states, sorting,
  deduplication, field bounds, and terminal control sanitization.
- `scripts/help_theme_terminal_check.py` passes the three final theme journeys and
  the additional empty-Custom journey against the immutable binary.
- `scripts/command_reference_pty.py` records the source without a conversation
  prompt or inference.
- A pre-final v7 fixture stopped only because its assertion expected the
  45-column count sentence on one row. The rendered product had correctly
  wrapped it; the final fixture asserts the complete visible fragments.
- Visual inspection found no open P0, P1, or responsive P2 issue in this help
  surface. The earlier inside-word splits are fixed with the shared styled word
  wrapper and hanging description indents.
- The accessible flat projection names the active tab and complete content, but
  screenshots alone do not establish full assistive-technology compliance.
