# Feature parity — heycode vs Codex CLI vs DeepSeek Harness

Foundation matrix for the original Codex CLI + DeepSeek Harness target. "✅" = implemented and tested in an assembled foundation path; "⚠️" = partial or not product-accessible enough to claim ship readiness; "🔌" = planned seam; "—" = not planned. The broader, current competitive target and live evidence are in [docs/engineering/README.md](docs/engineering/README.md) and [docs/engineering/RESEARCH.md](docs/engineering/RESEARCH.md).

This matrix does not imply that heycode is ready to ship. Provider breadth, fresh-machine certification, provider-specific MCP bundle/live evidence, managed code authority, delegated subscription runtimes and provider-native functionality remain active work.

## Agent loop & sessions

| Capability | Codex | dsh | heycode |
|---|---|---|---|
| Streaming chat with tool calls | ✅ | ✅ | ✅ `heycode-agent` turn/step machine |
| Parallel tool execution with deterministic history | ✅ | ✅ | ✅ bounded overlap, serialized admission and exact model-order durable/UI commits |
| Deferred large tool catalogs / Code Mode | ✅ | ✅ | ✅ default bounded lexical membership filters schemas, N01 routes and prompt names together only above 64 rows; Code Mode schedules ordinary calls but A06 alone executes them |
| Per-turn loop budgets | ✅ | ✅ | ✅ restart-applied Settings for step/token/elapsed/tool limits; counters reconstruct from JSONL and exact budget stop reasons persist in `turn/end` |
| Provider/tool failure closure | ✅ | ✅ | ✅ every handled failure closes its started step/turn before UI publication; crash projection never invents success |
| Reasoning-model support (thinking deltas) | ✅ | ✅ | ✅ DeepSeek `reasoning_content` → dim stream |
| Durable append-only session log | ✅ rollouts | ✅ JSONL/SQLite | ✅ JSONL v2 truth + byte-preserving v1 migration/closed gates and a rebuildable owner-only SQLite projection |
| Durable request header/context | ✅ | ✅ | ✅ validated v2 route/system hash+text/tools/options/default/context/catalog snapshots |
| Durable provider continuation state | ✅ | ✅ | ✅ v2 lossless Responses/Chat/Anthropic items with route/protocol/kind/schema validation and protocol-aware projection |
| Protocol-aware request reconstruction | ✅ | ✅ | ✅ correlated Responses/Chat/Anthropic substitution + neutral fallback + exact-adapter verified dispatch gate |
| Durable follow-up/steer/inject inbox | ✅ | ✅ | ✅ v2 insert-once splice identities, claim/cancel/replacement accounting across resume/compaction; invisible until user-message admission |
| Resume / continue a session | ✅ | ✅ fork+resume | ✅ `-c` latest · `--resume <path>` · bounded `/resume` picker with exact-id recomposition |
| Query/list/shared-prefix fork lineage | ✅ | ✅ | ✅ effect-owned bounded `session-query`, deterministic filters/pagination, exact-persisted-byte forks, recoverable archive/leaf trash and exact/Markdown/structurally-redacted exports; store integrity stays fail-loud for the whole scan while one unprojectable log is a single unreadable row (`SessionSummary::is_readable()`) that never breaks listing or `--continue` |
| Compaction of long context | ✅ auto | ✅ pressure + `/compact` | ✅ effect-owned native/portable/prune registry; `/compact [keep]`, auto pressure and native runtime share one commit owner; exact-route checkpoints plus explicit portable/fork/cancel provider-switch policy; provider adapters remain separate |
| Session titles | ✅ | ✅ LLM-backed | ✅ `session/title` log-only kind — opt-in auto after first turn (`[ui] auto_title`) + `/title [text]` + lifecycle `/rename` |
| Token/context/tool usage accounting | ✅ | ✅ token-meter | ✅ `/context` and `/usage` render durable exact/estimated/uncounted bounds, routes, cache/edit facts, local/provider-exact/provider-aggregate tool counts, honest cost and strategy state; OpenAI/Anthropic policies ship default-off |
| Content-free committed telemetry | ✅ | ✅ telemetry family | ✅ effect-owned post-commit provider/tool/compaction/cache metrics with purpose/lineage/execution and positive aggregate counts; local-off cannot emit, OTLP is opt-in |
| Multiple concurrent agents | ✅ cloud/multi | ✅ subagents registry | ✅ one engine, N sessions; spawn/fork subagents shipped (see Subagents row) |

## Providers

| Capability | Codex | dsh | heycode |
|---|---|---|---|
| Provider-neutral adapter trait | ✅ | ✅ | ✅ exact adapters plus caller-owned async readiness, contextual options, lossless state and replay-safe retry derived from the narrower of neutral resolution and protocol-specific adapter evidence; API/local/custom/AWS/Vertex routes compose conditionally |
| Provider request/response middleware | ✅ | ✅ waterfalls | ✅ effect-owned strict request/response chains; durable-only mutation, auth/native-tool admission, body-free telemetry failures, checked `next`, cancellation and refusal settlement |
| Provider request transforms | ✅ | ✅ provider transforms | ✅ effect-owned registry/P10 layer with explicit requested/effective/effect/cost rows; OpenRouter compression/file/healing policies are provider-owned and conflict-safe |
| Native/delegated agent-runtime contract | ✅ app server | ✅ | ⚠️ effect-owned registry/session contract, strict event normalizer, native adapter, primary Codex/Claude sessions, fresh one-shot delegated subagents and composed OpenCode/DeepSeek Harness process plugins ship; their required live turns remain |
| Shared HTTP/SSE/WebSocket transport | ✅ | ✅ | ✅ plugin-owned reqwest + fragmentation-invariant SSE and optional bounded WebSocket connector/fallback/metrics; no unsupported provider wire is advertised |
| OpenAI Responses protocol | ✅ | varies | ⚠️ strict state/compaction/cache plus Settings-derived Search/code/shell/file/MCP metadata ship; remaining hosted action/approval loops and live evidence stay open |
| OpenAI Chat Completions protocol | ✅ | ✅ | ✅ reusable new-contract adapter; DeepSeek/OpenRouter expose it with strict reasoning/multi-tool/state fixtures |
| Anthropic Messages protocol | ✅ | ✅ | ⚠️ production adapter, signed thinking, compaction/cache/context/counting plus Settings-derived Search/code/advisor/tool-search/MCP policy ship; remaining upper/live evidence stays open |
| DeepSeek | ✅ | ✅ default | ✅ V4 catalog-backed verified dispatch, none/high/max thinking, provider-state replay and retry/cancellation path |
| OpenRouter (any model) | ✅ | ✅ pi-ai | ⚠️ provider-owned auth/profile/live catalog, durable routing, explicit all-disabled transforms, strict GLM reasoning/tool-state dispatch and model-invoked web-search/citation wiring ship; trustworthy authenticated per-call evidence remains POR04–POR05 |
| LM Studio / Ollama local | ✅ | ✅ | ⚠️ LM Studio explicit load/unload/readback ships; explicitly configured Ollama conditionally composes its no-credential Chat provider and joined picker catalog without starting/pulling; fresh setup discovery and installed-model chat smoke remain |
| Custom OpenAI-compatible server | ✅ | ✅ | ✅ exact Chat Completions version root, optional bearer reference, canonical `/models` or explicit model selection, effect-owned production composition and Unknown-safe capability evidence; no server lifecycle management is claimed |
| Model catalog registry/cache | ✅ | ✅ | ✅ plugin-owned TTL/single-flight/last-good registry, lifecycle/aliases/filters, schema-v2 provenance, DeepSeek/OpenRouter, fuzzy picker and visible non-escalating attributed overrides |
| Adding more providers without forks | ✅ | ✅ | ⚠️ effect-owned catalogs/inference now include AWS Converse/Mantle, lazy Vertex Gemini/Claude, exact Azure OpenAI v1 and a user-supplied Chat server; MiniMax/Z.AI bound activation and live evidence remain |
| Key handling beyond env vars | ✅ login flows | ✅ credential/authorization seams | ⚠️ env → exact-argv command → owner-only home file, masked API-key authorization/live validation and exact-route per-operation rotation/no-fallback ship; trusted command config, OAuth/subscription/cloud flows remain open |
| Cross-plane secret canary | ✅ | ✅ | ✅ deterministic environment value reaches only strict outbound auth; provider body/prompt, UI/debug, request/session, process diagnostic and support export remain clean; provider error bodies are discarded |

## Tools (the model's hands)

| Capability | Codex | dsh | heycode |
|---|---|---|---|
| Read files (numbered, capped) | ✅ | ✅ | ✅ `read` |
| Write files | ✅ apply-patch | ✅ write/edit w/ CAS | ✅ `write` (read-before-overwrite enforced) |
| Edit files (exact match / replace_all) | ✅ | ✅ | ✅ `edit` |
| Shell execution with timeouts + scrubbing | ✅ sandboxed exec | ✅ bash/pwsh family | ✅ `bash` resolves once through `shell`, executes through private process trees, preserves sandbox wrapping, returns visible bounded tails and settles cancellation |
| Replaceable subprocess/process-tree service | ✅ | ✅ process family | ✅ exact argv/env-clear/bounded capture + text/raw pre-decode interactive IO, spawn/wait/cancel/terminate/kill, and common Consumers |
| Replaceable filesystem service | ✅ | ✅ filesystem family | ✅ capability-rooted canonical grants, fresh identities, atomic mutation and symlink/TOCTOU escape refusal across every file tool |
| Retained large output | ✅ | ✅ retained output | ✅ Unix owner-scoped SHA-256 spill with independent object/generation caps, verified bounded reads and a complete wrapper/metadata/body/footer preview cap; non-Unix owner security remains unsupported |
| Language-server navigation | ✅ | ✅ LSP family | ✅ replaceable effect-owned raw-stdio registry plus `lsp_servers`, definition, references and diagnostics tools; process-tree cancellation, large-result spill and distinct untrusted LSP provenance ship |
| Glob file discovery | ✅ | ✅ rg-backed | ✅ pure-Rust matcher (skip vendored dirs) |
| Grep content search | ✅ rg | ✅ rg via subprocess | ✅ regex crate, include-filter, caps |
| Todo list tool | — | ✅ todo_write | ✅ whole-list snapshot, ≤1 in_progress rule |
| Guarded pipeline: approval → guards → run → log | ✅ approval modes | ✅ pre/post waterfalls | ✅ async approval seam + monotonic guard waterfall; the card offers allow once, allow for this session, deny, and deny with a reason the model reads |
| Permission modes | ✅ | ✅ presets | ✅ Full access, Accepted edits and Default; AI Auto is unavailable without a classifier. ↑↓/Enter/Esc picker and Shift+Tab cycling. |
| OS sandboxing (argv wrap) | ✅ | ✅ full backends | ⚠️ truthful full/read-only/workspace capability report and picker, native **Seatbelt** matrix, Linux bwrap/fallback evidence + pending hosted Landlock CI; Windows restrictive backend remains absent |
| Logical native-tool routing | ✅ hosted/client tools | ✅ provider/client/MCP | ✅ effect-owned candidates, durable exact choices, safe normalized traces, bounded stateful continuation and live per-logical `prefer-native|prefer-local|native-only|local-only` Settings policy |
| Web search / fetch | ✅ browse | ✅ provider family | ✅ replaceable provider/processor registry; explicit selection/domains; DNS-pinned DDG/Brave/fetch; bounded citeable extraction; durable/model/TUI untrusted-content boundary |
| Attachments / multimodal input | ✅ images/files | ✅ attachment family | ⚠️ images, PDF/HTML and hidden experimental PCM-WAV input/output ship with owner-only bytes and metadata-only replay; office/spreadsheets remain |
| MCP client (external tools) | ✅ | ✅ mcp-client | ✅ stdio + pull-owned duplex HTTP, OAuth pre-registration/CIMD/DCR, listings, management, rich results, Inspector conformance, exact session TUI elicitation/progress/logging and shared Agent allowlist/approval all ship; a configured `[mcp.servers.<name>] url` connects in the default product root and `heycode mcp add` takes effect in the next session, with configuration winning a name collision |
| Background jobs (`job_*`) | partial | ✅ jobs registry | ✅ effect-owned stable ids/tokens/JoinHandles, shell/PTY producers, joined process-tree cancellation, durable commit-before-visible settlement, wake budget, `/tasks|ps|stop` and Jobs side panel |
| Subagents | ✅ threads | ✅ continuable | ✅ `task` with fresh/continuable/fork plus `background:true`; native lineage is durable/authority-scoped, delegated providers inherit permission/cancellation, and schema-26 exact-base worktree providers are conditional |
| Agent teams | ✅ multi-agent | ✅ teams | ✅ durable authority-scoped roster, task DAG, peer mailbox, recovery and job/inbox-ordered dispatch through the `team` tool |
| Isolated runtime review | ✅ review | ✅ reviewer | ✅ selectable `/review-runtime`, exact tracked patch, DenyAll isolated checkout, mutation refusal and strict structured findings |
| Plan mode | ✅ plan tool | ✅ logged plan state | ✅ `plan/mode` durable state, mid-turn pending/pre-step commit, `exit_plan_mode` review gate and logged `/plan <message>`; enforcement is two-layer — the `seam/pre_tool` allow-list for this agent's own tools plus a `DelegationGate` on the subagent registry for children the seam cannot reach — and `task` is admitted in plan mode ONLY for the native provider |
| Skills (SKILL.md catalog/loader) | ✅ | ✅ skill family | ✅ capability-bound no-follow rank-ordered roots, immutable bodies, user-only enforcement, `/skill` injection and catalog |

## UI (terminal)

| Capability | Codex TUI | Claude Code (reference look) | heycode |
|---|---|---|---|
| Markdown transcript w/ syntax highlighting | ✅ | ✅ | ✅ pulldown-cmark + syntect plus durable provider/server-tool/citation/compaction/runtime/route/plan renderers shared by live/replay/full/flat views |
| Tool-call cards (`⏺ name(args)` + `⎿ result`) | variant | ✅ | ✅ exact language |
| Diff rendering for edits | ✅ | ✅ +/- colored | ✅ edit returns a unified diff; renderer colors `-` red / `+` green under ⎿ |
| Rounded input editor, multiline paste | ✅ | ✅ | ✅ tui-textarea + bracketed paste (one `Paste` event, composer grows to ten rows; enabled in `--screen-reader` too), Alt+Enter / Shift+Enter / `\`+Enter newlines, Ctrl+C clears then arms then quits, prompt history persisted per home |
| Status line (model · cwd · tokens · spinner verbs) | ✅ | ✅ | ✅ |
| Interrupt generation (Esc) | ✅ | ✅ | ✅ cancels AND finalizes a partial anchor |
| Active-turn steering/follow-up | ✅ | ✅ | ✅ native Enter→durable next-step Steer, Tab→durable next-turn FollowUp, Esc→interrupt; queue state/resume are visible and delegated fallback is refused |
| Scrollback with follow-mode | ✅ | ✅ | ✅ PgUp/PgDn offset from bottom plus bounded item-fragment virtualization; 100K-event bottom/middle budgets are pinned |
| Plugin-contributed UI slots | ✅ | ✅ | ✅ typed effect-owned panel/dialog/status/side-panel registry; transcript/approval/session and Diff/Jobs/Agents surfaces attributed |
| Workspace trust modal/recompose | ✅ | ✅ | ⚠️ blocking live-bound modal and Unix persistent trust ship; Windows file persistence remains unsupported |
| Slash command plane (never model messages) | ✅ | ✅ | ✅ searchable scheduling plus `/init`, status/permission/web/routing/settings/session/capability panels, human-only `/diff /copy /mention /tasks /ps /stop`, explicit model-scheduling `/review`, logged `/plan|goal` domain messages and CAS-backed `/theme /keymap /vim` |
| Idle = zero CPU wakeups; busy redraw cap | ✅ | ✅ | ✅ event-driven select loop, 120 ms spinner tick while busy only |
| Screen-reader/no-alternate-screen mode | ✅ | ✅ | ✅ explicit `--screen-reader` plus automatic `TERM=dumb`; bounded control-sanitized flat frames reuse production state/key routing and survive recomposition |
| Deterministic interaction journeys | ✅ snapshots | ✅ snapshots | ✅ reusable production-reducer snapshots for trust, setup, commands, MCP and provider/runtime selection |

## Automation surfaces

| Capability | Codex | dsh | heycode |
|---|---|---|---|
| Headless one-shot runs | ✅ exec mode | ✅ headless profile | ✅ `heycode run "…"`: reply on stdout, tool activity and `session <id>` trailer on stderr, `--output-format text\|json\|stream-json`, piped stdin appended, exit 0/1/130 |
| Effective configuration with sources | ✅ | ✅ | ✅ `heycode config show` / `/config`: home → trusted project overlay → command-line pins, each value tagged with its file or flag; unknown keys warn, type errors name file/key/line |
| Live MCP server health | ✅ `mcp list` connects | ✅ | ✅ `heycode mcp test\|list` and `/mcp`'s "Refresh and probe" spawn/initialize each definition concurrently in bounded time (`reachable`, `unreachable`, `needs-auth`); plain listing never connects |
| Named profile selection | ✅ | ✅ | ✅ strict effect-owned `$HEYCODE_HOME/profiles/<name>.toml` store shared by `--profile` and queued TUI `/profile`, with exact-argument recomposition |
| Composition doctor | ✅ | ✅ | ✅ human/JSON zero-apply dependency/collision graph plus body-free isolated production activation with explicit suppressions |
| Unified health doctor | ✅ | ✅ | ✅ plugin-contributed async migration/settings/credentials/composition checks; raw config bytes structurally absent from redacted human/schema-v1 JSON |
| Bounded retained health history | ✅ | ✅ | ✅ effect-owned `/health` store survives restart with count/byte caps, screened labels and visible damage state |
| Durable goals and bounded rounds | ✅ | ✅ | ✅ CAS snapshots, disarmed resume, round/wake budgets, authoritative idle edge, model tool and `/goal` |
| Checkpointed workflows | ✅ | ✅ | ✅ replaceable worker Provider, progress/checkpoints, cancellation and exact-prefix resume |
| Durable schedules | ✅ automations | ✅ | ✅ session-local delay/absolute/fixed-rate timers, enqueue-before-dispatch recovery, restart rearm and explicit fork copy |
| Machine protocol server (IDE/desktop shells) | ✅ app-server | ✅ ACP + SDK JSON-RPC | ✅ ACP v1 plus stable heycode-local JSON-RPC v1, runtime/workspace/route controls, bounded stdio host, fixture-locked Rust/TypeScript SDKs and an installed VSIX shipping-binary journey |
| Offline fake-provider test harness | ✅ testkit | ✅ llm-replay/mock-server | ✅ `FakeProvider` + `--fake` CLI smoke |
| Shared real-composition harness | ✅ | ✅ | ✅ production factory/loader + isolated roots + fake inference + owned shutdown |
| Shared provider conformance runner | ✅ | ✅ | ✅ raw-SSE fragmentation/failure matrix reused by Chat, Responses and Anthropic adapters |
| Persisted request replay oracle | ✅ | ✅ | ✅ shared Chat/Responses/Messages/Gemini/Bedrock fixture commits C02, reopens JSONL and returns only through production C05; physical corruption control prevents self-comparison |
| Black-box performance budgets | ✅ | ✅ | ✅ startup/TTFT/1K replay/flat-render/1K-tool metrics, enforced debug regression ceilings and content-free baselines; release targets remain proposed pending reference hardware |
| Matched coding-agent evals | ✅ | ✅ | ✅ identical declared conditions, deterministic graders, Wilson intervals and paired bootstrap; small samples report insufficient evidence rather than a winner |
| Prompt-injection product eval | ✅ | ✅ | ✅ real Web/MCP/LSP hostile sources reach genuine writes inside exact warnings; deny settles without mutation and an independent auto control proves the action is non-vacuous |
| Parser/render fuzzing | ✅ | ✅ | ⚠️ five bounded libFuzzer targets and synthetic corpora ship; local fixed-seed smoke is green including ANSI/OSC cell safety, while hosted continuous runs remain Q12 evidence |
| Deterministic chaos | ✅ | ✅ | ✅ composition rollback, session settlement/repair, process-tree cancellation, SSE fragmentation and provider-failure redaction pass under a content-free seeded runner |
| External plugin manifest/install/activation | ✅ | ✅ | ⚠️ strict cache/managed admission plus concrete declarative/native/WASI six-domain activation ship; managed code grants, publisher verification, remote fetch and non-Unix owner security remain |
| Signed release/update boundary | ✅ | ✅ | ⚠️ attested owned artifacts, recoverable update/rollback, channel/plugin-API policy, GitHub verifier and release CLI ship; an observed signed heycode transaction and native real-provider matrix remain |

## Interactive setup

| Capability | Codex | heycode |
|---|---|---|
| First-run guided provider/key/model flow | ✅ sign-in | ✅ trust-first descriptor-driven masked authorization, authoritative commit/readback and exact-argument recomposition reach the provider-owned default model/effective permission composer; fresh-machine live matrix remains Q16 |
| Effective welcome/status card | ✅ | ✅ runtime and provider/model are distinct; live route/approval/workspace plus plugin-doctor health ship; rate-limit fields remain open |
| Provider/runtime picker | ✅ | ✅ live inference/native/delegated rows, fuzzy class filters/current markers; provider/model selection persists via Settings CAS, delegated activation remains |
| Permission/sandbox picker | ✅ | ✅ live effective/backend report, unsupported rows visible but unselectable, network truth explicit; mode changes require restart until hot Settings owner |
| Connect/logout/effort commands | ✅ | ⚠️ | ⚠️ connect reuses contributed masked flows and logout authoritative delete; effort is visibly capability-gated until active adapter levels land |
| Keys persisted safely, env still wins | varies | ✅ environment precedence, owner-only home file persistence without OS keychain access, migration, rotation-aware validation and preflight |

---

**Active program:** [docs/engineering/TASKS.md](docs/engineering/TASKS.md). Unsafe dylib loading remains rejected; the external extension plan uses declarative/process plugins and later the WASI Component Model.
