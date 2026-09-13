# Whole-session commands: isolated idle UI comparison

Date: 2026-09-11. This is a bounded comparison of current Claude Code 2.1.268 and native dshx at **100 columns × 35 rows**. It covers command discovery and the lifecycle states reachable without a model prompt. Successful Claude background/fork lifecycle parity remains open.

The dshx baseline was `tmp/cli-snapshots/4ec9c987abef1c64/dshx`, SHA256 `4ec9c987abef1c64ebf8ac24c22ed7dba771e5bc4f1314cdcae573ba25a29a2b`. Both applications ran in new disposable workspaces; dshx also had a new home and a loopback catalog fixture. Claude used the explicit session UUID `7eed917e-1508-424b-8d22-053200d02506`. No existing conversation was selected.

## Isolation and evidence

The initial fresh Claude process unexpectedly displayed automatic Remote Control activity despite safe mode and empty setting sources. That process was closed before any background/fork operation. The second process used `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC=1` and a CLI-only `remoteControlAtStartup:false` setting, and its inspected idle screen had no Remote Control state. Claude's [Remote Control documentation](https://code.claude.com/docs/en/remote-control) lists that environment setting as blocking Remote Control eligibility. No global preference values were changed.

Claude ran in safe mode with tools, MCP and Chrome integration disabled. Those restrictions were retained when the reference refused a fork. No model prompts were sent. The dshx fixture received **zero provider POSTs**; the owned Claude transcript contained **zero assistant messages**. These observations do not claim that every startup/authentication request was intercepted.

Images render the **actual current PTY cells** through the same terminal profile and font helper. They are not OS application screenshots. ANSI/text captures accompany every image. The accepted captures below were opened and visually inspected. This pass does not verify OS-specific font rendering, mouse interaction, screen-reader semantics, narrow layouts, light mode or no-color behavior.

Full source artifacts: evidence (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/evidence.json`), owned-session inventory before cleanup (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/ownership-before-cleanup.json`), cleanup receipts (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/cleanup-confirmed.json`), and Claude transcript summary (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/claude-transcript-summary.json`).

## Steps and observed states

| Step | Claude | dshx | Assessment |
| --- | --- | --- | --- |
| 1. Discover `/background` | Matching commands above the composer, with matched text emphasis and wrapped descriptions. | Baseline matches below composer/status, with a selected-row arrow and several native operation matches. | Placement/style mismatch found; root implementation follow-up is tracked below. |
| 2. Discover and invoke `/fork` | Command is discoverable. Execution explicitly refuses launch restrictions that a copied child would not inherit. | Native idle fork creates a separate detached owner and leaves the parent active. | Native behavior works; equivalent Claude success state was not safely reachable. |
| 3. Discover `/resume` | Suggestions above the composer. Resuming the exact current owned ID leaves the same session active. | Baseline suggestions below composer. Resuming the owned fork changes attachment to that child, leaving the parent detached. | Discovery comparison is valid; these different lifecycle targets do not establish equivalent-success UI parity. |
| 4. Invoke `/background` while no model message has been sent | Explicitly refuses until a message exists. | The native session detaches; its terminal client exits and both owned hosts remain detached. | Actual empty-session behavior differs. No model message was sent to bypass the reference boundary. |
| 5. Settle owned state | The new foreground process exits and its exact project state is purged through the public CLI. | Both isolated hosts stop successfully and their sockets disappear. | Cleanup confirmed; unrelated sessions were not operated on. |

### 1. Background discovery

| Claude reference | dshx before layout correction |
| --- | --- |
| Claude background command discovery (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/05-claude-background-discovery.png`) | dshx background command discovery before correction (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/06-dshx-background-discovery.png`) |

The baseline moves the composer upward and puts suggestions beneath two status rows. The reference keeps suggestions immediately above the composer. The result count and native command inventory differ; those differences must remain truthful rather than adding unrelated Claude-only operations.

### 2. Fork discovery and the restricted reference result

| Claude reference | dshx before layout correction |
| --- | --- |
| Claude fork discovery (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/07-claude-fork-discovery.png`) | dshx fork discovery (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/24-dshx-fork-discovery-clean.png`) |

| Claude restricted-fork result | dshx native idle-fork result |
| --- | --- |
| Claude refuses a fork that would lose launch restrictions (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/25-claude-idle-fork-result.png`) | dshx confirms a separate native background owner (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/27-dshx-idle-fork-settled.png`) |

Claude's result explains which launch restrictions could be lost and why it refused. Those flags were not removed for a success screenshot. dshx's receipt identifies the child and how to attach, but uses a plain transcript paragraph; this is not a same-state visual comparison to Claude's refusal.

### 3. Resume discovery and native attachment

| Claude reference | dshx before layout correction |
| --- | --- |
| Claude resume discovery (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/28-claude-resume-discovery.png`) | dshx resume discovery (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/29-dshx-resume-discovery.png`) |

The baseline also clips some long descriptions, while the reference wraps descriptions across rows. Keyboard selection is visible in both; keyboard/focus equivalence beyond the actions in this capture and assistive-technology behavior remain unverified.

The native resumed-child capture (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/38-dshx-resume-owned-child-result.png`) and saved owner inventory confirm that the attachment moved to the independent child. Claude's exact-current-ID action did not change its session and is not claimed as a reference for a cross-session handoff.

### 4. Empty-session background result

Claude refuses backgrounding before a model message exists (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/40-claude-idle-background-result.png`)

The native detach exit and detached-owner states are recorded in the inventory (local-only evidence: `tmp/terminal-evidence/session-background-idle-reference-20260911T093014Z-c6e2ac/ownership-before-cleanup.json`). Capture `41-dshx-idle-background-result.png` is **not accepted as normal-screen visual evidence**: this capture helper does not restore the previous main buffer when leaving the alternate terminal screen. The accompanying ANSI stream and confirmed process/owner state remain valid operational evidence.

### 5. Cleanup

Both dshx stop operations returned zero; both sockets and the disposable workspace were removed. After the Claude foreground process exited, public CLI purge dry-runs were inspected for the two exact disposable project paths, then only those project entries and their own transcript/history rows were removed. Follow-up dry-runs found no project state for either path. Unrelated shell snapshots and global backup files were left untouched, as the CLI reports they are not project-scoped. The cleanup plans/results are stored alongside the captures.

## Correction verification

The root task corrected suggestion placement and default Ctrl+U handling based on these captures. The narrow dshx recapture used `tmp/cli-snapshots/e203034ea7404312/dshx`, SHA256 `e203034ea74043125e4819c1c4fc5acae41f5ba1fa21261304d447ce01374d2b`, at the same **100 × 35** viewport. The accepted Claude references above were retained; this recapture did not start another Claude reference session. All three updated discovery images and the cleared-composer image were opened and inspected.

| Command | dshx after correction | Verified result |
| --- | --- | --- |
| `/background` | dshx background discovery above the composer (local-only evidence: `tmp/terminal-evidence/session-background-palette-after-20260911T094139Z-95e856/02-dshx-background-discovery.png`) | Matching suggestions appear above the composer. |
| `/fork` | dshx fork discovery above the composer (local-only evidence: `tmp/terminal-evidence/session-background-palette-after-20260911T094139Z-95e856/03-dshx-fork-discovery.png`) | Matching suggestions appear above the composer. |
| `/resume` | dshx resume discovery above the composer (local-only evidence: `tmp/terminal-evidence/session-background-palette-after-20260911T094139Z-95e856/04-dshx-resume-discovery.png`) | Matching suggestions appear above the composer. |

The original capture also found a keyboard difference: Ctrl+U cleared the Claude draft but undid one dshx textarea edit. The resulting malformed-prefix diagnostic capture is excluded from matching-command comparisons. After the correction, sending Ctrl+U while `/resume` and its suggestions were visible left the composer empty and the palette closed (local-only evidence: `tmp/terminal-evidence/session-background-palette-after-20260911T094139Z-95e856/05-dshx-ctrl-u-cleared-resume.png`). Nothing was submitted. This checks that single-line palette state; it does not verify every cursor position or multiline selection.

The recapture evidence (local-only evidence: `tmp/terminal-evidence/session-background-palette-after-20260911T094139Z-95e856/evidence.json`) records zero provider POSTs, a successful stop of the sole isolated host and no remaining owners. Its disposable workspace and socket were also confirmed removed. The baseline captures remain available as before evidence. This audit changed no shared UI implementation; the root task owns those corrections.

## Remaining gates

Placement is verified, but dshx still uses a selection arrow, blue emphasis, its own status/footer rows and a different command-column width. Long descriptions remain clipped instead of wrapped, especially in `/resume` matches. The native command inventory also differs and must remain truthful.

Keep successful Claude fork, background detach/reattach, cross-session resume, pending approval, active-turn behavior, mouse navigation, other viewport/color modes and full visual parity open. The separately completed [native lifecycle process audit](session-background.md) remains evidence for native implementation behavior, not for these reference gaps.
