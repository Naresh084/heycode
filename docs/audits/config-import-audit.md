# Reviewed configuration import: completion evidence

Date: 2026-09-11. Requirement: `P2-C-import`. The approved native import contract is implemented and its acceptance checks pass. This replaces the earlier agent-only assessment: `/agent-config import` still exists, while `/import` now owns reviewed migration across supported agent, skill, command, instruction and MCP resources.

## User-visible behavior

```text
/import codex|gemini|cursor [--dry-run] [--project] <absolute source root>
/import select item-1,item-2=new-name,keep:item-3
/import confirm <reviewed digest>
/import cancel | recover | status
```

User-scope discovery takes an explicit foreign home directory. Project discovery takes the exact authorized current workspace. Discovery is always a preview; omitting `--dry-run` does not publish anything. The inventory reports supported resources, native conflicts, duplicates, required renames, manual setup, unsupported semantics and excluded fields. Selection is explicit. Unlisted rows are omitted; native replacement is not supported. Confirmation requires the exact digest of the reviewed selection.

Publication adds one immutable resource generation. Existing compositions keep their pinned generation, and a fresh composition activates the committed resources through native owners. The receipt and `/import status` distinguish durable state from active state. Native configuration files are preserved. MCP definitions start disabled; importing does not execute them, change the selected model/provider, import credentials, or grant project trust.

## Supported adapters and deliberate dispositions

| Source | Native resources accepted | Cases requiring manual setup or excluded from publication |
| --- | --- | --- |
| Codex | Strict standalone agent TOML without an unresolved model binding; instruction-only common `SKILL.md`; supported root instructions with override precedence; exact supported user MCP transports, disabled on activation. | Provider/model binding, credentials and interpolation, trust/approval/sandbox grants, unsupported agent fields, skill assets or activation metadata, non-default instruction discovery semantics and project MCP scope binding. |
| Gemini CLI | Static TOML commands, preserving raw `{{args}}` substitution and documented default argument behavior; static root `GEMINI.md`; supported user stdio or `httpUrl` MCP definitions, disabled on activation. | Shell/file expansion, instruction includes, skill consent/activation differences, legacy SSE `url`, unresolved credentials, provider settings and project MCP scope binding. |
| Cursor | Static project `.mdc` rules with `alwaysApply = true` and no dynamic references; exact supported user stdio MCP definitions, disabled on activation. | Glob, model-selected or manual rule activation; ambiguous HTTP/SSE `url`; credential/interpolation requirements and project MCP scope binding. |

These dispositions are part of the approved bounded migration contract. The importer does not pretend that different provider, permission, dynamic-command or scope semantics are interchangeable. In particular, the existing settings writer cannot persist a project MCP definition at project scope, so that case is reported for manual setup. Extending that writer is a separate feature, not an unfinished commit phase in this importer.

Adapters were checked against the current primary references: [Codex configuration](https://learn.chatgpt.com/docs/config-file/config-reference), [Codex agents](https://learn.chatgpt.com/docs/agent-configuration/subagents), [Codex instructions](https://learn.chatgpt.com/docs/agent-configuration/agents-md), [Codex skills](https://learn.chatgpt.com/docs/build-skills), [Gemini configuration](https://geminicli.com/docs/reference/configuration/), [Gemini commands](https://geminicli.com/docs/cli/custom-commands/), [Gemini instructions](https://geminicli.com/docs/cli/gemini-md/), [Gemini skills](https://geminicli.com/docs/cli/skills/), [Cursor MCP](https://cursor.com/docs/mcp), and [Cursor rules](https://cursor.com/docs/rules).

## Publication, authority and native activation

- Fixed discovery paths and bounded capability-directory reads avoid arbitrary credential discovery and symlink traversal. Plans own frozen payloads privately; previews expose screened metadata and fixed reasons.
- Confirmation rechecks source bytes and directory inventory, the destination generation, current workspace identity and revision, trust, settings policy/revisions and relevant native declarations. Native conflicts require an explicit rename or keep decision.
- The store uses one writer lock, immutable generation files, synchronization and one atomic current-generation pointer. Cancellation before publication leaves the prior generation active. Publication wins late cancellation. An uncertain durable result requires reconciliation using the retained transaction identity, rather than an automatic fresh import.
- Same-scope native definitions take precedence. Agent and skill resolution preserve user/project scope. Instruction order is imported user, native user, imported project, native project, including dynamic instruction refresh. Project resources remain inactive in another workspace.
- The composition pins one generation across installed agents, skills, prompt sections, commands and the settings overlay. Imported MCP transport fields cannot be merged into a colliding native definition, including a higher-scope partial definition. Imported commands and instructions retain reviewed bytes after the foreign source changes.

Implementation owners: [store](../../crates/heycode-config/src/imports/store.rs), [source reads](../../crates/heycode-config/src/imports/fs.rs), [adapters](../../crates/heycode-extension-host/src/config_import/parser.rs), [review service](../../crates/heycode-extension-host/src/config_import/service.rs), [native mounts](../../crates/heycode-extension-host/src/config_import/mount.rs), and [human command integration](../../crates/heycode-cli/src/config_import_integration.rs).

## Passing checks

| Check | Result and retained output |
| --- | --- |
| Production CLI composition | 3 passed: log (local-only evidence: `tmp/import-cli-owner-tests.log`), [tests](../../crates/heycode-cli/tests/config_import_integration.rs). Real scan/select/confirm/commit, old-generation isolation, fresh native activation, exact command arguments in the provider request, instruction priority, native-file preservation, native-edit refusal, scope/trust admission and other-workspace isolation. |
| Adapter, review and mount owner | 7 passed: log (local-only evidence: `tmp/import-host-owner-tests.log`). Source and host staleness, cancellation, explicit rename/keep, source semantics, disabled MCP, whole-definition precedence and scoped native readers. |
| Durable generation store | 5 passed: log (local-only evidence: `tmp/import-store-owner-tests.log`). Publication fault boundaries, cancellation/reconciliation, stale/conflicting generations, duplicate behavior and bounded file handling. |
| Native prompt owner | 9 passed: log (local-only evidence: `tmp/import-prompt-owner-tests.log`). Includes identical initial and dynamically refreshed scope ordering. |
| Existing workspace instruction regression | Passed: log (local-only evidence: `tmp/import-workspace-guidance-regression.log`). Actual file/shell tools and prompt provenance after workspace changes. |
| Strict lint checks | Passed for CLI and config/settings-file/extension-host/prompt owners: CLI (local-only evidence: `tmp/import-cli-owner-clippy.log`), contained owners (local-only evidence: `tmp/import-owned-clippy-20260911T113412Z-f93888.log`). |

## Actual terminal acceptance

The immutable binary is `tmp/cli-snapshots/449b25cf42720f30/dshx`, SHA-256 `449b25cf42720f30409b550b7847853f2db0543426c6c9fa33717df8559cb318`. The final result (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/result.json`) passes. [The runner](../../scripts/config_import_pty.py) starts the real CLI in disposable homes/workspaces with `--fake --no-background --trust-workspace`. Its 14 screenshots render the actual PTY cell stream at 138×46; text and raw ANSI are retained. The fixture used the built-in offline provider and made zero commercial requests.

| Terminal boundary | Evidence |
| --- | --- |
| Explicit inventory, rename and keep | Inventory (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/02-read-only-inventory.png`), reviewed selection (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/03-explicit-rename-keep-selection.png`). |
| Cancelled selection cannot commit | Refusal (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/04-cancelled-selection-refused.png`); no generation is published. |
| Edited source invalidates reviewed digest | Refusal (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/05-stale-source-confirmation-refused.png`); no generation is published. |
| One generation committed; current composition stays pinned | Receipt (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/06-committed-recomposition-required.png`), old composition (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/07-old-world-remains-pinned.png`). |
| Restart activates frozen native resources | Active generation (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/08-restarted-generation-active.png`), native agent view (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/09-frozen-imported-agent-active.png`), native skill picker (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/10-imported-skill-native-picker.png`). |
| Imported MCP stays disabled | Native MCP view (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/11-imported-mcp-disabled.png`); no MCP process marker is created. |
| Subsequent command import and native dispatch | Second commit (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/12-command-import-committed.png`), actual dispatch (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/13-frozen-command-native-dispatch.png`). The durable journal contains the reviewed command bytes and exact quoted user arguments after foreign-source editing. |

The fixture retains generation 1 (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/generation-1.json`), generation 2 (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/generation-2.json`), and the three native session (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/session-0.jsonl`) journals (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/session-1.jsonl`) after restart (local-only evidence: `tmp/terminal-evidence/config-import-pty-20260911T113903Z-9b8276/session-2.jsonl`). Native configuration bytes remain unchanged; a source `auth.json` symlink is never read.

Reproduce into a new output directory:

```sh
tmp/subagent-comparison/venv/bin/python scripts/config_import_pty.py \
  --binary tmp/cli-snapshots/449b25cf42720f30/dshx \
  --output tmp/terminal-evidence/config-import-pty-repeat
```

The source subsequently replaced enum-style inventory labels with readable labels such as `Manual setup` and concrete fixed explanations. This copy-only change passed CLI compilation (local-only evidence: `tmp/import-cli-ui-copy-check.log`); it is not represented by the immutable v5 screenshots and does not change the accepted behavior.

## Claude comparison boundary and closure

Installed Claude Code 2.1.268 contains a feature-gated `/import` source contract, but the exact command is not advertised in the isolated reference session. The authoritative reference result (local-only evidence: `tmp/terminal-evidence/config-import-claude-reference-20260911T114341Z-5cfdac/result.json`) records safe mode off, restricted mode on, no model prompt submitted and zero commercial requests. The actual menu capture (local-only evidence: `tmp/terminal-evidence/config-import-claude-reference-20260911T114341Z-5cfdac/01-import-command-menu.png`) shows no matching `/import` command. A separate earlier safe-mode run had the same outcome. This is an availability observation in the recorded setup, not a claim that Claude universally lacks the feature. One updater CONNECT attempt was refused by the local proxy; it was not an inference request.

The native import contract is ready to mark complete. Exact visual equivalence to the gated Claude importer and live commercial-model behavior were not demonstrated. Neither additional model spending nor repeated broad suites are required to prove this local import transaction. Approved manual and unsupported cases remain explicit product boundaries.
