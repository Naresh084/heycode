# TUI approval and tool-card verification — 2026-09-08

The native TUI now supports direct user exits from Plan, session-wide file-edit
permission, compact expandable tool cards, and approval decisions on the original
card. Foreground shell completion no longer injects duplicate output as a user
message. Empty task inventories hide the task strip; Ctrl+T remains available.
Routine mode and runtime messages stay in session diagnostics.

The agent still uses `exit_plan_mode` with full-plan review to request its own
transition. A user can switch directly, including while that review is pending.
Accepted edits permits native file reads and edits; commands and other tools
still require approval. Changing back to Default restores per-call prompts.

## Results

The workspace suite passed 4,488 tests with 9 ignored. The final focused TUI run
passed 65 unit tests and 280 integration tests (2 ignored). Clippy, formatting,
generated-document freshness, and the final debug build passed. All five Plan
PTY journeys and the compact tool-card PTY journey passed.

## Reproducible checks

- `cargo test -p dshx-tui -p dshx-agent -p dshx-status --all-targets`
- `cargo test --workspace --all-targets`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo fmt --all --check`
- `python3 scripts/generate_doc_references.py --check`
- `cargo build -p dshx-cli --bin dshx`
- `python3 scripts/plan_review_pty.py --output tmp/live-product-audit/plan-review-ui-20260908T023038Z`
- Run `scripts/tool_cards_pty.py` with a Python environment containing `pyte` and
  Pillow. It writes terminal captures and results under
  `tmp/live-product-audit/tool-cards-pty`.

The two terminal scripts run the production binary on real PTYs with a local
OpenRouter-shaped streaming fixture, temporary homes, and real filesystem and
shell tools. They do not contact a paid model or establish parity across every
external runtime.

The Plan journeys cover both acceptance policies, rejection with feedback,
Escape, and manual Shift+Tab during review. The tool-card journey verifies two
subsequent file writes without prompts, command rejection without a filesystem
effect, command acceptance, retained output expansion/collapse by mouse, and
exactly one durable user message. PNGs render the captured terminal cells.

Backend and TUI regressions additionally cover manual exit during idle and busy
states, failed policy transitions, persisted Plan state, pending-review
withdrawal, promotion of foreground jobs, attributed job-history projection,
human messages resembling job output, late tool-event ordering, keyboard
expansion, and preserving the draft.
