# Terminal compatibility integration — 12 September 2026

This audit records one integrated verification of the shared footer, command receipts, OpenRouter effort metadata, session management, core tool cards and dialog-panel alignment against the pinned Claude Code 2.1.269 reference described in [terminal-reference-baseline.md](terminal-reference-baseline.md). Row-level evidence and statuses live in the [tracker](../terminal-compatibility-tracker.md).

## Verified snapshot

`tmp/cli-snapshots/310bbc0aa9960cf4/dshx`, SHA-256 `310bbc0aa9960cf4e1bd11c3fd84459debb08c6ce885b5d1c88a4da411d1fb5b` (build receipt (local-only evidence: `tmp/cli-snapshots/build-receipt.json`)). On this tree `cargo fmt --all -- --check` was clean, strict clippy passed for `dshx-tui`, `dshx-status`, `dshx-onboarding`, `dshx-routing`, `dshx-ui`, `dshx-extension-host`, `dshx-agent`, `dshx-llm` and `dshx-provider-openrouter` (log (local-only evidence: `tmp/verification-logs/20260912T052325Z-integrated-clippy.log`)), and the TUI (216 unit, 371 integration), status, onboarding, routing, UI and extension-host suites passed (log (local-only evidence: `tmp/verification-logs/20260912T052402Z-integrated-tui-tests.log`)).

## Journeys on the snapshot

All journeys used loopback fixtures or the built-in fake provider and made no commercial request.

| Journey | Result | Evidence |
| --- | --- | --- |
| Footer and `?` shortcuts | 12/12 | `tmp/terminal-checks/20260912T053157Z-footer-shortcuts-dark` |
| Command receipts | 5/5 | `tmp/terminal-checks/20260912T053204Z-command-receipts-dark` |
| Core tool cards and approvals | captured, 11 loopback requests | `tmp/terminal-checks/20260912T053217Z-core-tools-dark` |
| Memory and Skills (dark, light, no-colour) | 25/25 each | `tmp/terminal-checks/20260912T053457Z-memory-skills-dark`, `…053551Z-memory-skills-light`, `…053330Z-memory-skills-no-color` |
| Combined model/effort scope | 13 checks | `tmp/terminal-checks/20260912T053019Z-openrouter-model-effort` |
| Session lifecycle | passed | `tmp/terminal-checks/20260912T053057Z-session-lifecycle-dark` |
| Account commands (`/login`, `/connect`, `/logout`) | captured | `tmp/terminal-checks/20260912T053136Z-account-commands-dshx-dark` |
| Dialog commands, dark/light/no-colour | captured | `tmp/terminal-checks/20260912T0526…0545Z-<command>-dshx-<variant>` |

The dialog captures come from [`command_panel_terminal_check.py`](../../scripts/command_panel_terminal_check.py), which now replays the bytes painted before the composer became ready (earlier captures from the same harness had blank header rows for that reason; the panels themselves were unaffected) and accepts `--theme`. The Claude references are the prompt-free captures listed in the baseline.

## What now matches the reference

- One dim footer row per permission mode, the standalone `?` shortcut list, and the effort/session-name composer chrome.
- Content-width `❯` command bands with the measured `prompt-glyph` colour, `  ⎿  ` receipts with five-cell continuation, `⏺ Unknown command: /x`, and truthful panel-close receipts including Skills `No changes`.
- Bottom-anchored dialog panels (`▔` rule, three-space indent, numbered `❯` options, `✔` current mark, hint rows) for `/permissions`, `/theme`, `/hooks`, `/keybindings`, `/scroll-speed`, `/config`, `/help`, `/tasks`, `/workflows`, `/agents`, `/login`; receipt forms for `/plan`, `/color`, `/focus`, `/diff`, `/mcp`, `/release-notes`, `/logout`; the `/context` grid and `/cost` `Session` block.
- Core tool cards: `⏺ Read/Write/Update/Search/Bash(...)` titles, `Read N lines`, `Wrote N lines to …` with numbered content, `Found N files/lines`, `Error: Exit code N`, bare grouped summaries, and per-tool approval cards with numbered choices.
- Rewind picker and confirmation, resume picker frame/search/rows, `/branch` recovery route, `/clear`.
- Per-model OpenRouter effort vocabularies with truthful "Effort unavailable" for unpublished models.

## Live comparison

The user-authorized C1 scheduling run compared real Claude Code (`claude-opus-5`) with real dshx on its saved OpenRouter route: identical call sequences, both schedules deleted, empty final lists, nothing fired (run audit (local-only evidence: `tmp/terminal-evidence/schedule-live-20260912T040531Z-authorized/c1-live-audit.md`), 18/18 assertions).

## Open items

- Approval dialog still sits under the footer instead of replacing the composer; no global `ctrl+o` verbose transcript; bold counts/filenames and `Thought for <1s` rows differ from the reference; denied wording differs.
- `/sandbox` and `/autocompact` are text receipts, not panels; `/cost` is still the Settings Usage tab; `/hooks` lacks the event list; `/config` keeps its tab strip; `/scroll-speed` shows a ruler rather than sample text; `/release-notes` uses `⎿` instead of `⏺`.
- Resume picker keeps dshx filter/hint rows and lists the current session; the `/branch` receipt is plain and omits the parent title.
- Remaining tool families (agents, structured tasks, web, notebook, LSP, MCP resources, monitor, workflow, worktree, plan mode, findings, files, questions, scheduling cards) and the remaining commands still need paired references; P2-WS01 needs authorized live provider calls.
- An external agent system began further edits in this working tree after the snapshot (sandbox/autocompact panels, a verbose-transcript module, branch parent titles, a panel-title colour). They are not part of the verified snapshot and were not reviewed here.
