# heycode-provider-compatible

Provider-owned Chat Completions bindings for Fireworks AI, Groq, Mistral AI, Together AI and xAI. This crate keeps the shared adapter, masked API-key flows and bounded catalog transport together while retaining each provider's response dialect and model evidence. It depends on the protocol and authorization layers; those layers do not depend on vendor authorization behavior.

`catalog-compatible` registers the model catalogs with effect-owned disposal. The production composition root constructs the selected `CompatibleProvider`, and setup uses the same safe profiles. Credentials resolve through the credential registry once per operation. An explicit endpoint override keeps validation and discovery on that authority.

Fireworks lists native account metadata with bounded pagination and offers ready serverless chat models. Missing capability fields stay unknown. Groq reads the live active-model list and applies tool evidence only to exact documented model ids. Protocol compatibility alone supplies no intrinsic capability.

Validation: seven catalog tests cover pagination, unavailable models, missing capabilities and malformed ids. The shared raw-SSE conformance runner checks every maintained provider across every byte split. A production-loader regression preserves OpenRouter and checks profiles, authorization flows and catalogs for all maintained connections. Live Fireworks/Groq account turns have not been observed.

Reviewed 2026-09-05:

- [Fireworks Chat compatibility](https://docs.fireworks.ai/tools-sdks/openai-compatibility)
- [Fireworks model catalog](https://docs.fireworks.ai/api-reference/list-models)
- [Fireworks model capabilities](https://docs.fireworks.ai/api-reference/get-model)
- [Fireworks tool calling](https://docs.fireworks.ai/guides/function-calling)
- [Groq Chat compatibility](https://console.groq.com/docs/openai)
- [Groq model tool support](https://console.groq.com/docs/tool-use/overview)
- [Groq remote tool model list](https://console.groq.com/docs/tool-use/remote-mcp)

Fireworks `deprecationDate` describes serverless shutdown. Full calendar dates become UTC-day retirement boundaries; partial dates retain deprecation without an invented instant, and absent/empty dates remain unknown. Source: https://docs.fireworks.ai/api-reference/get-model.

Mistral model discovery requires explicit `completion_chat` evidence and excludes archived models. Function calling and vision are independently projected from model-card fields; missing fields stay unknown. The authenticated production route uses the strict Chat Completions adapter. No live Mistral account turn has been observed. Reviewed 2026-09-05: [model cards](https://docs.mistral.ai/api/endpoint/models), [Chat API](https://docs.mistral.ai/api), [OpenAI migration contract](https://docs.mistral.ai/resources/migration-guides).

Together consumes the native task-typed model array and admits only chat rows. xAI uses the language-model endpoint, preserving explicit image modality evidence; unknown tool evidence stays unknown. Exact documented tool rows include Together Llama 3.3 70B Instruct Turbo and xAI Grok 4.6/4.3. Both have independent credential references, masked validation flows and real production Chat adapters; live account turns remain unobserved. Sources reviewed 2026-09-05: [Together compatibility](https://docs.together.ai/docs/inference/openai-compatibility), [model list](https://docs.together.ai/reference/models), [tool support](https://docs.together.ai/docs/serverless/models), [xAI catalog](https://docs.x.ai/developers/rest-api-reference/inference/models), [Chat protocol](https://docs.x.ai/developers/model-capabilities/legacy/chat-completions), [Grok 4.6](https://docs.x.ai/developers/grok-4-6), [Grok 4.3](https://docs.x.ai/developers/models/grok-4.3).

Selected compatible Chat routes now attempt ordinary function tools when model
metadata is Unknown, including newly listed gateway models. The evidence stays
Unknown; explicit Unsupported still refuses locally. Errors never cause a retry
with schemas removed or a protocol switch. This admits portable native-agent
features without asserting a model has intrinsic reasoning or vision.
