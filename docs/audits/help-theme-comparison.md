# Phase 2 help/catalog theme comparison

Date: 2026-09-11 (Australia/Melbourne)

## Scope and disposition

This is a bounded visual and interaction audit of the read-only `/help` panel:
General, Commands, Custom commands, the live command and custom registries,
search, aliases, unavailable reasons, wide/narrow behavior, and
dark/light/color-disabled rendering. It does not claim parity for command
execution states outside help.

**Functional, theme, and three-tab structural result: passed.** The dshx panel
remains local-only, keyboard/mouse operable, readable in all three color modes,
and unable to submit a model message. The missing Custom commands tab and the
narrow word-wrapping issue found in the first comparison pass are both resolved.
Strict pixel identity is not claimed because dshx intentionally retains its
typed search, counts, attribution, unavailable reasons, and key guidance.

## Source and implementation evidence

The current reference is the prompt-free Claude Code 2.1.268 capture in
`tmp/terminal-evidence/command-reference-claude-help-20260911T095439Z-7c1337/`:

- `04-help.png` — General;
- `04b-help-commands.png` — Commands; and
- `04c-help-custom.png` — Custom commands.

The final dshx captures use immutable
`tmp/cli-snapshots/eabd2225c5d5c94e/dshx`, SHA-256
`eabd2225c5d5c94e2d74c67e96d61c1d203937ea97d27f0cd3d993326fa9ff0d`:

- `tmp/terminal-evidence/help-theme-dark-20260911T100015Z-1fb407/`;
- `tmp/terminal-evidence/help-theme-light-20260911T100015Z-c9a625/`; and
- `tmp/terminal-evidence/help-theme-no-color-20260911T100015Z-adac89/`.

Each contains ten PNG/text states, the ANSI transcript, durable events, and a
`result.json` carrying the binary hash and source paths. A separate dark run in
`tmp/terminal-evidence/help-theme-dark-custom-empty-20260911T100140Z-6cbdce/` proves the honest
empty-Custom state. The actual source and dark dshx General, Commands, and
empty-Custom rasters are placed together in
`tmp/terminal-evidence/help-comparisons-20260911T100153Z-fc2952/`.

The Claude v4 source uses the corrected `TerminalByteStream`. The prior raw
ANSI contained `CSI > 4 m`, a private keyboard-mode sequence that plain pyte
incorrectly rendered as underline; the final decorative comparison therefore
uses only the corrected v4 rasters.

Wide source and implementation captures use 110 × 42 terminal cells and
1132 × 914 PNGs, rendered at 10 × 21 pixels per cell with 16 pixels of outer
padding. The dshx responsive state uses 45 × 24 cells and a 482 × 536 PNG. CSS
pixels and browser device-pixel ratio do not apply. No source narrow, light, or
color-disabled Claude capture was available, so those dshx checks are standalone
quality evidence rather than paired parity proof.

## Verified behavior

- General exposes the active runtime/model and current resolved key bindings.
- Commands is populated from the real 85-entry catalog snapshot; the displayed
  total matches the complete unfiltered result.
- Custom commands is populated from actual installed package-backed command
  sources and admitted skill records. The final fixture discovers a real
  project skill and exposes only its supported `/skill audit-terminal`
  invocation with `project skill` attribution. A second run proves the empty
  registry state.
- Mouse and keyboard tab changes cycle all three tabs. Typing searches the
  active Commands or Custom commands catalog.
- Searching `reset` returns the canonical `/new [title...]` row and its
  `/clear, /reset` aliases. Searching `background` exposes the live unavailable
  reason for the current non-hosted fixture instead of advertising false support.
- At 45 × 24, the active tab, search/result content, and close/scroll footer
  remain present. End scroll reaches the final General guidance.
- Dark and light themes retain semantic hierarchy. The color-disabled run emits
  no foreground or background color SGR; bold/underline, labels, spacing, and
  text preserve the active state.
- The modal creates no `user/message` or `request/header` event. The three runs
  made zero inference requests.

## Comparison findings

### P0 and P1

None found.

### P2 — three-tab topology, resolved

Claude and dshx now both present General, Commands, and Custom commands in the
same order. The dshx Custom tab is not cosmetic: it is backed by actual installed
package commands and admitted skills, and it renders explicit empty and
unavailable states. Wide and narrow mouse targets and forward/backward keyboard
cycling cover all three tabs.

### P2 — narrow word wrapping, resolved

The first 45-column comparison split `connected`, `shortcuts`, and
`/keybindings` inside words. Help copy now uses the shared styled word wrapper,
including a hanging indent for command descriptions and fallback for an
unbroken token wider than the content area. The final narrow alias result and
End-scrolled General captures keep every word intact, preserve hierarchy, and
retain the close/scroll footer. Exact scroll accounting and content reachability
continue to pass.

### Non-blocking product differences

dshx adds explicit search, total/matching counts, aliases, source attribution,
live unavailable reasons, and keyboard/scroll guidance. Claude's default list
instead uses a current-row arrow. These differences make the dshx catalog more
explicit, but they are not pixel-equivalent to the source.

## Test fixture

`scripts/help_theme_terminal_check.py` owns this isolated evidence. It uses the real
dshx CLI/TUI with disposable settings/workspace state, discovers a real project
skill, never submits a normal composer prompt, verifies the durable event log,
checks real mouse and keyboard paths across all three tabs, resizes the PTY, and
parses only SGR sequences for the color-disabled assertion. `--empty-custom`
repeats the journey without a skill or installed custom command.

Two pre-final fixture attempts failed because their text expectations assumed a
different command description and an unsplit narrow substring. The v4 audit then
surfaced the real inside-word wrapping defect. A v7 attempt then stopped because
its assertion expected a correctly wrapped narrow count on one row. The final
v9 fixture asserts the visible fragments, the actual typed registries, and all
content after scrolling while capturing corrected word-aware wrapping in all
three color modes.
