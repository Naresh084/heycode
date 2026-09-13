# Design QA — terminal runtime integration

The 2026-09-06 redesign was visually inspected from the production binary in a
110 × 40 pseudo-terminal with a real Claude session, plus a 64 × 22 narrow
terminal and a multiline bracketed-paste draft. Screenshots were reconstructed
from the actual ANSI stream, preserving foreground/background colors and text
styles. They are terminal-cell captures rather than native window screenshots.
The supplied Claude and heycode references were reviewed before the redesign.

The inspected idle and live chat captures show the orange animated cat,
concrete `claude-opus-5[1m]` model ID, effort and Claude identity; compact workspace;
full-width shaded user message; assistant bullet; inline composer prompt; and
separate workspace/model and approval/action footer rows. The multiline draft
keeps its indentation, continuation rail and cursor within the composer. Narrow
layouts retain the composer and primary model/approval information. Cat frames
and disabling animation are regression-tested; a still capture cannot show the
animation itself.

Real-terminal checks cover startup, bracketed paste, submission/reply, deliberate
double-Ctrl+C exit, command display, typing `/exit` and `/exist` without submitting,
and local endpoint setup with masked credentials: all eight scenarios pass.
A real Claude control probe verified startup identity, the advertised effort
choices, changing low to medium and reopening the model picker. A live tool
journey verified a read approval, automatic `ls` with exit code zero and task
progress; completion is checked through actual tool results and session events.

The evidence folder is `/tmp/dshx-quality-20260906`, including
`cat-real-idle.png`, `cat-real-chat.png`, `cat-real-working.png`,
`cat-real-effort.png`, `redesign-composer.png`, `redesign-narrow.png`,
the live tool logs and test logs.
Fresh captures use isolated heycode settings and do not alter the user's saved route.

The final real Claude chat capture includes measured `5.7k / 1M` context.
Raw SDK evidence and app-server propagation agree on the 1,000,000-token window;
input, cache-write and cache-read tokens are taken from the latest request,
not summed across a multi-tool turn. The post-integration live TUI journey also
passed read approval, automatic shell execution, task progress, effort switching
and retained backend identity.

Workspace gates: 4,170 Rust tests, eight TypeScript tests and nine Python tests
(including the eight PTY scenarios) passed with no failed or ignored tests.
Workspace formatting, all-target warnings-denied Clippy, generated references,
documentation checks and the final binary build passed.

The latest live Claude proof covers the shared question tool in automatic
approval mode: selecting Amber, typing Teal directly from the initial choice,
and cancelling with Esc all return the exact result and resume the model.
The card has content-sized horizontal rails, descriptions, visible keyboard
hints and an Other entry; normal composer input is hidden while it is active.
The final captures also confirm each result appears once in the transcript.
See `question-choices.png`, `question-custom-entry.png` and
`question-cancel-complete.png`. These live checks cover Claude; native
tool-capable providers use the same tested broker, while delegated support
depends on the adapter exposing configured host tools.

The subsequent footer refinement follows the supplied Claude status-line
reference: branch without a redundant label in green, concrete model in purple,
measured input/output counts in cyan and quieter separators/context. Approval
mode occupies the second row; effort is aligned above the composer on the right.
A real Claude PTY check cycled Shift+Tab through ask, auto and deny, retained an
unsubmitted draft and confirmed the committed mode in the footer. Shortcut
acknowledgments stay out of the transcript. See `footer-real-chat.png` and
`footer-real-draft.png`.


Logout was reproduced against a real Claude session, then verified after the
fix: immediate full Welcome, Welcome after restart, and explicit reconnection
with no previous conversation content. A production native-provider PTY journey
used an isolated authenticated LM Studio-shaped HTTP fixture, entered a masked
key, selected its model, logged out, verified saved-key deletion and restarted
into Welcome. This verifies setup and logout, not live LM Studio inference.
Captures are `logout-after-same-process.png`, `logout-after-restart.png`,
`logout-after-reconnected.png` and `logout-native-after.png`. The ANSI replay
harness clears its screen on alternate-buffer entry to model terminal behavior
across the logout restart.


The permission picker now offers Full access, Accepted edits and Default. Auto
is removed from both the picker and command catalog. The three-row menu, selected
mode, narrow layout and Shift+Tab path share the same policy owner. Accepted edits
cards offer Accept, Accept all future edits and Reject. The explanatory line
limits future approvals to the identical tool and complete inputs; Default has
one-time Accept and Reject.

The real native terminal fixture executes five calls in each mode: identical
inputs, changed inputs, then repeats after one-time and future approvals. All 15
read results succeed. Accepted edits asks three times, Default five times, Full
access zero times. Captures include `permission-exact-accepted_edits-ask-1.png`,
`permission-exact-default-complete.png` and `permission-exact-full_access-complete.png`.

An isolated OpenRouter-shaped HTTP fixture serves a public model catalog and a
separate authenticated current-key endpoint. The production terminal starts with
an empty key field and separate hint, renders only the first five and final key
characters, rejects an invalid key without saving it, and presents an empty retry
field. A valid replacement proceeds through model selection into a connected
conversation. Revoking that saved key blocks a subsequent connection attempt
despite the available public catalog; restart requires repair. The complete key
never appears in terminal output. Captures include `key-validation-empty.png`,
`key-validation-entered.png`, `key-validation-retry.png`,
`key-validation-revoked.png` and `key-validation-restart.png`.

The eight existing terminal scenarios also pass, including local-server invalid
key rejection and direct retry. Unit/integration regressions cover short/long
masking, empty retries, cancellation without persistence, custom references across
recomposition, exact approval matching, concurrent identical calls and mode
switches. The OpenRouter probe follows its documented
[current-key authentication endpoint](https://openrouter.ai/docs/api/api-reference/api-keys/get-current-key).

## Phase 2 terminal navigation, focus, and color addendum — 2026-09-11

### Audit scope and target

This addendum records the final Product Design QA pass for the bounded Phase 2
terminal surface owned by this work: the bottom agent navigator, agent switching,
foreground drafts, the background browser, the agent inspector, `/focus`, and
`/color`. The target was the current Claude Code terminal interaction hierarchy
while preserving heycode's approved five-agent and persistent-footer product model.
It is not a blanket acceptance of every tool or slash-command surface in
`P2-UI03`.

### Source and implementation evidence

The source is a fresh Claude Code 2.1.268 run in a disposable workspace:

- Agent navigation source: `tmp/terminal-evidence/claude-terminal-20260911T081009Z-7bed76/`
  (`01-main-agent-switcher.png`, `02-background-list.png`,
  `02-background-results.png`, `03-agent-inspector.png`, and
  `04-agent-foreground-draft.png`).
- Focus/color source: `tmp/terminal-evidence/claude-focus-color-20260911T075049Z-03165a/`
  (`01-normal-completed-turn.png` through `06-color-default.png`).

The final heycode implementation evidence was rendered from immutable binary
`tmp/cli-snapshots/3367999eec7f6af2/dshx-20260911T090955Z-fef429`, SHA-256
`3367999eec7f6af2bed6b5dbe610b6be59124aea6281183bc1e2c2d6eaa3fb55`:

- Dark navigation: `tmp/terminal-evidence/navigation-dark-20260911T091055Z-b1e0f6/`.
- Light navigation: `tmp/terminal-evidence/navigation-light-20260911T091055Z-bd8bda/`.
- Color-disabled navigation: `tmp/terminal-evidence/navigation-no-color-20260911T091022Z-a404bc/`.
- Dark focus/color: `tmp/terminal-evidence/dshx-focus-color-dark-20260911T091022Z-3f77fa/`.
- Light focus/color: `tmp/terminal-evidence/dshx-focus-color-light-20260911T091022Z-693221/`.
- Same-input source/implementation composites used for the comparison pass:
  `tmp/terminal-evidence/comparisons-20260911T085846Z-f3f7ad/`.

### Viewport and normalization

- Full-state source and implementation captures use the same 100 × 45 terminal
  cell viewport and a 1032 × 977 PNG raster.
- Responsive evidence uses 52 × 30 terminal cells and a 552 × 662 PNG raster.
- The screenshot renderer uses the same 10 × 21 pixel terminal-cell density and
  16-pixel outer padding for both products. CSS pixels and browser device-pixel
  ratio do not apply. No comparison image was density-scaled before inspection.
- The combined comparison images place the actual source and implementation
  rasters together; they are review aids, while the original PNGs above remain
  the pixel evidence.

### States inspected

The full comparison covered the main agent switcher, keyboard-selected agent,
retained child tool result, isolated child draft, restored parent draft,
background list/results, inspector, narrow active-agent state, normal transcript,
focused transcript, restored transcript, random/cyan/default session color, and
restart behavior. Dark, light, and color-disabled navigation were inspected.
The final light `/color cyan` capture resolves to `#0e7490` rather than the dark
surface's `#22d3ee`; title, command rows, status text, and the input cursor remain
legible without using color as the sole state cue.

### Findings and iteration history

- P0: none found.
- P1: none found.
- P2, resolved: the initial session-color palette used bright dark-surface
  accents on a light terminal. Prompt color resolution now selects a darker
  light-surface palette. The final light run confirms cyan `#0e7490`, default
  `#20242c`, persistence of focus, and reset of the session-local color after
  restart.
- Test-only issue, resolved: the first color-disabled navigation harness treated
  a cursor-position CSI sequence as a color sequence. The parser now inspects
  only SGR parameters; the final color-disabled run emitted no foreground or
  background color SGR.
- Intentional product differences, not defects: heycode shows five agents rather
  than the two-agent reference journey, keeps its approved persistent multi-agent
  footer, uses the heycode banner, and retains accepted local command rows in focus
  view. The information hierarchy, keyboard/mouse ownership, draft isolation,
  terminal rhythm, and bounded narrow layout remain coherent in those states.

### Final result

**PASSED — scoped Phase 2 terminal navigation, focus, and color visual QA.** No
open P0, P1, or P2 issue remains in this bounded surface. The broader mandatory
`P2-UI03` every-tool/every-command Claude parity gate remains **BLOCKED/open**
until its separate state matrix and live-provider comparisons are completed.

## Phase 2 help/catalog follow-up — 2026-09-11

The corrected Claude Code 2.1.268 General, Commands, and Custom commands source
is in `tmp/terminal-evidence/command-reference-claude-help-20260911T095439Z-7c1337/`. The immutable heycode
dark, light, and color-disabled implementation captures are in
`tmp/terminal-evidence/help-theme-{dark,light,no-color}-final-v9/`; actual same-size
General, Commands, and empty-Custom rasters are combined in
`tmp/terminal-evidence/help-comparisons-20260911T100153Z-fc2952/`. They use
`tmp/cli-snapshots/eabd2225c5d5c94e/dshx`, SHA-256
`eabd2225c5d5c94e2d74c67e96d61c1d203937ea97d27f0cd3d993326fa9ff0d`.
The v4 source and final heycode fixture use `TerminalByteStream`, so the source's
private keyboard-mode sequence is no longer misrendered as underline.

Source and wide implementation states use 110 × 42 terminal cells and
1132 × 914 pixels at the same 10 × 21 cell density plus 16-pixel padding. The
narrow heycode state uses 45 × 24 cells and 482 × 536 pixels. CSS and browser density
normalization do not apply. Claude narrow/light/no-color source states were not
captured, so those are standalone heycode checks rather than paired evidence.

Dark, light, and color-disabled runs pass General/Commands/Custom commands, the
real 85-command catalog, an actual admitted project skill with supported
invocation and source attribution, mouse/keyboard tabs, alias and custom search,
live unavailable reasons, narrow scrolling, and zero model requests. A separate
dark run passes the actual empty-Custom state. No P0 or P1 was found.

- P2 resolved, structural: Claude and heycode both expose General, Commands, and
  Custom commands in the same order. The heycode Custom tab is backed only by
  current installed package commands and admitted skills, with explicit empty
  and partial-inventory states.
- P2 resolved, responsive polish: the first comparison split `connected`,
  `shortcuts`, and `/keybindings` inside words at 45 columns. The final captures
  use word-aware styled wrapping with hanging command-description indents;
  words stay intact, all content remains reachable, and the footer stays
  visible.

**Final scoped help result: PASSED for local function, interaction, theme,
three-tab structure, and responsive behavior.** Exact pixel identity is not
claimed because the heycode product intentionally keeps search, counts, source and
unavailable-state detail, aliases, and key guidance. The broader `P2-UI03`
every-tool/every-command gate outside help remains open. Detailed evidence and
boundaries are in `docs/audits/help-theme-comparison.md`.
