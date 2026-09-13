# heycode ↔ Claude Code parity

Findings from driving both TUIs side by side under tmux and comparing
screens. Reference: Claude Code v2.1.267.

## How to reproduce the comparison

```bash
tmux new-session -d -s compare -x 240 -y 50 './target/debug/heycode --trust-workspace'
tmux split-window -h -t compare 'claude'
tmux select-layout -t compare even-horizontal
tmux attach -t compare
```

Drive either pane with `tmux send-keys -t compare.1 '/mcp' Enter` and read it
back with `tmux capture-pane -p -t compare.1`. Pane 1 is heycode, pane 2 is Claude.

## Status line

| Element | Claude Code | heycode | Status |
|---|---|---|---|
| Dirty worktree marker | `master *` | `master *` | **fixed** |
| Token accounting at rest | `in:0 out:0` | `in:0 out:0` | **fixed** |
| Field separator | `\|` | `│` | open — style choice, see below |
| Context meter | absent | `context — · limit unknown` at rest, gauge once a budget lands | heycode ahead |
| Background agent count | `← 1 agent` in the mode row | `Tasks 3 · 1 running · 1 waiting` strip | heycode ahead, see below |
| Approval mode colour | one colour per mode | one colour per mode | **fixed** |
| Mode row | `⏵⏵ auto mode on (shift+tab to cycle)` | `Default · shift+tab` | open — wording only |

### Fixed

- **Dirty worktree marker.** `WorkspaceContext` carries a `dirty` flag probed
  with `git status --porcelain`; `workspace_status` renders `branch *`, and the
  screen-reader snapshot says `branch: master (uncommitted changes)`. An
  unreadable worktree reports clean — a marker we cannot substantiate is worse
  than no marker. Refreshes on the existing 30s ticker and after each turn.
- **Token accounting at rest.** The `in:/out:` field used to be omitted
  entirely until the first turn produced a `usage`. It now renders zeroes, so
  the accounting reads as live rather than as a field that has yet to appear.
- **Approval mode colour.** The controls row previously had two colours for
  five modes: warn for `full_access`, dim for everything else — so Default,
  Plan and Accepted edits were indistinguishable without reading the word.
  `permission_role` now gives each mode its own theme role. Measured live:
  Default `rgb(138,133,124)`, Plan `rgb(217,119,87)`, Full access
  `rgb(217,160,91)`, Accepted edits `rgb(127,176,105)`.

  Caveat: Plan and Full access are both warm (accent and warn resolve close
  together in this theme). Distinguishable, but not at a glance — worth
  re-pointing Plan at a cooler role if that matters.

### Open, needs a decision

- **Separator glyph.** heycode uses `│`, Claude uses `|`. Cosmetic; matching costs
  nothing but heycode's is arguably cleaner. No action without a call.
## Panels and modes

- **`/mcp`.** Both exist. Claude groups by scope (User / Built-in), shows
  per-server tool counts and a docs URL. heycode has richer sections
  (`auth · tools · resources · prompts · actions`) but no scope grouping and no
  help link.
- **`/agents`.** Divergent by design, not a gap. Claude **removed** its wizard —
  it prints a pointer at `.claude/agents/`. heycode has a live panel with provider
  readiness probes. **Do not fix toward Claude here.**
- **shift+tab mode cycling.** Verified equivalent. Both cycle modes; heycode
  suppresses cycling *and* its `· shift+tab` hint while a panel is open, which
  is consistent behaviour, not a bug.

### Retracted — checked and not a defect

- **Background agent count.** heycode does surface it, in a persistent strip
  (`Tasks 3 · 1 running · 1 waiting`) that is hidden at zero and pinned by
  `task_console_persistent_strip_counts_are_authoritative_and_accessible`.
  Claude puts a bare `← 1 agent` in the mode row; heycode's strip carries more.
  A second count in the status line would only duplicate it.
- **`/plugins`.** Not a bug. The earlier `unknown command /plugins` was
  self-inflicted: `Esc` closes the palette but leaves the typed `/` in the
  composer, so typing `/plugins` after it submitted `//plugins`. Sent cleanly,
  `/plugins` opens the Installed plugins panel as intended.
- **shift+tab hint hiding while a panel is open.** Cycling really is
  suppressed then — two shift+tabs with `/mcp` open changed nothing after the
  panel closed — so hiding the hint is consistent, not a gap.

## Bugs found that are not parity gaps

- **`--fake` cannot start.** `heycode --fake` dies before the TUI with
  `plugin 'routing' failed to activate at the apply stage; 4 later plugin(s)
  never ran: unknown inference provider 'openrouter'`. Without `--fake` the
  same config starts fine, so the fake provider path does not register the
  providers routing expects. **Open.**

## Not yet compared

Chat loop and rendering (composer, streaming, tool-call cards, diffs,
interrupts), then slash commands and the palette.
