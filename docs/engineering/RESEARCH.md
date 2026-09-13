# Research evidence and competitive baseline

Research date: 2026-08-24. Product behavior and provider catalogs change quickly; implementation must re-query primary sources and live endpoints rather than treating this document as a model catalog.

## Method

The investigation combined:

- Current official product and provider documentation.
- The local DeepSeek Harness source and its architecture guides.
- Read-only CLI inspection of locally configured Codex, Claude Code and OpenCode installations.
- Interactive PTY inspection of all four terminal UIs.
- Minimal no-tool live requests to verify subscription and API paths.
- A live OpenRouter model lookup for `stealth/ox-alpha`.

No credential value was printed, copied between products or added to the repository.

## Live verification

| Product/path | Observed state | Live result |
|---|---|---|
| Codex CLI `0.146.0` | Authenticated using ChatGPT | Returned `CODEX_LIVE_OK` through `codex exec --ephemeral --sandbox read-only` |
| Claude Code `2.1.241` | Authenticated through a Claude subscription | Returned `CLAUDE_LIVE_OK` through print mode with no persisted session |
| OpenCode `1.18.20` | OpenRouter credential configured | `openrouter/stealth/ox-alpha` returned `OPENCODE_OX_LIVE_OK` |
| heycode current binary | OpenRouter credential file exists and provider request reached OpenRouter | Failed with HTTP 401 `User not found`; presence-only credential checks are insufficient |
| OpenRouter model catalog | `stealth/ox-alpha` resolves as Ox Alpha | 1,048,576 context; tool calling, tool choice, reasoning and response-format parameters; zero listed prompt/completion price at verification time |

2026-08-25 implementation addendum: the integrated `runtime-codex` safe gate passed against installed Codex CLI 0.146.0 using temporary `CODEX_HOME`, sanitized/bound launcher+Node interpreter and only version + initialize/initialized. The integrated `runtime-claude` gate passed against installed Claude Code 2.1.243 using credential-blind official status and one strict tool/MCP-free no-persistence canary. Neither test read credential files or token values.

2026-08-27 OpenRouter/Z.ai addendum: the requested spelling `glm-4.3-flash` does not exist in the current official OpenRouter catalog. OpenRouter publishes `z-ai/glm-5.3-flash`, released 2026-08-26, with text/image/video input, reasoning, tools, structured output, a 1,310,720 model context field and a current top-provider limit of 1,048,576 context / 131,072 completion tokens. Z.ai's release and developer documentation confirm that `ox-alpha` was the model's anonymous OpenCode/OpenRouter pre-release identity, that the direct model code is `glm-5.3-flash`, and that it supports 1M context, mandatory enabled thinking, function calling, caching, structured output and native multimodal input. Product work therefore migrates the reference lane to exact OpenRouter slug `z-ai/glm-5.3-flash` rather than encoding the incorrect user-supplied version.

POR02 live addendum: OpenRouter's complete unauthenticated `/api/v1/models` response contained 417 rows at verification. Exact lookup is singular `/api/v1/model/{author}/{slug}`; the plural single-row path returns 404. The complete generation includes multiline descriptions, a few display names with trailing whitespace and three router rows with an empty `supported_parameters` array. POR02 normalizes those display-only shapes while keeping ids/slugs/limits strict, validates the full generation before requesting the GLM detail row and cross-checks both representations before publication.

Primary sources: [OpenRouter model page](https://openrouter.ai/z-ai/glm-5.3-flash), [OpenRouter models API](https://openrouter.ai/api/v1/models), [Z.ai release](https://z.ai/blog/glm-5.3-flash), [Z.ai developer overview](https://docs.z.ai/guides/vlm/glm-5.3-flash).

2026-08-28 image-input addendum: OpenAI Responses defines a user content part
`type=input_image` whose `image_url` may be a fully qualified URL or a base64
data URL and whose `detail` defaults to `auto`. Chat Completions defines user
content arrays with `type=image_url` and nested `{url,detail}` using the same
URL/base64 choice. Anthropic Messages defines user `image` blocks with a
base64 source, explicitly supports JPEG/PNG/GIF/WebP, and recommends image
blocks before text. ATT02 implements those exact embedded-byte shapes and
refuses any model without explicit image capability evidence. Primary sources:
[OpenAI Responses input image](https://platform.openai.com/docs/api-reference/responses-streaming/response/content_part),
[OpenAI Chat Completions image content](https://platform.openai.com/docs/api-reference/chat/create),
[Anthropic Messages vision](https://platform.claude.com/docs/en/build-with-claude/working-with-messages),
[Anthropic Messages API](https://platform.claude.com/docs/en/api/http/messages).

2026-08-28 document-input addendum: OpenAI's current file-input guide documents
native file parts for both Responses and Chat. Responses uses
`{type:"input_file",filename,file_data:"data:application/pdf;base64,..."}`;
Chat uses `{type:"file",file:{filename,file_data}}`. OpenAI notes that PDF
processing supplies both extracted text and page images on vision-capable
models, whereas non-PDF documents are text-only. Anthropic Messages uses a
`document` block with a base64 `application/pdf` source; current limits are a
32-MB request and up to 600 pages (100 below a 1M context window), and
password/encrypted PDFs are unsupported. ATT03 therefore treats native file
support as model evidence, not a protocol assumption, and otherwise reuses the
bounded local extractor. Primary sources: [OpenAI file inputs](https://developers.openai.com/api/docs/guides/file-inputs),
[OpenAI Responses input-file reference](https://platform.openai.com/docs/api-reference/responses-streaming/response/file_search_call),
[Anthropic PDF support](https://platform.claude.com/docs/en/build-with-claude/pdf-support),
[Anthropic Messages API](https://platform.claude.com/docs/en/api/http/messages).

2026-08-27 routing addendum: OpenRouter's documented Chat `provider` object exposes `order`, `allow_fallbacks` (default true), `require_parameters` (default false), `data_collection` (`allow` default or `deny`) and optional per-request `zdr`, plus later performance/price/quantization filters. Setting `order` or `sort` disables default price/uptime load balancing. `require_parameters=true` prevents routing to endpoints that would ignore request parameters; `data_collection=deny` and `zdr=true` are distinct filters. POR03 implements the five tracker-required fields as typed durable policy rather than an opaque serialization-only JSON object. Primary source: [OpenRouter provider routing](https://openrouter.ai/docs/guides/routing/provider-selection).

2026-08-27 reasoning addendum: OpenRouter's unified request is `reasoning: {effort}`. Catalog reasoning metadata defines supported/default/mandatory state; mandatory models must not offer/send `none`. Assistant continuation accepts raw `reasoning`, the `reasoning_content` alias or complete ordered `reasoning_details`; details are specifically required for encrypted/summarized/signature-bearing models and must be replayed unchanged around tool results. POR04 implements all three continuation forms. Primary source: [OpenRouter reasoning tokens](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens).

The current shell contains no raw OpenRouter, Anthropic, DeepSeek, MiniMax, Z.AI, AWS or Vertex credentials. Codex, Claude Code and OpenCode correctly rely on their own credential stores or subscription sessions. heycode must model this distinction instead of assuming every provider is an environment variable.

2026-08-28 ACP addendum: the current upstream v1 schema defines prompt as an
array of MCP-compatible content blocks, with image and embedded-resource input
behind advertised capabilities. `session/update` includes user/agent/thought
chunks, tool call/update, full plan and usage updates. `session/cancel` is a
session notification and the original prompt must return `stopReason=cancelled`;
protocol-level `$/cancel_request` is separate. X03 implements those shapes over
the native RuntimeSession and retains legacy string-prompt compatibility only
for existing heycode clients. Primary sources: [ACP v1 schema](https://github.com/agentclientprotocol/agent-client-protocol/blob/main/schema/v1/schema.json),
[ACP v1 TypeScript SDK](https://github.com/agentclientprotocol/typescript-sdk/blob/main/src/acp.ts),
[ACP protocol overview](https://agentclientprotocol.com/protocol/overview),
[ACP cancellation](https://agentclientprotocol.com/protocol/cancellation).

## Live TUI comparison

### heycode

The observations below are the pre-program baseline captured on 2026-08-24. They are retained as competitive evidence, not current-state claims; current implementation status is in `docs/STATUS.md`.

- Opens directly into an empty composer with the configured model in the status line.
- Has no workspace trust screen, welcome state, provider health state or setup summary.
- Typing `/` inserts a character but opens no command menu.
- `/help` renders a text list, not a searchable palette.
- The configured `$HEYCODE_HOME/config.toml` contains an explicit eight-plugin profile written by an earlier setup flow. That profile omits the currently shipped skills, MCP, subagent, plan and sandbox plugins, so the live TUI cannot expose them.
- `/provider` and `/model` report or accept raw strings; they do not discover or validate choices.

### Claude Code

- A new directory opens a dedicated workspace-trust screen before the agent can read, edit or execute there.
- The welcome card shows product version, selected model/context, account class, workspace and task-oriented tips.
- The footer shows mode, effort and agent navigation.
- Typing `/` immediately opens a fuzzy command menu with descriptions. Typing more characters filters it.
- The menu includes setup and lifecycle commands such as `/init`, `/memory`, `/mcp`, `/permissions`, `/model`, `/plan`, `/compact`, `/context`, `/tasks`, `/resume`, `/branch`, `/doctor` and `/plugin`.
- Safe mode visibly disables project customizations and explains how to restore them.

Official references: [getting started](https://code.claude.com/docs/en/getting-started), [commands](https://code.claude.com/docs/en/commands), [authentication](https://code.claude.com/docs/en/authentication), [extensions](https://code.claude.com/docs/en/features-overview), [plugins](https://code.claude.com/docs/en/plugins).

### Codex

- The startup card shows model/reasoning, directory and permission mode before the first prompt.
- MCP startup progress is visible without blocking the composer.
- Typing `/` opens a dynamic command menu with descriptions; `/m` filters to model, memories, mention and MCP.
- Commands cover model/reasoning, permissions, keymaps, Vim mode, experiments, skills, plugins, MCP, apps, sessions, compaction, review, diagnostics and account usage.
- Codex supports ChatGPT browser/device-code login, API keys, enterprise access tokens and provider-specific authentication through the app-server account API.
- The app server exposes model discovery, provider capability discovery, MCP status/OAuth, config writes, thread compaction and streamed thread/item events.

Official OpenAI documentation: [authentication](https://learn.chatgpt.com/docs/auth), [CLI commands](https://learn.chatgpt.com/docs/developer-commands?surface=cli), [MCP](https://learn.chatgpt.com/docs/extend/mcp), [plugins](https://developers.openai.com/plugins/concepts/plugins), [app server](https://learn.chatgpt.com/docs/app-server), [web search](https://learn.chatgpt.com/docs/web-search).

### OpenCode

- The composer continuously shows agent, model, provider and reasoning variant.
- The footer exposes agent switching, command palette, project/branch and version.
- Ctrl+P opens a searchable command palette; session and model pickers are first-class dialogs.
- `/connect` stores provider credentials and `/models` selects from a large live catalog. The CLI can refresh its model cache from models.dev.
- The provider directory includes API, cloud, local and coding-plan providers.
- The V2 plugin API can transform provider/model catalogs, agents, commands, integrations, references, skills and tools, and can intercept request/response and tool execution.

Official references: [providers](https://opencode.ai/docs/providers/), [CLI](https://opencode.ai/docs/cli/), [MCP](https://opencode.ai/docs/mcp-servers/), [agents](https://opencode.ai/docs/agents/), [skills](https://opencode.ai/docs/skills/), [V2 plugins](https://opencode.ai/v2/docs/build/plugins).

## DeepSeek Harness architecture findings

The local clone is the architectural source of truth for “everything is a plugin.” Its design is materially broader than the current heycode port.

Key patterns to retain:

- An empty microkernel context receives services, typed events and reversible effects.
- Profiles are ordered plugin trees assembled from bundles and user overlays; any row can be replaced.
- A capability seam is complete only when it has a Service Definition, Service Provider and Consumer.
- Services use stable keys, while implementations register behind them without changing consumers.
- Durable session events and live agent events are separate planes.
- The agent loop itself is replaceable. Extensions attach to request, pre-step, tool and turn events instead of patching the loop.
- Request headers log the provider/model route, rendered system prompt and tool schemas so model-visible state is reconstructable.
- Settings and credentials are separate services. Settings carry credential references; credentials resolve per operation.
- Provider profiles, model catalogs and credentials can update without rebuilding the loop.
- Filesystem, subprocess, shell, terminal, sandbox and LSP are independent but coordinate-compatible services.
- Jobs, goals, schedules, workflows, plan mode, subagents and agent teams are independent capabilities.
- MCP is a lifecycle-managed tool provider, not a special case in the agent loop.
- Codex and Claude Code integrations are delegated subagent providers using official runtimes, not stolen subscription tokens.
- Process-local self-modification is possible through model-defined Cordis plugins, but it is explicitly treated as bash-level trust.

Local references: `/Users/naresh/Work/Personal/deepseek-harness/docs/architecture.md`, `docs/cordis-primer.md`, `.agents/guides/core-spine.md`, `.agents/guides/execution-world.md`, `.agents/guides/operations.md`, `.agents/guides/orchestration.md`, `.agents/guides/surfaces-and-clients.md`.

Official public reference: [DeepSeek Harness architecture](https://github.com/deepseek-ai/deepseek-harness/blob/master/docs/architecture.md).

## Product capability comparison

| Capability | Claude Code | Codex | OpenCode | Current heycode | Target heycode |
|---|---:|---:|---:|---:|---:|
| Integrated first-run TUI | Yes | Yes | Provider connect flow | No | Yes |
| Workspace trust | Yes | Permission-centric | Permission-centric | No | Yes |
| Searchable commands | `/` | `/` | Ctrl+P | Text-only `/help` | `/` plus command palette shortcut |
| Interactive model picker | Yes | Yes | Yes | No | Live provider/model picker |
| Live model catalog | Product-managed | `model/list` | models.dev/provider | No | Provider-owned discovery |
| Subscription auth | Claude account | ChatGPT account | Provider-specific plugins | No | Delegated official runtimes |
| Direct API providers | Anthropic/cloud | OpenAI/custom/Bedrock/local | Broad catalog | Two | Broad capability-based catalog |
| MCP stdio | Yes | Yes | Yes | Yes | Yes |
| MCP HTTP/OAuth | Yes | Yes | Yes | No | Yes |
| MCP resources/prompts/instructions | Partial/product-specific | Yes | Evolving | No | Yes where a Consumer exists |
| Plugin marketplace | Yes | Yes | npm/local/V2 | No | Signed/pinned marketplaces |
| Hooks | Extensive | Extensive | Plugin hooks | No | Typed lifecycle hooks |
| Persistent terminal/jobs | Yes | Yes | Yes | No | Yes |
| LSP | Plugin/built-in integrations | IDE surface | Yes | No | Yes |
| Native provider tools | Anthropic server tools | OpenAI Responses tools | Provider-dependent | Portable web only | Capability-routed native + portable |
| Native compaction | Claude context compaction | Responses compact / thread compact | Client-specific | Generic summarizer | Provider-native strategy registry |
| Parallel agents/worktrees | Yes | Yes | Yes | Basic subagents | Jobs, runtimes, worktrees, teams |
| Settings UI | Yes | App/config dialogs | Config/TUI | No | Plugin-contributed settings panels |
| Doctor/diagnostics | Yes | Yes | Debug commands | No | Unified health report |

## Provider research findings

### OpenRouter

- Live models API supports capability filtering and sorting by price, context, throughput, latency and recency.
- Provider routing supports ordered providers, fallbacks, parameter requirements, data-collection policy and ZDR preferences.
- `openrouter:web_search` is a model-invoked server tool that can use native provider search or a fallback engine.
- Its current request uses `tools[].type = openrouter:web_search` with a nested `parameters` object and optional top-level `max_tool_calls`; the older web plugin and `:online` form are deprecated.
- Chat output standardizes public sources as nested `url_citation` annotations and reports aggregate `usage.server_tool_use.web_search_requests`. Current official Chat docs/schema do not expose a per-search id or query, so implementation must not infer one.
- Current server-tool search accepts `allowed_domains` and `excluded_domains`, but exact compatibility varies by engine/native provider; OpenAI native ignores exclusion while Google native does not support filters. Server-tool fetch accepts independent allowed and blocked domains. A portable policy must enforce its own model-visible/fetch boundary and provider-native adapters must consume or refuse filters rather than assume one universal wire behavior.
- OpenRouter plugins include response healing, PDF parsing and context transforms; they are request transforms, not interchangeable with model-invoked tools.

References: [models](https://openrouter.ai/docs/guides/overview/models), [provider routing](https://openrouter.ai/docs/guides/routing/provider-selection), [web-search server tool](https://openrouter.ai/docs/guides/features/server-tools/web-search), [web-fetch server tool](https://openrouter.ai/docs/guides/features/server-tools/web-fetch), [tool calling](https://openrouter.ai/docs/guides/features/tool-calling), [OpenAI web-search filter schema](https://platform.openai.com/docs/api-reference/responses-streaming/response/file_search_call).

### OpenAI API and Codex

- `POST /responses/compact` returns opaque compaction items for continuation and usage accounting.
- Responses exposes hosted web search, file search, code interpreter, shell, computer use, image generation, remote MCP and deferred tool search depending on model and account.
- Codex app server exposes `thread/compact/start`; this is a delegated-runtime operation, separate from OpenAI API compaction.
- ChatGPT subscription access is supported by Codex itself, not as a generic raw inference credential for third-party loops.

References: [compact a response](https://developers.openai.com/api/reference/resources/responses/methods/compact), [Responses API](https://developers.openai.com/api/reference/resources/responses), [Codex auth](https://learn.chatgpt.com/docs/auth), [Codex app server](https://learn.chatgpt.com/docs/app-server).

### Anthropic API and Claude Code

- Claude Code supports Claude subscriptions, API billing, Bedrock, Vertex and other managed deployment paths.
- Anthropic server tools include web search, web fetch, code execution, advisor, tool search and an MCP connector.
- Server-side context compaction uses `compact_20260112` and returns a compaction block that must be preserved in later messages.
- Context editing can clear older tool results and thinking blocks; interleaved tool use requires preserving relevant thinking blocks.
- Parallel client calls require every `tool_result` together in one immediately following User content array; separate User messages are explicitly wrong. Result blocks precede any text.
- Total prompt usage is the checked sum of `input_tokens`, `cache_creation_input_tokens` and `cache_read_input_tokens`; `output_tokens` is inclusive. Streaming usage components are cumulative.
- Streaming is message-start → sequential block start/delta/stop → one-or-more message deltas → message-stop. Thinking signature closes thinking, compaction emits exactly one complete delta, and `pause_turn` requires exact response replay.
- Claude subscription access remains owned by Claude Code/Agent SDK.

References: [tool reference](https://platform.claude.com/docs/en/agents-and-tools/tool-use/tool-reference), [parallel tool use](https://platform.claude.com/docs/en/agents-and-tools/tool-use/parallel-tool-use), [server tools](https://platform.claude.com/docs/en/agents-and-tools/tool-use/server-tools), [streaming](https://platform.claude.com/docs/en/build-with-claude/streaming), [Messages usage](https://platform.claude.com/docs/en/api/messages/create), [compaction](https://platform.claude.com/docs/en/build-with-claude/compaction), [extended thinking](https://platform.claude.com/docs/en/about-claude/models/extended-thinking-models), [effort](https://platform.claude.com/docs/en/build-with-claude/effort), [stop reasons](https://platform.claude.com/docs/en/build-with-claude/handling-stop-reasons), [context editing](https://platform.claude.com/docs/en/build-with-claude/context-editing), [Claude Code auth](https://code.claude.com/docs/en/authentication).

### DeepSeek

- Current DeepSeek V4 models use `deepseek-v4-pro` and `deepseek-v4-flash`; legacy `deepseek-chat` and `deepseek-reasoner` retired on 2026-07-24 at 15:59 UTC. B08 now defaults fresh heycode worlds to V4 Flash and migrates only setup-fingerprinted older home configs.
- V4 supports OpenAI Chat Completions and Anthropic formats, 1M context, thinking/non-thinking modes and tool use.
- Thinking defaults enabled/high. The conservative cross-model profile exposes exact high/max efforts plus none for disabled; enabled thinking does not send temperature or other unsupported sampling controls.
- Tool turns in thinking mode require the complete `reasoning_content` to be passed back.
- The API also has strict tool calling, JSON output and beta prefix/FIM surfaces.

References: [V4 release](https://api-docs.deepseek.com/news/news260424), [model listing](https://api-docs.deepseek.com/api/list-models), [thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/), [change log](https://api-docs.deepseek.com/updates/).

### MiniMax

- MiniMax offers OpenAI-compatible and Anthropic-compatible model-list and inference endpoints.
- M2 tool turns require the full assistant response and reasoning state to be preserved.
- Token Plan credentials and pay-as-you-go keys are distinct.
- Token Plan includes web search and image-understanding MCP tools.

References: [API overview](https://platform.minimax.io/docs/api-reference/api-overview), [OpenAI-compatible API](https://platform.minimax.io/docs/api-reference/text-openai-api), [models](https://platform.minimax.io/docs/api-reference/models/openai/list-models), [Token Plan MCP](https://platform.minimax.io/docs/guides/token-plan-mcp-guide).

### Z.AI / GLM

- The general and Coding Plan endpoints differ. Coding Plan usage is restricted to supported coding-tool scenarios.
- The coding plan includes official web search, web reader and vision MCP capabilities.
- Current GLM models expose thinking, function calling, caching and structured output with model-specific limits.

References: [quick start](https://docs.z.ai/guides/overview/quick-start), [Coding Plan](https://docs.z.ai/devpack/quick-start), [model overview](https://docs.z.ai/guides/overview/overview), [web search](https://docs.z.ai/guides/tools/web-search).

### LM Studio

- Native `/api/v1/models` describes downloaded and loaded models and whether a model was trained for tool use.
- LM Studio supports native chat plus OpenAI Responses, Chat Completions and Anthropic Messages endpoints with different capability sets.
- Its native API can load and unload models with context and hardware settings.

References: [REST API](https://lmstudio.ai/docs/developer/rest), [list models](https://lmstudio.ai/docs/developer/rest/list), [load model](https://lmstudio.ai/docs/developer/rest/load), [tool use](https://lmstudio.ai/docs/developer/openai-compat/tools).

### Amazon Bedrock

- Model discovery differs between Bedrock Runtime (`ListFoundationModels`) and Bedrock Mantle (`/models`).
- Bedrock exposes Converse, native invoke, OpenAI-compatible Responses/Chat Completions and Anthropic Messages families with model-specific compatibility.
- Authentication may use a Bedrock API key or the standard AWS credential chain.
- Prompt caching, guardrails, cross-region inference, tool use and model lifecycle metadata are provider capabilities, not universal assumptions.

References: [list models](https://docs.aws.amazon.com/bedrock/latest/userguide/models-get-info.html), [API compatibility](https://docs.aws.amazon.com/bedrock/latest/userguide/models-api-compatibility.html), [endpoints](https://docs.aws.amazon.com/bedrock/latest/userguide/endpoints.html), [prompt caching](https://docs.aws.amazon.com/bedrock/latest/userguide/prompt-caching.html).

### Vertex AI

- Vertex hosts Gemini, partner models including Claude, and open-model MaaS offerings with different APIs and capabilities.
- Gemini supports function calling, Google Search grounding, external search grounding, code execution, implicit/explicit caching and model-specific thought signatures.
- Claude on Vertex uses the Anthropic request family with Google Cloud authentication.
- Application Default Credentials, project and location are part of the route identity.

References: [Vertex AI overview](https://docs.cloud.google.com/vertex-ai/generative-ai/docs), [Google Search grounding](https://cloud.google.com/vertex-ai/generative-ai/docs/multimodal/ground-with-google-search), [code execution](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/model-reference/code-execution-api), [thought signatures](https://docs.cloud.google.com/vertex-ai/generative-ai/docs/thought-signatures), [Claude partner models](https://cloud.google.com/vertex-ai/generative-ai/docs/partner-models/use-claude).

### Local sandbox and process containment

- Linux Landlock is an unprivileged, stackable restriction layer inherited by future children. Its ABI exposes filesystem rights and newer optional network rights, but heycode currently constructs filesystem rules only; it must report host networking as available.
- Bubblewrap exposes read-only binds, writable binds/tmpfs, PID namespaces, parent-death behavior and optional network unsharing as separate flags. The current heycode argv uses read-only root, workspace bind/private tmp, PID namespace and die-with-parent, but not network unshare; it therefore cannot claim network isolation.
- macOS Seatbelt capability claims are grounded in the exact generated profile plus the E12 native runtime matrix because `sandbox-exec` is a platform facility without a current public product contract suitable for stronger portability claims. The current profile is filesystem-write-only and deliberately permits TCP/Unix connections.
- Process-tree containment is separate from filesystem sandboxing. processkit uses Job Objects, cgroup v2/process groups or a FreeBSD reaper and exposes the selected mechanism; POSIX process groups remain vulnerable to a deliberate `setsid` escape.
- Windows Job Objects group and terminate process trees, including kill-on-last-handle-close, but they do not grant filesystem/network isolation. AppContainer is the relevant default-deny resource boundary and must be configured at process creation. Microsoft's composable Create Process in Sandbox API exposes that shape but is explicitly experimental; it cannot be the current stable `windows-2022` backend claim.

References: [Linux Landlock userspace API](https://kernel.org/doc/html/latest/userspace-api/landlock.html), [bubblewrap source/option contract](https://github.com/containers/bubblewrap/blob/main/bubblewrap.c), [processkit repository](https://github.com/ZelAnton/ProcessKit-rs), [Windows Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects), [AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation), [Create Process in Sandbox](https://learn.microsoft.com/en-us/windows/win32/secauthz/createprocessinsandbox), [GitHub macOS runner image matrix](https://github.com/actions/runner-images/blob/main/README.md).

### Attachment image metadata

- image-rs 0.25.10 exposes `ImageReader::with_format(...).into_dimensions()` over a `Read + Seek` source, reading dimensions through the selected decoder without decoding a full pixel buffer.
- ATT01 enables only PNG/JPEG/GIF/WebP codecs and applies heycode's own byte, side and total-pixel ceilings. The crate API is format parsing evidence, not a substitute for durable MIME/content/hash validation.

References: [image-rs ImageReader](https://docs.rs/image/0.25.10/image/struct.ImageReader.html), [image-rs repository](https://github.com/image-rs/image).

### Untrusted HTML/PDF extraction

- lopdf 0.44 documents `LoadOptions::max_decompressed_size`, bounded stream/page content and `extract_text_with_limit` specifically for hostile PDF decompression bombs. Its 0.42 line also fixed the deeply nested-object stack-overflow advisory; WEB03 pins the newer 0.44 family.
- html2text 0.17.1 parses an `io::Read` source and renders wrapped plain text/links. heycode still applies its own raw/output limits, UTF-8 cap, source capture and untrusted-content boundary.
- A parser timeout cannot safely detach Rust blocking work. WEB03 cancels its bounded worker and then joins it; hard-kill containment would require moving extraction behind the subprocess/runtime boundary rather than dropping the handle.

References: [lopdf bounded stream API](https://docs.rs/lopdf/0.44.0/lopdf/struct.Stream.html), [lopdf load options](https://docs.rs/lopdf/0.44.0/src/lopdf/load_options.rs.html), [lopdf repository/changelog](https://github.com/J-F-Liu/lopdf), [html2text 0.17.1](https://docs.rs/html2text/0.17.1/html2text/), [html2text repository](https://github.com/jugglerchris/rust-html2text/).

## Research conclusions

1. A single OpenAI-compatible client cannot deliver correct multi-provider behavior.
2. Subscription-backed products belong behind an agent-runtime seam, not an inference-provider seam.
3. Provider/model capability discovery must be live, cached and explicit.
4. Provider-owned reasoning and continuation state must be preserved durably and replayed byte-for-byte where required.
5. Native tools and native compaction need provider strategies selected by capability, with portable fallbacks.
6. Interactive setup, commands, settings, MCP and diagnostics are product capabilities and therefore plugins.
7. Configuration migration is a first-class release concern; a stale explicit profile can make shipped plugins disappear.
8. Live credential validation and actionable error taxonomy are required before a provider is considered configured.
