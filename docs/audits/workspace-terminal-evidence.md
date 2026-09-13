# Workspace commands: immutable CLI terminal evidence

Date: 2026-09-11. Scope: the actual `/add-dir` and `/cd` terminal paths, instruction/file authority after a change, and same-session plugin reload after changing cwd. This adds evidence to the existing workspace service/composition tests; it does not replace them or claim all Phase 2 UI gates are complete.

## Exact artifact and isolation

- dshx executable: `tmp/cli-snapshots/ec95c4bbc96501ca/dshx`.
- Verified SHA-256: `ec95c4bbc96501ca627e470f0126915387fee9513cdf14f57cd0b419cea8e903`.
- Runner: `scripts/workspace_transitions_pty.py`, using `tmp/subagent-comparison/venv/bin/python`.
- Both clients run in new disposable homes and workspaces. The retained evidence is written under `tmp/terminal-evidence`; neither journey opens a real repository or existing conversation.
- PNGs render the actual PTY cell stream through pyte/Pillow at 126×46. Raw ANSI and text-cell dumps are retained beside them. These are terminal-emulator captures, not `ratatui::TestBackend` projections or macOS Terminal.app screenshots.
- dshx uses a localhost OpenRouter-compatible SSE fixture, an explicit full-access fixture configuration, and a dummy key. Claude uses a dummy key with a localhost API endpoint, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1`, process-local `remoteControlAtStartup:false`, isolated `HOME`/`CLAUDE_CONFIG_DIR`, strict MCP configuration and no Chrome integration. Claude receives only local slash commands.

## dshx result

**Passed**: result (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/result.json`), provider requests (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/requests.json`), session journal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/session.jsonl`), final workspace sidecar (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/workspace-final.json`), raw terminal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/terminal.ansi`).

The single session `5529096b-717b-44ec-bc22-3c46732a3904` made eight localhost provider requests across six user turns, with two actual `read` tool calls. The recorded journal contains one session creation, one native runtime link, and one activation after plugin reload. No commercial model request was made.

| Boundary | Observed evidence |
| --- | --- |
| Command discovery and missing argument | `/add-dir` appears in the command menu. Submitting it without a path reports a required-path error. Menu (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/00a-add-directory-command-menu.png`), error (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/00b-add-directory-no-argument.png`). |
| File authority before grant | The actual `read` tool rejects the sibling directory before `/add-dir`; the fixture receives the tool error. Read refusal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/01-read-before-grant-refused.png`). |
| `/cd` refusal before grant | `/cd <outside>` reports that the directory is outside authorized roots and directs the user to `/add-dir`. No sidecar was created. Refusal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/02-cd-before-grant-refused.png`). |
| Missing directory | `/add-dir <missing>` reports an unavailable directory without creating the sidecar. Refusal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/03-add-missing-refused.png`). |
| Grant and authorized read | `/add-dir <outside>` commits revision 1 while cwd remains the original workspace; the next actual `read` returns `AUTHORIZED_OUTSIDE_FILE`. Grant (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/04-add-directory-granted.png`), read result (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/05-authorized-read-result.png`). |
| Current cwd with spaces | `/cd "nested dir"` commits revision 2. The header and command receipt show the nested cwd. Cwd (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/06-current-cwd-nested.png`). |
| Current instruction source | `/memory show project:agents` reads the updated nested `AGENTS.md`. The next recorded provider request includes `NESTED_GUIDANCE_AFTER`, excludes the prior nested marker and excludes the original root guidance. Visible source (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/06b-current-instruction-source.png`), completed request (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/07-guidance-refreshed.png`). |
| Same-session reload after `/cd` | `/reload-plugins` leaves and re-enters the alternate screen. The exact same journal remains, its previous bytes remain a prefix, cwd remains nested at revision 2, and the subsequent request still includes updated nested guidance. Reloaded session (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/08-reloaded-same-session-cwd.png`). |
| Access does not imply project trust | `/cd <outside>` commits revision 3. The next system prompt includes neither the outside `AGENTS.md` marker nor the prior nested guidance. Outside cwd (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/09-outside-cwd-without-project-trust.png`). |

Reproduce:

```sh
tmp/subagent-comparison/venv/bin/python scripts/workspace_transitions_pty.py \
  --engine dshx --binary tmp/cli-snapshots/ec95c4bbc96501ca/dshx \
  --output tmp/terminal-evidence/workspace-transitions-pty-repeat
```

## Claude reference and remaining comparison gaps

The prompt-free reference uses installed Claude Code 2.1.268: final result (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/result.json`), zero local API requests (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/local-api-requests.json`), raw terminal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/terminal.ansi`). It accepts trust only for the disposable empty workspace and selects only the session-scoped additional-directory option. It does not select persistent directory access. The path input dialog (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/03-add-directory-open.png`), scope confirmation (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/04-add-directory-result.png`), successful session grant (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/04b-add-directory-confirmed.png`), and missing-directory refusal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-claude-20260911T093542Z-f3a9b4/05-add-missing-result.png`) are all captured and validated. No model prompt was submitted; the local API received zero requests.

```sh
tmp/subagent-comparison/venv/bin/python scripts/workspace_transitions_pty.py \
  --engine claude --output tmp/terminal-evidence/workspace-transitions-claude-repeat
```

Observed differences already visible in the captured surfaces:

- Claude's bare `/add-dir` opens a directory-path input dialog with Tab completion and Enter/Escape controls. dshx's bare command reports a missing path.
- Claude's path argument opens a confirmation with session-only, remembered-directory and cancellation choices. dshx commits its session scope directly from the human command and reports current cwd, revision and authorized roots.
- dshx's successful authority change and restored session state are verified here, but these interaction differences mean this journey cannot be labeled full Claude UI parity.

This bounded run does not exercise current-session `EnterWorktree`/`ExitWorktree` through the PTY, restrictive-sandbox dialogs, active-process refusal through the PTY, or `/tui` after changing cwd. The existing service/composition tests cover the first three behavior classes in their stated scopes; this journey specifically covers `/reload-plugins` after `/cd`. It also does not test any live commercial provider.

Only the new script, this audit, and generated evidence were changed. Product source, the immutable CLI artifact and the central parity tracker were not edited.

## Later closure check: exact Claude `/cd` availability

The prompt-free Claude `/cd` reference result (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-reference-20260911T115226Z-a28c79/result.json`) and actual command menu (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-reference-20260911T115226Z-a28c79/01-cd-command-menu.png`) confirm that installed Claude Code 2.1.268 advertises the exact command with the description “Move this session to a new working directory.” The screenshot was visually inspected. This is the real counterpart, not an inferred alias.

The fresh fixture uses isolated `HOME` and `CLAUDE_CONFIG_DIR`, safe mode off, restricted mode on, strict MCP configuration, no model tools, remote control disabled, and a dummy API key with a refusing loopback endpoint/proxy. The query stops at the menu without submitting `/cd`. No model prompt or commercial request was sent. The recorded updater CONNECT to `downloads.claude.ai:443` was refused locally.

```sh
tmp/subagent-comparison/venv/bin/python scripts/config_import_reference_pty.py \
  --command cd --without-safe-mode \
  --output tmp/terminal-evidence/workspace-cd-claude-reference-repeat
```

The supported dshx `/cd` behavior already has successful native command, permission, cwd, instruction-refresh and reload evidence above. This initial availability check resolved reference uncertainty. The subsequent actual command journey below supplies its success/refusal comparison.

The earlier bare `/add-dir` differences above are historical: the final native dialog, paired session-scope comparison and draft/cursor checks are documented in [the completed directory-dialog audit](add-directory-dialog.md). The accepted session-scoped `/add-dir` contract and the [reviewed configuration import contract](config-import-audit.md) are complete.

## Final `/cd` interaction comparison and verdict

**Passed:** Claude bounded command journey v2 (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/result.json`), with 11 actual PTY screenshots and raw ANSI (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/terminal.ansi`). The setup is the same disposable, prompt-free reference described above. Commands are submitted only after the exact local `/cd` menu entry is observed. Each later submission requires an empty composer, preventing accidental command concatenation.

| Source interaction | Actual evidence | Native comparison |
| --- | --- | --- |
| Bare command | Usage response (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/02-bare-usage.png`): `Usage: /cd <path>`. No path-entry dialog opens. | dshx uses the same required path-argument contract. |
| Cancel before submission | Cancelled path draft (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/05-cancelled-draft-cwd-unchanged.png`): Escape dismisses completion, then Ctrl-U clears the draft; a fresh `/cd .` confirms the original cwd. | No cwd mutation is requested until the user submits the command. |
| Invalid directory | Missing path refusal (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/06-missing-path-refused.png`). | dshx exposes actionable directory refusals; the retained native `/cd` refusal (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/02-cd-before-grant-refused.png`) specifically verifies the additional authorized-root boundary. These are different refusal causes and are not presented as identical error text. |
| Owned child directory | Moved receipt (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/09-moved-to-owned-directory.png`) and independent `/cd .` check (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/10-real-current-directory.png`). Both the header and current-directory response show the child path. No confirmation is shown for this already trusted child. | The retained native success (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/06-current-cwd-nested.png`) shows the actual nested cwd in the header and revision-bearing receipt. Native integration checks additionally prove real file/shell/protocol ownership and refreshed instructions. |
| Relative paths after moving | Refusal under the new cwd (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/11-relative-refusal-after-move.png`), then return via `/cd ..` (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115728Z-868813/12-returned-to-original-directory.png`). | Native relative-path and restored-session behavior is covered by the existing workspace tests and same-session reload capture (local-only evidence: `tmp/terminal-evidence/workspace-transitions-pty-20260911T093418Z-f6bf9b/08-reloaded-same-session-cwd.png`). |

Source cancellation, success/current-cwd and return screenshots were visually inspected alongside the retained native cwd/refusal captures. The reference made zero inference or commercial requests; its only recorded network attempt was the updater CONNECT refused by the loopback proxy. The journey grants no additional directory access and never visits an unrelated path. Since the owned child is already trusted, a new-project trust confirmation is not part of this observed path.

The initial v1 journey had a capture-driver error: coalesced Escape/Ctrl-U did not clear the draft, and the next current-directory check concatenated onto it. Visual inspection detected the issue. Its result (local-only evidence: `tmp/terminal-evidence/workspace-cd-claude-journey-20260911T115605Z-b91081/result.json`) is corrected to partial and the original terminal evidence remains intact. The authoritative v2 separates the keys and verifies an empty composer before every command. This was a harness correction; no product change was needed.

```sh
tmp/subagent-comparison/venv/bin/python scripts/config_import_reference_pty.py \
  --command cd --cd-journey --without-safe-mode \
  --output tmp/terminal-evidence/workspace-cd-claude-journey-repeat
```

**Closure verdict: `P2-C-cd` is ready for complete within the adopted native path-argument and authorized-root contract.** Its functional owner checks, actual native cwd/refusal/reload evidence, and the applicable source command interactions are now present. Native source formatting differs from Claude's receipts; this verdict is not a pixel-identity claim. Current-session worktree tool pilots remain a separate requirement and do not reopen this command.
