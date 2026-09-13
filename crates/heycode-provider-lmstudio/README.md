# heycode-provider-lmstudio

Provider-owned LM Studio endpoint, auth and health detection.

PLM01 answers three questions about a local LM Studio install without assuming
any of them: is the server there, which published release is it *at least*, and
which protocol families does it actually serve. Plugin `provider-lmstudio`
publishes `LmStudioDetector` under service `lmstudio`; PLM02 owns the model list
and capability mapping and is the intended consumer.

Three properties of LM Studio shape everything here.

**There is no version, health or status endpoint.** The request for one is an
open, unanswered issue
([bug-tracker #920](https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/920)),
and no documented response body carries an application version. So a version is
never claimed exactly. `LmStudioVersion` is `Unknown` or `AtLeast`, and the only
bound this crate can cite is `AtLeast("0.4.0")`, proved by a recognized native
REST v1 surface — "With LM Studio 0.4.0, we have officially released our native
v1 REST API at `/api/v1/*` endpoints"
([API changelog](https://lmstudio.ai/docs/developer/api-changelog)). A v0-only
server bounds nothing: the changelog neither dates v0's arrival nor its removal.

**The server answers `200 OK` on paths it does not route**
([bug-tracker #1323](https://github.com/lmstudio-ai/lmstudio-bug-tracker/issues/1323):
`"[ERROR] Unexpected endpoint or method. (GET /swagger.json). Returning 200
anyway"`). A status code is therefore only a gate, never evidence. Every probe
validates the surface's documented body shape: the native v1 list is
`{"models":[…]}`
([List Models](https://lmstudio.ai/docs/developer/rest/list)) while the legacy
v0 list and the OpenAI-compatible list are both `{"object":"list","data":[…]}`
([REST v0](https://lmstudio.ai/docs/developer/rest/endpoints)), so the path is
what separates those two.

**Every inference route is `POST`-only**, and probing one would load a model. So
each surface is observed through its own model-list `GET`, and a protocol with
no observable surface stays `Unknown`. Concretely: a recognized
`GET /v1/models` proves the OpenAI-compatible surface that also serves
`/v1/chat/completions`
([OpenAI compatibility](https://lmstudio.ai/docs/app/api/endpoints/openai)), so
`OpenAiChatCompletions` reaches `Supported` and names the surface that proved
it. `OpenAiResponses` (`POST /v1/responses`, LM Studio 0.3.29) and
`AnthropicMessages` (`POST /v1/messages`, LM Studio 0.4.1) stay `Unknown`
forever under this design — knowing a release *shipped* an endpoint is not
observing that *this* server routes it, and a version lower bound is not a
protocol observation. Unknown is never promoted to Supported (GOTCHAS #22), and
this crate never emits `Unsupported` at all: a probe can prove a surface is
present, never that it is absent.

Health is four distinct outcomes, not a boolean. Every probe refused is
`NotRunning` — an ordinary, expected local state reported as a verdict rather
than an error the user must decode. A probe that timed out, was cancelled or ran
out of budget is `Indeterminate`, deliberately **not** `NotRunning`: a hung
server is not an absent one. An endpoint that answered without a documented
shape is `RunningUnrecognized`. Only a recognized shape is `Running`, and even
then a recognized OpenAI-compatible list alone does not identify the product —
many local runtimes serve one — so `identified_lm_studio()` requires a native
REST surface.

Detection is bounded by one budget shared by the whole run, not granted per
probe, so a hung local server costs one timeout rather than three. Each probe
gets only the remainder; an exhausted budget skips the request entirely and
records `Indeterminate`.

"By default, LM Studio does not require authentication for API requests"
([Authentication](https://lmstudio.ai/docs/developer/core/authentication)), and
that posture is modelled explicitly as `LmStudioAuth::None` rather than as an
absent credential field. `LmStudioCredentialState` separates the four real
answers: an accepted **unauthenticated** probe proves `NotRequired`, an accepted
**authenticated** probe is only `Accepted` (acceptance does not prove the token
was needed), a `401`/`403` anywhere is `Required` and outranks any acceptance,
and nothing reachable is `Unknown`. A configured bearer query that resolves to
nothing falls back to an unauthenticated probe, because a server that does need
a token then answers `401`, which is the actionable verdict. The plugin injects
`credentials` only when a token is actually configured; the resolved secret
lives for one run and never reaches `Debug`, `Display` or an error.

The deterministic suite drives an injected `HttpTransport`. The separately
gated PLM05 canary is the only test allowed to reach the documented local
Ollama origin, and it runs only under an explicit environment switch.

PLM01 leaves two limits open. LM Studio publishes no verbatim body for its
OpenAI-compatible `GET /v1/models`, so that surface is held to the same
documented list envelope its own v0 example serves — a deviation there would be
a false `Unknown`, never a false `Supported`. And detecting the Responses or
Anthropic surfaces at all would need a `POST` probe, whose safety (JIT model
loading) and shape are not settled by any published document.

## PLM02 — native model list and capability mapping

Plugin `catalog-lmstudio` registers the native model list into the shared
`"models"` catalog and publishes the LM Studio-owned records under
`"lmstudio/models"`. It reads the documented
[`GET /api/v1/models`](https://lmstudio.ai/docs/developer/rest/list).

That endpoint enumerates the local library, so **every row is downloaded**; a row
whose `loaded_instances` is non-empty is additionally **loaded**. Neither state
is availability — LM Studio loads a downloaded model on demand, so a
downloaded-but-not-loaded model is ready, never missing. The two contexts stay
separate: a loaded instance's `config.context_length` is its running
configuration (the documented example loads a 262,144-token model at 4,096) and
never substitutes for the model's `max_context_length`.

Capability mapping keeps three distinct facts apart, because the endpoint
publishes all three. An explicit `"vision": false` is real evidence of absence
and becomes `Unsupported`. An absent key is not a denial and stays `Unknown`. An
absent `capabilities` object — what an embedding model has — leaves every
component `Unknown`. `Unknown` is never promoted to `Supported`, which matters
because `trained_for_tool_use` is what PLM03 uses to keep a chat-only model out
of agent mode. Note that `capabilities.reasoning` is an **object**
(`{"allowed_options": […], "default": …}`), not a boolean; its published options
are retained.

Only `type: "llm"` reaches the routing catalog — offering an embedding model as a
chat route would be a real defect — while every row, embeddings included, stays
visible through `list_models`. The v1 list documents `"llm" | "embedding"`, but
the legacy v0 list also emitted `"vlm"`, so an unrecognized `type` is retained
verbatim and simply never routed, rather than dropped or assumed chattable.

The source claims **no protocol**: reading the model list proves the native REST
surface, not which inference protocol this server speaks. `ProviderDescriptor`
therefore carries `ProviderProtocol::Unknown`, and a consumer joins it with
PLM01's detector rather than the catalog guessing. LM Studio publishes no output
cap, lifecycle, price or performance evidence for a local model, so those stay
unknown rather than invented — and unknown is not zero.

One generation is all-or-nothing: a malformed envelope, a blank or duplicate
model key, or a documented body under a non-JSON content type rejects the whole
list rather than publishing a partial library. The read carries its own deadline
because the registry supplies cancellation but no timeout, so a hung local server
cannot stall a refresh.

## PLM03 — conservative agent-mode route validation

PLM02 keeps three distinct facts about tool training; PLM03 decides what each
means for agent mode. **Only proven support is offered.** An explicit
`trained_for_tool_use: true` on a recognized chat model is eligible; everything
else is refused, because being wrong about an unproven model costs a user a
silently broken agent loop rather than a clear refusal.

Refusing and explaining are separate jobs. `LmStudioAgentRefusal` keeps four
reasons apart, and the two that matter most must never collapse into one
message: a model LM Studio published as `trained_for_tool_use: false` is
**proven incapable**, while a model it published nothing about is **unproven**.
`is_published_denial()` is the accessor a consumer checks before rendering "this
model cannot use tools" — it is `false` for `ToolTrainingUnproven` and
`UnrecognizedModelKind`, because those mean heycode lacks evidence, not that
LM Studio denied anything.

`LmStudioRouteError::UnknownModel` stays distinct from every refusal for the
same reason: a model never seen is not a model judged incapable.

Withholding agent mode withholds only agent mode. A refused model is still a
usable chat model, and a test pins that so the two decisions cannot merge.

**Scope note.** This is the *model* dimension. It does not assert that the
server speaks a protocol able to carry tool calls — that evidence lives in
PLM01's `LmStudioServerReport`, and joining the two is a route-composition
concern above this crate.

## PLM04 — explicit model load/unload control

`LmStudioModelControl` owns the documented native REST v1 management endpoints:
`POST /api/v1/models/load` and `POST /api/v1/models/unload`. A load begins as a
pure, non-Clone `LmStudioLoadPlan`; preparing or selecting a downloaded model
performs no request. The explicit operation may set context length, evaluation
batch size, Flash Attention, MoE expert count and GPU KV-cache offload. It
always sends `echo_load_config: true` and returns a receipt only when every
specified value matches LM Studio's applied configuration.

Unload targets one validated `instance_id`, never a model key guessed into an
instance. Cancellation, authentication, status and response failures publish no
receipt. `require_loaded` is the provider-local no-surprise-load policy: a
downloaded-only model must be explicitly loaded before inference selection.
The existing `provider-lmstudio` plugin publishes the raw control boundary as
`lmstudio/model-control`. Default product plugin `lmstudio-control` is its
Consumer. It owns live Settings namespace `lmstudio-load` and queued
`/lmstudio <load|unload> <target>`. Numeric modes distinguish server default
from an explicit bounded value; Flash Attention and GPU KV-cache placement are
three-state. Load rereads Settings, admits only a downloaded affirmative
tool-capable chat model, refuses duplicate loaded state, verifies the provider
echo and confirms the exact instance in a fresh native list. Unload requires a
currently observed instance and confirms its removal. Both then force-refresh
the shared model catalog before publishing success.

LM Studio's current official docs describe global JIT behavior and expose its
switch in Server Settings, but publish no stable REST/CLI mutation for it. heycode
therefore never edits LM Studio's explicitly unstable private config. Its own
path permits no implicit load: only the explicit command calls the load
endpoint, while future inference selection must consume the already-loaded
record/instance guard. An external client may still change LM Studio memory;
each heycode operation rereads authoritative native state rather than trusting a
cached assumption.

Sources: [load endpoint](https://lmstudio.ai/docs/developer/rest/load),
[unload endpoint](https://lmstudio.ai/docs/developer/rest/unload), and
[server JIT settings](https://lmstudio.ai/docs/developer/core/server/settings).

## PLM05 — Ollama sibling boundary

Ollama is a sibling product with its own `ollama` identity. It never reuses LM
Studio endpoints, auth assumptions, native response types, settings or catalog
records. Standalone `catalog-ollama` remains available; the stronger
`provider-ollama` plugin owns the joined catalog plus concrete `ollama/catalog`,
`ollama/profile`, `ollama/inference` and `ollama` inspector services as Context
effects.

Picker publication is a five-surface join, not an optimistic label:

- `GET /api/version` establishes Ollama product identity;
- `GET /api/tags` supplies downloaded model identity/storage metadata;
- `GET /api/ps` adds exact current loaded context/VRAM/expiry state;
- read-only `POST /api/show` supplies the exhaustive model capability list and
  architecture-specific context length;
- `GET /v1/models` proves the same id is exposed by Ollama's documented
  OpenAI-compatible surface.

Only the intersection reaches the shared picker catalog, and only a model whose
`/api/show` capabilities include `completion` is chattable. `tools`, `vision`
and `thinking` map independently to Supported/Unsupported; unfamiliar future
capability strings are retained without becoming support for a known feature.
Native-only `list_models` still has protocol Unknown, while the joined picker
descriptor and explicit profile both say `OpenAiChatCompletions`. This keeps
the evidence layers separate without leaving descriptor drift in the picker.

`OllamaInference` uses the shared Chat adapter at `/v1/chat/completions`, sends
the documented `ollama` required-but-ignored compatibility key, and exposes no
credential reference for the local endpoint. `OllamaInspector` performs the
same bounded read-only join and returns picker metadata, but
`live_smoke_passed()` remains false: version/list/show/ps observations are not a
generated chat response. Deterministic tests never call a real chat, generate,
pull, create, copy, delete or any model-lifecycle endpoint. The explicitly
gated `HEYCODE_OLLAMA_E2E=1` canary first completes the five-surface join, then
sends one no-tools Chat request through `OllamaInference`; it withholds response
text and fails on any tool-call delta. It never starts a daemon or downloads or
mutates a model.

Product reachability is conditional and explicit. Root registers all four
service keys and mounts `provider-ollama` only when `[llm] provider = "ollama"`
already carries a model. The ordinary `llm` plugin bridges the published
`OllamaInference` into the shared provider registry without a credential;
provider/profile/catalog descriptors then reach the standard route/model
pickers. Composition performs no HTTP or process action, and a configured
credential reference fails before lookup. Nothing pulls a model or starts a
server. On 2026-08-31 this host had neither an `ollama` executable nor a daemon
answering `http://127.0.0.1:11434/api/version`, so no model identity or Chat
response was observed and PLM05 remains active. First-run setup also still
cannot choose an Ollama model without one being discovered.

Run the live gate only against an already-installed, already-downloaded model:

```sh
HEYCODE_OLLAMA_E2E=1 HEYCODE_OLLAMA_MODEL='<exact-local-model-id>' \
  cargo test -p heycode-provider-lmstudio \
  it::live_ollama::installed_ollama_picker_and_chat_smoke_are_explicit_and_tool_free \
  -- --exact
```

Sources: [Ollama API introduction](https://docs.ollama.com/api/introduction),
[version](https://docs.ollama.com/api-reference/get-version),
[native model list](https://docs.ollama.com/api/tags),
[running models](https://docs.ollama.com/api/ps),
[model details](https://docs.ollama.com/api-reference/show-model-details), and
[OpenAI compatibility](https://docs.ollama.com/api/openai-compatibility).

LM Studio now constructs a strict Chat inference route. Preparation rechecks the exact loaded instance, tool-training evidence and observed Chat surface. The shared picker contains loaded instance ids; the native catalog service still exposes downloaded/embedding records. Ollama exposes its catalog before selection and labels running/downloaded models.

LM Studio and Ollama implement read-only draft endpoint discovery without inherited credentials. LM Studio still lists only loaded instance ids in the shared inference picker.

LM Studio draft endpoint discovery supports an explicitly entered bearer key. Authenticated and unauthenticated operations share no credential state; a regression verifies a key for one address is absent from the next address. Ollama also accepts optional explicit server credentials.

Ollama authenticated setup uses the exact native discovery URLs with an operation-time credential, while Chat binds the same reference through the shared inference adapter. Draft endpoints never inherit the active key. Unauthenticated local Ollama retains its existing behavior; 401/403 discovery failures are classified as authorization failures. Twelve Ollama regressions pass, including native key isolation and the Chat key replacing the placeholder.

Unauthenticated native-loop Chat inference now uses the explicit no-auth adapter
binding and sends no Authorization header. Configured bearer tokens still resolve
per operation. Loaded-instance/tool capability preparation remains authoritative.
