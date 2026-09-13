# Worktree tool cards: bounded paired execution

Date: 2026-09-11. Scope: `P2-T-EnterWorktree` and `P2-T-ExitWorktree`. Both actual CLIs completed the four bounded worktree operations below using disposable Git repositories and localhost scripted provider responses. The existing [native owner evidence](workspace-transition-audit.md) remains valid. The staged live-provider pilot has not run in this pass.

## Artifacts and isolation

| | dshx | Claude Code |
| --- | --- | --- |
| Executable | `tmp/cli-snapshots/94633f465265a786/dshx` | Installed version `2.1.268` |
| Immutable SHA-256 | `94633f465265a7869daa12dd1de35fd7b070344b51ab870bbef1fcf19c1f5530` | Version recorded in result |
| Result | Passed (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/result.json`) | Passed (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/result.json`) |
| Controlled provider requests | 8 | 8 |
| Actual worktree tool results | 4 | 4 |
| Real tool contracts | `enter_worktree {}` and `exit_worktree {}` | `EnterWorktree {name: "bounded-reference"}` and `ExitWorktree {action: "keep"}` |
| Provider-request evidence | Requests including advertised definitions (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/requests.json`) | Requests including advertised definitions (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/requests.json`) |
| Executed results | Tool results (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/tool-results.json`) | Tool results (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/tool-results.json`) |
| Commercial requests | 0 | 0 |

[The runner](../../scripts/worktree_tools_pty.py) creates a fresh home, configuration, temporary directory and Git repository for each client. Each repository contains the same committed `fixture.txt` with `WORKTREE_BASELINE`. Git ignores global/system configuration, inherited provider credentials are removed, dummy credentials point to a localhost fixture, and the proxy refuses external connections. No real user configuration or credential store is opened. Claude receives an explicit two-tool allowlist, restricted mode, manual permissions, strict MCP configuration, remote control disabled and no Chrome integration. The only non-inference Claude request recorded is an updater CONNECT refused by the loopback proxy.

The displayed model names identify the configured UI routes of these fixtures. They are not evidence that a commercial model made the decisions: every inference response came from the scripted local server. Tool dispatch, Git operations and terminal rendering are real. Five screenshots per client are rendered from actual 126×46 PTY cells; native ANSI (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/terminal.ansi`) and source ANSI (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/terminal.ansi`) are retained.

## Paired observations

| Operation | Native card | Source card | Execution evidence |
| --- | --- | --- | --- |
| Enter a new worktree | Entered (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/01-local_worktree_enter.png`) | Created worktree (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/01-local_worktree_enter.png`) | Both create exactly one new Git worktree containing the baseline file. dshx creates a detached checkout; Claude creates a named branch. |
| Enter again while active | Refused (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/02-local_worktree_nested.png`) | Refused (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/02-local_worktree_nested.png`) | Both return an actionable error and leave the same worktree in place. |
| Exit while retaining work | Exited (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/03-local_worktree_exit.png`) | Kept and returned (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/03-local_worktree_exit.png`) | Both restore their session state and keep the created worktree on disk. dshx always retains; Claude's explicit `keep` action is selected. No removal or discard operation runs. |
| Exit with no active worktree | Refused (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/04-local_worktree_exit_again.png`) | No-op refusal (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/04-local_worktree_exit_again.png`) | Both report the inactive state without creating, removing or replacing a worktree. |

The source fixture file and source index bytes remain unchanged in both clients. After every operation, `git worktree list --porcelain` still contains exactly the original repository and the one created worktree. The final native list (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T120522Z-ac2e81/worktree-list-4.txt`) and source list (local-only evidence: `tmp/terminal-evidence/worktree-tools-claude-20260911T120558Z-e455aa/worktree-list-4.txt`) prove retention after both exit calls. These owned temporary repositories are removed only by fixture teardown after evidence collection; retention is assessed before teardown.

Both actual entry/exit/refusal screenshot sets were visually inspected. Native header cwd changes on entry and return. Claude's entry card exposes the new branch and worktree path; its exit card exposes retention and the restored directory. This comparison covers the shared create/retain behavior. Claude's existing-worktree switching and destructive removal options are not adopted native features.

## Exact remaining UI defect

The successful native `enter_worktree()` and `exit_worktree()` cards currently collapse to the last two lines of the generic JSON result, showing `"read_only": false` and a closing brace. That text does not explain the outcome or where the session moved. The source cards show the destination for entry and the retention/return outcome for exit. The native failure cards already display useful actionable errors.

The bounded required repair is a structured success summary for the exact native receipt shapes:

- Entry: show that the session entered a worktree and its current cwd.
- Exit: show the restored cwd and that the worktree was retained, with the retained path available.
- Preserve the complete structured result for inspection; keep refusal messages intact. Do not invent a branch name for the detached native checkout or expose an unsupported removal action.

The shared-render owner has the paired screenshots and this repair description. No shared product renderer was changed by this evidence task. This is a concrete result-presentation gap, not a reason to restart backend or broad regression testing. A rebuilt native capture after that repair can reuse the unchanged source reference.

## Reproduction and closure boundary

```sh
tmp/subagent-comparison/venv/bin/python scripts/worktree_tools_pty.py \
  --engine dshx --binary tmp/cli-snapshots/94633f465265a786/dshx \
  --sha256 94633f465265a7869daa12dd1de35fd7b070344b51ab870bbef1fcf19c1f5530 \
  --output tmp/terminal-evidence/worktree-tools-native-repeat

tmp/subagent-comparison/venv/bin/python scripts/worktree_tools_pty.py \
  --engine claude --output tmp/terminal-evidence/worktree-tools-claude-repeat
```

Core native functional evidence and the applicable source tool availability/execution are complete. The remaining worktree gates are the two native success-card summaries and the separately staged live-provider pilot. These do not reopen the completed `/add-dir`, `/cd` or `/import` contracts, and this controlled pass does not claim remote-model validation or complete worktree UI parity.

## Native summary follow-up: v9

The root implementation added exact-status success summaries while preserving the expanded structured receipt. The native-only v2 capture (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T121332Z-8d31e4/result.json`) uses immutable `tmp/cli-snapshots/41f94417ef1ba5ff/dshx`, SHA-256 `41f94417ef1ba5ffa745d67d97a12c99164ad68ae38c1aae66e48d6d2904451d`. All four tool outcomes pass again, with eight localhost requests, one retained worktree and unchanged source index/content. The original Claude reference is reused.

The entry card (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T121332Z-8d31e4/01-local_worktree_enter.png`) now correctly says `Entered worktree`, and the exit card (local-only evidence: `tmp/terminal-evidence/worktree-tools-native-20260911T121332Z-8d31e4/03-local_worktree_exit.png`) correctly says `Returned to workspace` and `Retained worktree`. Visual inspection at 126×46 found a narrower remaining defect: both long worktree paths are hard-clipped at the right edge before their identifying leaf. The original workspace path on exit fits. The required follow-up is bounded path wrapping, or explicit elision that preserves the leaf and exposes the full path on expansion. This supersedes the earlier raw-JSON summary defect; the outcome labels are now correct.

The runner's new `--inspect-details` option is prepared for the next native-only build. It checks a complete destination/retained path or explicit elision preserving the complete leaf in the summary, opens the actual mouse target, scrolls an overlong receipt if needed, checks original fields and full path, and collapses the card. It has not yet run against a binary containing the path repair. No extra Claude or paid-provider request is needed for that verification.

Paused at the user's request after the completed v9 check. The coordinator's subsequent path-wrapping change has not received an actual native PTY capture in this task. The prepared detail-inspection run and staged live-provider pilot remain unexecuted; no fixture process, local server or follow-up schedule remains active.

## Final local presentation repair before pause

Root's immutable v10 (`777a393dd9acab29ad030c20891693fb4f14bfcc962cdaab3fd19765d6d10de6`) wraps complete destination/retained paths using the same width-aware summary projection for rendering and transcript height. The 40/126-column regression proves complete paths and intact expandable original receipts (`tmp/worktree-summary-tests-20260911T121758Z-65d4d1.log`).

The actual native run `tmp/terminal-evidence/worktree-tools-native-20260911T122217Z-4a302a/result.json` passed all four operations, eight localhost requests and thirteen retained captures. Actual mouse expansion, wheel inspection of both receipt ends, and collapse preserve original cwd, roots, status and full worktree path. Root visually reviewed the returned/retained summary and expanded final receipt. Source index/content stayed unchanged; both CLI and fixture server were closed. The prior v3 failure was the inspection harness stopping at the receipt's visible start instead of scrolling down to its status; v4 explicitly scans the remaining viewport before checking.

This finishes the bounded local presentation repair. The configured-model staged-D gate is still open and was not started during pause.
