# Terminal reference baseline

The terminal comparison uses the installed Claude Code **2.1.269** binary, SHA-256 `c942e1228b93cb4d52183b3dfbc77f28264f35aa947acd9c0853d029164cf450`, for the refreshed Memory, Skills, Model and Effort surfaces. Preserve earlier captures as evidence of their recorded versions; do not mix their layout geometry into this baseline.

## Capture conditions

The current Memory and Skills captures use disposable home, configuration and workspace directories, project-local instruction and skill fixtures, no inherited account state, and the installed renderer default (`CLAUDE_CODE_NO_FLICKER` unset). Provider and proxy targets are closed loopback addresses. No model prompt was submitted. The source captures cover 126×46 and 60×24 terminals:

- Dark reference result (local-only evidence: `tmp/terminal-checks/20260912T023224Z-memory-skills-reference-dark/result.json`), Skills (local-only evidence: `tmp/terminal-checks/20260912T023224Z-memory-skills-reference-dark/06-skills-after-reload.png`), Memory (local-only evidence: `tmp/terminal-checks/20260912T023224Z-memory-skills-reference-dark/02-memory-open.png`).
- Light reference result (local-only evidence: `tmp/terminal-checks/20260912T023224Z-memory-skills-reference-light/result.json`), Skills (local-only evidence: `tmp/terminal-checks/20260912T023224Z-memory-skills-reference-light/06-skills-after-reload.png`).
- No-colour reference result (local-only evidence: `tmp/terminal-checks/20260912T023225Z-memory-skills-reference-no-color/result.json`), narrow Skills (local-only evidence: `tmp/terminal-checks/20260912T023225Z-memory-skills-reference-no-color/06a-skills-after-reload-narrow.png`).
- Light routing controls (local-only evidence: `tmp/terminal-checks/20260912T015543Z-claude-routing-light/result.json`), including the combined Model and Effort picker.

The current source places these Memory and Skills panels at the bottom of the viewport. Its two-row Skills list occupies nine rows at 126 columns; the full keyboard caption wraps at 60 columns. Admission states use `✔ on`, `● name-only`, `◯ user-only` and `✘ off`. Ordinary state changes are represented in their rows, with no extra success footer. Source provenance and token quantities may differ with the actual native fixtures and must remain truthful.

## Observed semantic colours

The native palette keeps one central role map. These values were read from source ANSI captures, rather than estimated from screenshot pixels.

| Role | Dark | Light |
|---|---|---|
| Accent | `#b1b9f9` | `#5769f7` |
| Success | `#4eba65` | `#2c7a39` |
| Error | `#ff6b80` | `#ab2b3f` |
| Warning | `#ffc107` | `#966c1e` |
| Dim | `#999999` | `#666666` |
| Border | `#888888` | `#999999` |
| Prompt background | `#373737` | `#f0f0f0` |
| Prompt-band `❯` glyph | `#505050` | `#afafaf` |

The prompt-band glyph values were read from the SGR sequences that precede `❯ /memory` and `❯ /skills` in the dark and light `04-reload-skills-result.ansi` captures; dshx carries them as the `prompt-glyph` theme role. Light accent, warning and border contrast on the captured `#f8f9fb` terminal background is approximately 4.18:1, 4.47:1 and 2.70:1. Matching this source palette is not a claim that every role reaches WCAG text contrast of 4.5:1. Regression tests preserve explicit measured floors; the separate high-contrast theme remains available. Unknown source tokens, such as light inline-code colours, are not presented as matched.

## Evidence boundaries

Source discovery screens do not prove provider dispatch, subscription availability or native persistence. Native session/default routing, stale-owner rejection, combined model/effort mutation and restart require their own controlled tests and actual CLI journeys. Likewise, previous native skill admission and instruction security evidence remains valid for those unchanged owners but cannot substitute for captures of a newer renderer.

## Additional interaction references

The unequal-cost token-sort probe (local-only evidence: `tmp/terminal-checks/20260912T025634Z-skills-token-order-reference/result.json`) confirms that `t` selects token ordering, with the larger catalog entry first. The Skills input probe (local-only evidence: `tmp/terminal-checks/20260912T030228Z-skills-input-reference/result.json`) confirms that clicking another row does not select or change admission, paste starts search, Escape clears a non-empty search without closing the panel, Enter returns from search to selection without cycling admission, and Escape then closes the normal selection view. Sorting resets selection to the first row. The source's `No changes` dismissal refers to admission changes, not the sorting preference.

The footer and shortcut reference (local-only evidence: `tmp/terminal-checks/20260912T030812Z-footer-reference/result.json`) records idle footer, standalone `?`, Backspace, a question mark inside an ordinary unsent draft, and Escape. Standalone `?` opens a compact multi-column shortcut list below the composer; it does not insert a prompt character. Backspace closes that list. The capture submitted no prompt and contains no user/assistant journal messages. These are pending native comparison boundaries, not accepted native behavior.

## Command panel and account references

On 2026-09-12 the same isolated 2.1.269 binary produced prompt-free references for the dialog-style commands `/permissions`, `/theme`, `/hooks`, `/sandbox`, `/diff`, `/context`, `/keybindings`, `/statusline`, `/voice`, `/agents`, `/workflows`, `/config`, `/mcp`, `/tasks`, `/plan`, `/cost`, `/export`, `/color`, `/focus`, `/scroll-speed`, `/autocompact`, `/release-notes` and `/help` through [`claude_command_panel_terminal_reference.py`](../../scripts/claude_command_panel_terminal_reference.py), and for `/login` and `/logout` through [`claude_account_commands_terminal_reference.py`](../../scripts/claude_account_commands_terminal_reference.py). Each run is a fresh process with a disposable home, configuration and workspace, inherited `CLAUDE_CODE_*` and credential variables removed, closed loopback provider and proxy targets, `$EDITOR` and `$BROWSER` forced to `/usr/bin/false`, a 110×42 viewport and the dark theme. Captures live under `tmp/terminal-checks/<timestamp>-<command>-reference-dark/` as `00-start`, `01-command-menu`, `02-opened` and `03-after-escape`.

Observed conventions: dialog commands open a bottom-anchored panel under a full-width `▔` rule with a three-space indent, an optional tab strip in the title row, numbered options with a `❯` cursor and `✔` current mark, and a dim hint row such as `Enter to select · Esc to cancel`. Dismissal that changes nothing is reported as a receipt (`⎿  Scroll speed unchanged`, `⎿  Auto-compact window unchanged: auto`, `⎿  Export cancelled`, `⎿  Login interrupted`); inline results use the same `⎿` form (`⎿  Enabled plan mode`, `⎿  Focus view enabled`, `⎿  Session color set to: orange`). `/plan` switches the footer to `⏸ plan mode on (shift+tab to cycle) · ← for agents`.

Boundaries: in `--bare` isolation `/voice` is reported as an unknown command, `/statusline` answers `Not logged in · Please run /login`, `/keybindings` hands off to `$EDITOR`, and `/agents` reports that its wizard was removed; those states have no isolated panel reference. `/logout` prints `Successfully logged out from your Anthropic account.` and exits the process. No login method was selected and no prompt was submitted, so no OAuth, browser or model traffic occurred.

The dshx counterparts were captured with [`command_panel_terminal_check.py`](../../scripts/command_panel_terminal_check.py) and [`account_commands_terminal_check.py`](../../scripts/account_commands_terminal_check.py) against the fake provider with zero inference.
