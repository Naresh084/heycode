# Phase 2 `ReportFindings` production-path validation

Date: 2026-09-11 (Australia/Melbourne)

## Disposition

`P2-T-ReportFindings` now has end-to-end controlled evidence through the real
dshx CLI, TUI, native foreground `reviewer`, model-tool registry, approval
system, filesystem revision checks, durable journal, and session reopen path.
The accepted case produced exactly one local report; a second report against a
source file changed after `read` was refused; reopening reconstructed the exact
accepted evidence from an old unannotated journal shape without another
provider request. The accepted live report preserved review level, category,
verification verdict, and post-fix outcome through the tool, durable record,
and grouped compact/expanded UI.

This is **production-path local validation**, not production-model validation.
Both dshx and the paired Claude Code reference used deterministic localhost API
fixtures. No commercial inference, real provider credential, external
publication, commit, push, deployment, or migration occurred. An unscripted
tool-capable model choosing and populating the schema remains a separate live
provider gate.

This evidence supplements
[`report-findings-audit.md`](report-findings-audit.md),
which owns the backend and focused-test contract.

## Immutable dshx runtime and reproduction

The completed journey used the real macOS CLI/TUI artifact
`tmp/cli-snapshots/d9a9c1f29cafc51e/dshx-20260911T100713Z-40e433`, SHA256
`d9a9c1f29cafc51e93dc1950938b0643a01291ce7d730ea52574ebaf92dc3c1c`.
The build log (local-only evidence: `tmp/reviewer-reference-build-20260911T100707Z-485a62.log`)
records its successful build.

The [dshx PTY harness](../../scripts/report_findings_pty.py) created fresh
disposable dshx home and workspace directories, supplied only a dummy
credential, and forced all inference traffic to its loopback OpenRouter-shaped
SSE server. Reproduce the passing run from the repository root with the
existing PTY environment:

```sh
PYTHONDONTWRITEBYTECODE=1 \
  tmp/subagent-comparison/venv/bin/python \
  scripts/report_findings_pty.py \
  --binary tmp/cli-snapshots/d9a9c1f29cafc51e/dshx-20260911T100713Z-40e433 \
  --output tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5 \
  --color
```

The machine-readable result (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/result.json`)
reports 10 localhost requests, zero external-provider use, zero requests on
reopen, one accepted annotated report, a refused stale revision, and a
successful unannotated legacy replay. Full request envelopes are retained in
`requests.json` (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/requests.json`),
the annotated parent journal in
`parent-session.jsonl` (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/parent-session.jsonl`),
the missing-optional-fields replay input in
`legacy-input-session.jsonl` (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/legacy-input-session.jsonl`),
the stale child journal in
`stale_child-session.jsonl` (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/stale_child-session.jsonl`),
and raw live/replay terminal streams in
`live.ansi` (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/live.ansi`) and
`replay.ansi` (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/replay.ansi`).
The screen emulator used the terminal helper that ignores unsupported private
keyboard-mode CSI sequences while retaining the raw ANSI unchanged.

## Observed dshx journey

| Check | Observed result |
| --- | --- |
| Native reviewer dispatch | The parent called the production `agent` tool with the built-in `reviewer` preset in the foreground. The real default approval flow gated the parent dispatch. |
| Reviewer tool policy | Both child provider catalogs were exactly `glob`, `grep`, the four LSP compatibility tools, `read`, and `report_findings`. `write` and `bash` were absent. The default approval flow independently gated `read` and `report_findings`. |
| Advertised schema | Both reviewer requests exposed `level`, `category`, `verdict`, and `outcome` with the reference spellings and capped new tool calls at 32 findings. Required dshx severity, path, line range, read revision, trigger, failure, and impact fields remained required. |
| Accepted source | The reviewer read `review-target.rs`, passed the exact 64-character revision into `report_findings`, and cited the path, line, and revision in its final text. |
| Preserved dimensions | The accepted journal contains level `high`, category `correctness`, verdict `CONFIRMED`, and outcome `skipped`. The collapsed card shows `Code review(high · 1 finding)`, grouped file, line, category, and title. Expansion adds severity, trigger, failure, impact, verdict, outcome, and abbreviated revision. |
| Durable authority | The parent journal contains exactly one versioned `review/change` / `findings_reported` event. Its session/workspace/root/cwd identity was derived from the live parent and was not model-supplied. |
| Stale source refusal | The fixture changed `stale-target.rs` after the child read it and before its report. The tool returned `a finding file changed since it was read; read it again before reporting`; no second report card or durable report appeared. |
| Old-shape reopen | The harness removed only the four new optional fields from a retained copy of the accepted journal, then resumed it with the new binary. The fallback card omitted level/category/verdict/outcome, retained all required evidence and grouping, and caused zero provider requests. |

Representative retained screens are the
real report approval (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/03-report-findings-pending.png`),
annotated collapsed result (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/04-report-completed-collapsed.png`),
annotated expanded result (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/05-report-completed-expanded.png`),
stale refusal (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/09-stale-source-refused.png`),
legacy collapsed fallback (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/10-legacy-reopened-collapsed.png`),
and legacy expanded fallback (local-only evidence: `tmp/terminal-evidence/report-findings-pty-20260911T100820Z-4434a5/11-legacy-reopened-expanded.png`).
Matching text captures are adjacent to every PNG.

## Isolated current-Claude comparison

The [Claude reference harness](../../scripts/report_findings_claude_reference_pty.py)
ran the installed `Claude Code v2.1.268` in a disposable config, session, and
git workspace. It stripped inherited Anthropic and OAuth credentials, disabled
nonessential traffic and customizations, used strict MCP isolation, forced
Messages API traffic to `127.0.0.1`, and supplied the `ReportFindings` tool
explicitly. The local fixture returned one deterministic finding. The two
observed message requests both reached the localhost server; no commercial
model was invoked.

Reproduce it with:

```sh
PYTHONDONTWRITEBYTECODE=1 \
  tmp/subagent-comparison/venv/bin/python \
  scripts/report_findings_claude_reference_pty.py \
  --output tmp/terminal-evidence/report-findings-claude-20260911T094609Z-9e9cc9
```

The Claude result (local-only evidence: `tmp/terminal-evidence/report-findings-claude-20260911T094609Z-9e9cc9/result.json`),
exact advertised schema (local-only evidence: `tmp/terminal-evidence/report-findings-claude-20260911T094609Z-9e9cc9/report-findings-schema.json`),
completed screen (local-only evidence: `tmp/terminal-evidence/report-findings-claude-20260911T094609Z-9e9cc9/02-report-completed.png`),
and raw terminal (local-only evidence: `tmp/terminal-evidence/report-findings-claude-20260911T094609Z-9e9cc9/terminal.ansi`)
are retained. The generic `API Usage Billing` header is Claude's UI for the
dummy custom-key route; it does not indicate an external request or charge in
this fixture.

### Presentation comparison

| State | Claude Code 2.1.268 | dshx controlled production path |
| --- | --- | --- |
| Completed compact report | `Code review(high · 1 finding)`, grouped by file, then line, category, and summary. | `Code review(high · 1 finding)`, grouped by file, then line, category, and title. |
| Detailed evidence | The captured terminal result stays compact; it does not show the supplied failure scenario, verdict, or an expansion hint. | Enter/mouse disclosure shows explicit local-only status, severity, trigger, failure, impact, verdict, outcome, and abbreviated read revision. |
| Permission | The read-only Claude tool completed without a permission prompt in manual mode. | dshx deliberately applies its normal approval boundary to the model-originated local report. |
| Stale source | Claude's captured schema has no file revision or authority-derived workspace source, so there is no equivalent stale-read state to exercise. | The live file revision is rechecked before durable append; stale input fails without a report. |
| Replay | Not exercised in the bounded Claude renderer reference. | Both annotated durable persistence and an old missing-optional-fields fallback were exercised; the fallback reopen made zero provider requests. |

The schemas are intentionally not exact copies. Claude currently accepts an
optional review `level` and up to 32 findings with `file`, optional `line`,
`summary`, optional `short_summary`, `failure_scenario`, optional `category`,
`verdict`, and `outcome`. New dshx tool calls now share the 32-finding cap and
preserve `level`, `category`, `verdict`, and `outcome`; its durable domain keeps
the earlier 128-finding ceiling so existing journals remain readable. dshx
continues to require `severity`, `path`, `line_start`, `line_end`, exact
`revision`, `title`, `trigger`, `failure`, and `impact`, then adds an
authority-derived report source. Those additions provide stronger stale-source
safety, causal detail, and durable local provenance.

## Remaining presentation gaps

- The `report_findings` approval body is a bounded but dense raw JSON value. A
  finding-aware summary would be easier to scan before approval.
- Its generic approval helper says `Read and edit files without asking;
  commands still need approval.` That wording is misleading for a local report
  tool. The report itself is still explicitly labelled local-only and is never
  externally published.
- The stale reviewer summary is visible in the parent transcript, but the
  fixed underlying stale-revision error text remains only in the child tool
  result/journal. A failure card or child inspection path would make the reason
  discoverable without reading evidence files.

These gaps prevent an exact pixel/content-parity claim. They do not invalidate
the demonstrated report acceptance, stale refusal, durable ordering, or
provider-free replay behavior.

## Evidence hashes

| Artifact | SHA256 |
| --- | --- |
| dshx CLI | `d9a9c1f29cafc51e93dc1950938b0643a01291ce7d730ea52574ebaf92dc3c1c` |
| dshx build log | `03485340af070b7daedf08cf083c3e01234c6d06759f19d11ad859cbe7d763d3` |
| dshx result JSON | `4a01ef3f9b6e21e190f6c988776e5cacbe29db0852a1f7c99a38626ecf570b6a` |
| dshx pending PNG | `bcfb2c791ed9fd73d6b40c482eed47f217bee7eb41d677c533736d1904455996` |
| dshx annotated collapsed PNG | `9838f96df35bdcc9e48fcde305b2e8b263d084b664101b7eaa6c40b375028724` |
| dshx annotated expanded PNG | `cca526eb4cd35274390d9e62b8b90349f485eb1a109d0e052993158c9a3adfcb` |
| dshx stale PNG | `363cda27cf82480a80a2880962cdae73534037c15b491b98c0ec548d5f6de976` |
| dshx legacy collapsed PNG | `f2ce0e6a8d10be833a6c2ad5f637f26d114b927562b07b2054a2c08899ceb9b0` |
| dshx legacy expanded PNG | `6cab8445f7660f0756d28619a34b9058392efdff3a93d8629001bc279da9037d` |
| Claude result JSON | `015063af5243ac4a70bf5e62a7632c477dde6c6a077a7f165c78fa76fae733ca` |
| Claude completed PNG | `773d80c8d6b090c504fcf73de740bdc928daf60ba308890d4230cef2e71c49dd` |
| Claude schema JSON | `5e12dfc4d70230a2a6926f1ceccf2149ac8740644b1d97fe9db94bdf74fc991b` |
