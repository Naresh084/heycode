# Connection onboarding implementation tasks

Requested 2026-09-05. Checkboxes require observed acceptance evidence.

Scope decision: cloud configuration forms and custom-server setup were implemented in a separate isolated task and integrated into the primary checkout at the user's request. All unfinished entries, mock choices, teasers and hints remain out of product UI. The five phases and their evidence are recorded in [the isolated sequential plan](deferred-cloud-local-connections.md); the branch was not merged into the primary checkout automatically. LM Studio/Ollama, the exact custom Chat route and currently implemented provider/subscription/cloud connections are the visible scope.

## Accepted product contract

Exactly three entry points: Use a subscription, Use a local model, Select a provider. API and cloud providers share the provider picker. The invocation directory is the session workspace. Setup appears when no usable connection exists, and /connect explicitly reopens it. A saved connection survives restart. A timeout, empty catalog, missing local server or rejected credential keeps the connection and offers targeted recovery; it must not silently clear it or restart first-run setup.

## Tasks and acceptance

- [ ] C01 — Startup and persistence. Derive first-run, saved-ready, saved-unverified and reconnect states from durable selection plus actual credential/runtime facts. Do not use an unrelated default API credential to decide readiness of a subscription or local connection. Prove fresh home → setup, successful setup → composer, second launch → composer, temporary network failure → preserved route, rejected credential → named recovery. Do not persist a ready flag before the connection commits.
- [ ] C02 — Three-choice wizard. Keep cloud providers inside Select a provider and preserve the readable title/description hierarchy, selected background, scrolling, footer and Back semantics. Make provider, cloud and local search/edit forms keyboard accessible; never promote text to a model prompt.
- [ ] C03 — Connection catalog. Plugin-owned identity, family, authentication fields, model discovery and exact adapter binding. Every advertised target must either be usable or state a concrete missing prerequisite before requesting credentials. Provider metadata is independent of the currently active inference route.
- [ ] C04 — Local connection setup. LM Studio and Ollama first-class discovery, endpoint editing, optional authentication, installed/running model distinction, exact model selection and real inference. Extend through supported protocols to Jan, GPT4All, llama.cpp, llamafile, KoboldCpp, Text Generation WebUI, vLLM, SGLang, LocalAI, Xinference, Docker Model Runner, Lemonade and MLX-based servers. Native start/load/download only where the runtime exposes a reviewed management interface; otherwise show exact external instructions. Custom URL + protocol + optional credential + model handles other compatible servers. Preserve endpoint/model across restart.
- [ ] C05 — Direct providers. Add Fireworks and Groq, extend the maintained catalog for OpenAI, Anthropic, Gemini, xAI, DeepSeek, Mistral, Cohere, AI21, Moonshot, MiniMax, Z.ai, Alibaba/Qwen, Together, Cerebras, SambaNova, DeepInfra, Nebius, Novita, Hyperbolic, FriendliAI, NVIDIA NIM, Hugging Face, Replicate and Baseten. Protocol claims require actual adapter admission; catalog-only rows are not inference support. Use reviewed official endpoints and authentication, with model discovery or explicit model entry when listing is unavailable.
- [ ] C06 — Cloud and gateways in provider selection. Amazon Bedrock/SageMaker, Google Vertex and Microsoft Azure/Foundry have their own account/project/region/deployment forms. Expose the existing compatible gateway path for OpenRouter, LiteLLM and Vercel. Include Cloudflare, IBM watsonx, OCI, Databricks and Snowflake only with their real protocol/auth adapter. Show product differences explicitly; a cloud account is not an API-key synonym.
- [ ] C07 — Subscription setup and recovery. Keep official Codex/Claude/Grok runtime ownership; signed-in accounts must be reused. Missing executable/version/sign-in gets provider-owned installation or login guidance plus retry. Do not extract vendor tokens. Direct subscription access remains unavailable unless a documented integration contract supports it. Preserve successful tool-free live turn evidence already collected; rerun only if the relevant runtime implementation changes.
- [ ] C08 — Commit and workspace boundary. Connection persistence completes before publication; stale startup overrides cannot replace a newly chosen target. Pre-trust discovery stays outside the project. Starting actual inference/tools waits for the user's workspace policy. Failed new connection must not corrupt the previous saved connection.
- [ ] C09 — Verification and docs. Add focused regressions for changed behavior, run required fmt/clippy/workspace gates on the finished implementation and verify the actual TUI. Stop testing after relevant checks pass; no repeated live calls or unrelated expansion. Update STATUS, GOTCHAS, affected READMEs and authoritative inventory for every meaningful change. Record unavailable live environments honestly.

## Subscription decision sources

- Codex App Server: https://developers.openai.com/codex/app-server
- Grok official ACP/headless interface: https://docs.x.ai/build/cli/headless-scripting
- Claude credential-use restrictions: https://code.claude.com/docs/en/legal-and-compliance

These sources support runtime-based integration. No universal direct consumer-subscription API is assumed.
