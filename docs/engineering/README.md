# heycode engineering program

Status: architecture and delivery plan, not an implementation claim. This program replaces the earlier assumption that feature presence alone made the product ready to ship.

Current tracker (2026-08-31): **254 complete · 31 active · 7 not started · 0 blocked**. PAWS05, MCP11 and MCP13 are accepted. Schema 28 makes provider protocol/output defaults and Settings-policy dependencies explicit. The 115-plugin Unix composition shares one durable session/approval generation across MCP/TUI/hooks, integrates Settings-backed OpenAI/Anthropic/AWS/Google policy, async cloud readiness, hidden metadata-only audio and code-aware native/WASI extensions. The full workspace gate is green at **3,749 unit/integration tests plus 7 doctests** (0 failed, 0 ignored), with formatting and all-target warnings-denied clippy green. Authenticated/live providers, concrete O09 handlers, managed code authority, hosted fuzz/platform, security approval and signed/fresh-machine release clauses remain active. [TASKS.md](TASKS.md) is authoritative.

## Outcome

heycode will become a terminal-first, multi-provider coding agent whose product capabilities are assembled entirely from plugins. The target is not a visual clone of another agent. The target is a faster, more reliable and more extensible system with the onboarding quality of Claude Code, the control surfaces and security posture of Codex, the provider breadth of OpenCode, and the composability of DeepSeek Harness.

The product is engineering only when a new user can launch `heycode`, trust a workspace, connect a supported account or provider, choose a live-discovered model, configure permissions and MCP, complete a real coding task, resume it, and diagnose failures without editing TOML by hand.

## Evidence behind this plan

The plan is based on four evidence classes:

1. The current heycode source, tests, configuration and live TUI.
2. The local DeepSeek Harness clone at `/Users/naresh/Work/Personal/deepseek-harness`, including Cordis, profiles, capability seams, credentials, settings, compaction, MCP, jobs, workflows and delegated Codex/Claude providers.
3. Current official documentation for Codex, Claude Code, OpenCode, OpenRouter, DeepSeek, MiniMax, Z.AI, LM Studio, Amazon Bedrock, Vertex AI and their provider-native tools.
4. Live local verification on 2026-08-24: Codex CLI 0.146.0 using ChatGPT auth, Claude Code 2.1.241 using a Claude subscription, OpenCode 1.18.20 using OpenRouter, and `openrouter/stealth/ox-alpha` completing a real request. Z.ai's 2026-08-26 release identifies that alias as GLM-5.3-Flash; current work uses `z-ai/glm-5.3-flash`.

The detailed evidence and source links live in [RESEARCH.md](RESEARCH.md). The generated [provider/model capability reference](../reference/capabilities.md) comes from exhaustive code descriptors and a drift test; it deliberately leaves per-model facts to live catalogs. The current [provider-authoring guide](../guides/provider-authoring.md) documents that vertical; DOC02 remains blocked on its broader setup/MCP/plugin guide acceptance. Completed implementation slices and their verification evidence are recorded in [IMPLEMENTATION_LOG.md](IMPLEMENTATION_LOG.md); task state remains authoritative in [TASKS.md](TASKS.md).

## Current product diagnosis

The current Rust workspace contains a sound microkernel, trust-first composition, durable request/provider/inbox/lineage state, a production-verified DeepSeek adapter with replay-safe retry, strict OpenRouter GLM dispatch, OpenAI/Anthropic protocol adapters, deterministic provider/client/MCP native-tool routing, safe durable server-tool/citation inspection separated from exact provider replay, bounded stateful pause continuation, capability-rooted filesystem/process/raw-IO services, plan mode/shared-prefix subagents, strict delegated-event boundaries, registry-backed MCP stdio tools, manifest admission plus an immutable Unix install cache, safe Codex/Claude process foundations and a polished transcript/picker shell. These are useful foundations, but they do not yet form a competitive product.

The remaining live gaps are structural:

- Launch now opens canonical workspace trust before project discovery; a blocking typed modal commits through the live service and recomposes. Descriptor-driven masked authorization commits through authoritative credential readback and exact-argument recomposition before exposing the ready composer. Windows persistent trust is a release blocker.
- Provider-owned masked OpenRouter auth/profile/catalog/routing plus strict GLM reasoning/tool-state dispatch now ship deterministically beside DeepSeek. Native-tool selection and the shared safe server-event substrate are durable; OpenRouter web-tool wire parsing and authenticated evidence remain POR05/POR07, alongside other provider sources/broad authorization methods.
- Older setup releases wrote `[profile].plugins` snapshots that froze capabilities. The current startup now versions config, recognizes that exact generated home snapshot, backs it up and restores the built-in profile; full settings/profile UI remains open.
- `/`/Ctrl+P command discovery, model/provider/runtime pickers, permission/sandbox picker, MCP/plugin panels and effective status/doctor commands now ship. The settings/session/context browsers remain open.
- MCP stdio and Streamable HTTP, OAuth PKCE, atomic tool generations, resources, prompts, bounded reconnect and CLI/TUI management ship. Elicitation, rich results, allowlists and official live-lab evidence remain open.
- DeepSeek and OpenRouter now use exact catalog refresh → resolved call → durable header/context/native-tool routes → independent verification → cancellation/retry-aware normalized dispatch. The shared native/portable/prune compaction transaction and route-scoped durable checkpoint exist; OpenAI/Anthropic adapter activation, MiniMax/Gemini and hosted-tool loops remain open.
- Provider authentication now resolves through environment and owner-only home-file providers plus effect-owned authorization flows. Existing credentials receive live preflight; the real stored OpenRouter key is currently classified unauthorized before model dispatch. OAuth, cloud and subscription methods remain open.
- Codex and Claude subscriptions remain delegated runtimes rather than raw API keys. Their pinned/credential-blind account/session/event/callback primary bridges now ship; ephemeral subagents still await O05 and OpenCode remains open.
- Native macOS sandbox evidence and truthful cross-platform capability reporting now ship; hosted Landlock proof and a Windows filesystem backend remain release blockers rather than hidden caveats.

## Non-negotiable product principles

1. Everything is a plugin, including setup, settings, credentials, provider catalogs, agent runtimes, commands, UI panels, MCP, compaction and diagnostics.
2. Every swappable capability has a Service Definition, at least one Service Provider and a current Consumer.
3. Model-visible input is durable. A request header snapshot records the resolved route, prompt, tool schemas and provider state needed to reconstruct the call.
4. Configuration stores references to secrets, never secret values. Credentials resolve per operation.
5. Subscription authentication remains owned by the official product runtime. heycode never scrapes or reuses private OAuth tokens as generic API credentials.
6. Protocol compatibility is not capability parity. Every provider/model advertises explicit capabilities and the request resolver refuses unsupported combinations before network I/O.
7. Native provider functionality is preferred when it improves correctness or cost, with an explicit portable fallback when one exists.
8. Human commands never become model messages unless the command deliberately schedules model-visible work and logs it.
9. Every registration has a disposer. Plugin unload, process exit and failed composition unwind to quiescence.
10. The default product is useful with zero hand-edited configuration.
11. Every product-visible capability has a real-composition test and a replayable UI contract.
12. Speed, quality and safety claims are measured against fixed benchmarks and reference clients.

## Target runtime

```text
CLI arguments / TUI actions / ACP / SDK
                  │
                  ▼
        profile + plugin loader
                  │
       ┌──────────┴──────────┐
       ▼                     ▼
settings / credentials   UI contributions
authorization / doctor  commands / panels
       │                     │
       └──────────┬──────────┘
                  ▼
            agent runtime registry
       ┌──────────┼───────────────┐
       ▼          ▼               ▼
 native heycode   Codex runtime   Claude/OpenCode runtime
 agent loop    delegated       delegated
       │
       ▼
 request resolver → provider/model capability snapshot
       │
       ▼
 LLM adapter → native tools/compaction/cache/state handling
       │
       ▼
 durable session events → projections → TUI / ACP / SDK
       │
       ▼
 tools → approval → fs/shell/terminal/LSP/MCP/jobs/workflows
```

The native heycode loop is the primary product path for API, cloud and local inference providers. Delegated runtimes are first-class agent backends used when the user's entitlement belongs to Codex, Claude Code or OpenCode rather than a raw inference API.

## Target first-run experience

Launching `heycode` performs these steps inside the TUI:

1. Render immediately without waiting for provider or MCP startup.
2. Ask whether the workspace is trusted and explain the effective read/write/execute permissions.
3. Detect existing heycode state and offer a safe migration; do not copy another product's secrets.
4. Choose a connection type: subscription runtime, API/router, cloud, or local.
5. Run the selected authorization flow and validate it live.
6. Fetch the provider's current model catalog and render searchable capability badges.
7. Choose model, reasoning mode and provider-specific defaults.
8. Choose permission and sandbox presets.
9. Optionally import or add MCP servers and test each connection.
10. Show a final health summary and enter the composer.

The full UX specification is in [UI.md](UI.md).

## Provider classes

| Class | Examples | heycode owns model loop? | Authentication owner |
|---|---|---:|---|
| Direct API | OpenAI API, Anthropic API, DeepSeek, MiniMax, Z.AI | Yes | heycode credential plugin |
| Router | OpenRouter | Yes | heycode credential plugin |
| Cloud | Amazon Bedrock, Vertex AI | Yes | cloud SDK credential plugin |
| Local | LM Studio, Ollama, compatible gateways | Yes | local endpoint or optional token |
| Subscription runtime | Codex subscription, Claude subscription | No | official Codex/Claude runtime |
| Delegated external agent | OpenCode, DeepSeek Harness, ACP agents | No | external runtime |

Provider design and native-capability routing are specified in [PROVIDERS.md](PROVIDERS.md).

## Delivery phases

### Phase 0 — truthful baseline and migrations

- Replace the old “base complete” claim with this program. **Implemented.**
- Add configuration schema versions and migrate frozen plugin profiles. **Implemented for the known v0 setup-generated home profile.**
- Add `heycode doctor` and fail setup on invalid credentials, retired models or unusable endpoints. **The unified plugin registry, redacted schema/human forms and foundational settings/credentials/composition checks are implemented; live provider/model/setup gating remains.**
- Pin the existing behavior with integration tests before refactoring. **The exact live composition inventory is now pinned.**

### Phase 1 — product shell

- TUI workspace trust and first-run state machine. **U01/U02/U06/U10 core path implemented; broader auth classes and Q16 fresh-machine certification remain.**
- Searchable command palette and immediate dialog commands.
- Settings, credentials, authorization and live model catalog services.
- Provider/model/permission/status/MCP pickers.

### Phase 2 — provider kernel

- Capability descriptors and explicit request resolution.
- Protocol adapters for OpenAI Responses, OpenAI Chat Completions, Anthropic Messages, Gemini GenerateContent and Bedrock Converse.
- Provider-state preservation, token counting, error classification, retry and rate-limit metadata.
- OpenRouter, current DeepSeek, MiniMax, Z.AI, LM Studio, Bedrock and Vertex implementations.

### Phase 3 — native provider features

- Logical server-tool registry with native and portable implementations.
- OpenAI and Anthropic native compaction.
- Anthropic context editing, OpenRouter server tools, Gemini grounding/code execution and provider cache controls.
- Durable citations, provider-state items and compaction checkpoints.

### Phase 4 — MCP and distributable plugins

- MCP stdio and Streamable HTTP, OAuth, resources, prompts, instructions, notifications, elicitation and diagnostics.
- Plugin manifests, scopes, enablement, marketplaces, signatures, lockfile and upgrade policy.
- Skills, commands, agents, hooks, themes, provider declarations and MCP bundles as plugin contributions.
- WASI component plugins only after the declarative/plugin-process surface is stable; no unsafe dylib ABI.

### Phase 5 — coding-agent depth

- Filesystem, subprocess, shell, persistent terminal, sandbox and LSP as independent capability seams.
- Parallel tool scheduling with deterministic log order.
- Attachments and image inputs.
- Background jobs, continuable subagents, delegated runtimes, agent teams, goals, workflows and schedules.
- Hooks and reusable execution policies.

### Phase 6 — hardening and release

- Provider conformance lab and live test matrix.
- Cross-platform sandbox and process-tree tests.
- Security review, threat model, secret-leak and prompt-injection suites.
- Performance and quality benchmarks against Codex, Claude Code and OpenCode using matched models.
- Packaging, upgrades, migration rollback, crash recovery, telemetry and support runbooks.

The complete dependency-aware backlog is in [TASKS.md](TASKS.md).

## Ship criteria

### Functional

- First-run works for every Tier 1 provider without hand-editing files.
- `/`, `/model`, `/provider`, `/connect`, `/mcp`, `/plugins`, `/permissions`, `/status`, `/doctor`, `/web`, `/init`, `/resume`, `/fork`, `/compact` and `/help` are discoverable and tested.
- Resume and replay reproduce the model-visible request state for every supported provider family.
- Native tool and compaction behavior is capability-gated and has a portable fallback or an explicit unsupported result.
- MCP and plugin lifecycle survives reload, crash and process exit without leaked children or registrations.

### Reliability

- All deterministic tests pass on macOS, Linux and Windows.
- A 1,000-turn replay/compaction stress test has no sequence gaps or request-desync failures.
- Provider retry never duplicates a committed tool call or mutation.
- Crash recovery repairs open turns and unknown tool outcomes deterministically.

### Performance

- Cached cold start to interactive composer: p50 under 300 ms, p95 under 700 ms on the release reference machine, excluding first-time migrations.
- Idle TUI CPU below 0.2% and zero redraw timer while idle.
- heycode request orchestration adds under 20 ms p95 before network dispatch.
- MCP startup is concurrent and never blocks the composer; required servers report readiness or failure within their configured budget.
- Tool schemas are deferred or code-mode collapsed when catalogs would materially harm context or latency.

### Quality

- On the internal coding-agent eval set, heycode with the same provider/model is non-inferior to the reference client's task success rate within the agreed statistical margin.
- Tool-call validity, recovery rate, edit correctness and test completion are reported separately from subjective answer quality.
- Every advertised provider has a current model-discovery test and a live smoke artifact.

### Security

- Secrets never appear in logs, session events, diagnostics, prompts, child environments or test snapshots.
- Workspace trust, approval and sandbox settings are visible before the first mutation.
- MCP/plugin provenance, requested permissions and authentication destination are reviewable before enablement.
- The threat model covers untrusted repositories, tool output prompt injection, SSRF, symlink races, process escape, malicious plugins and credential theft.

## Documentation set

- [RESEARCH.md](RESEARCH.md) — evidence, live verification and competitive analysis.
- [ARCHITECTURE.md](ARCHITECTURE.md) — microkernel, plugin contracts, services, events and session v2.
- [UI.md](UI.md) — Claude-inspired TUI, onboarding and command interaction specification.
- [PROVIDERS.md](PROVIDERS.md) — provider taxonomy, capability negotiation and per-provider requirements.
- [MCP_AND_PLUGINS.md](MCP_AND_PLUGINS.md) — MCP lifecycle, plugin packaging, scopes and security.
- [AGENT_CAPABILITIES.md](AGENT_CAPABILITIES.md) — tools, execution world, sessions, jobs, subagents, workflows and hooks.
- [QUALITY_AND_RELEASE.md](QUALITY_AND_RELEASE.md) — verification, evals, performance, security and release operations.
- [TASKS.md](TASKS.md) — implementation tracker with dependencies and acceptance criteria.

## Planning constraints

- This program does not authorize scraping subscription tokens or bypassing vendor terms.
- Provider and model names are dynamic data. Examples in these documents are not a replacement for live discovery.
- A provider reaches “supported” only after contract, mock, live and replay tests exist.
- New crates are introduced only when a current Service Definition, Provider or Consumer requires an independent dependency/lifecycle boundary.
- Each implementation batch updates `AGENTS.md`, `FEATURES.md`, `docs/STATUS.md`, `docs/GOTCHAS.md` and the tracker when their facts change.
