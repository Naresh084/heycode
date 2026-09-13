# Provider and runtime setup

heycode separates two route classes that look similar in a picker but own
different things:

- An inference provider exposes a model API; heycode owns the agent loop.
- An agent runtime (for example an official coding-agent process) owns the loop;
  heycode bridges its sessions and normalized events.

An API key for the first class is not a subscription login for the second. Do
not copy an official runtime's token files into heycode.

## Recommended: masked interactive authorization

Start with an isolated or deliberate home and restricted workspace authority:

```sh
# docs-check: syntax
export HEYCODE_HOME="$(pwd)/.heycode-evaluation"
cargo run -q -- --restricted-workspace
```

The setup UI derives methods from contributed authorization descriptors. A key
is entered into a masked, capped, zeroized input; the flow validates it, the
credential registry commits it, and only an authoritative safe readback allows
the shell to recompose. The key is not written to `heycode.toml` or a generated
reference.

From an existing shell:

```text
/connect
/provider
/model
/status
```

`/provider` distinguishes inference APIs, the native heycode loop, and delegated
agent runtimes. `/model` uses the selected provider's live or visibly cached
catalog; retired rows are not selectable and Unknown capability evidence never
passes a Supported filter.

## Terminal setup without the full TUI

The compatibility setup flow uses provider profiles and catalog services, not a
binary-maintained provider/model table:

```sh
# docs-check: syntax
setup_home="$(mktemp -d)"
HEYCODE_HOME="$setup_home/home" cargo run -q -- setup
```

The setup world cannot dispatch inference and creates no session, agent, tool,
TUI, or MCP process. A catalog failure is shown as a provider-default fallback;
only that unproven state permits a custom model id.

The metadata-only setup world lists provider-owned API profiles plus explicit
managed-cloud profiles. Amazon Bedrock, Google Vertex and Azure OpenAI require
their real non-secret coordinates rather than a fabricated setup default.

## Manual live smoke without a secret literal

This example is syntax-checked but never executed by the documentation gate.
It reads the key without echoing it or placing the value in the command line,
uses the provider-owned default model, and starts with restricted project
authority.

```sh
# docs-check: manual-live
read -r -s HEYCODE_PROVIDER_SECRET
printf '\n'
DEEPSEEK_API_KEY="$HEYCODE_PROVIDER_SECRET" cargo run -q -- --restricted-workspace run "reply with: live route reached"
unset HEYCODE_PROVIDER_SECRET
```

A successful response is one live text smoke for that credential, account,
route, model, date, and platform. It does not prove tools, continuation state,
native features, compaction, cost, every failure class, or another model.

## Configuration stores references, not values

Root TOML may select a provider/model and name an environment reference:

```toml
[llm]
provider = "provider-id"
model = "provider-native-model-id"
api_key_env = "PROVIDER_API_KEY_REFERENCE"
protocol = "auto"
```

`protocol` is schema-27 explicit. DeepSeek may select `openai_chat` or
`anthropic_messages`; Bedrock Mantle requires `openai_responses` or
`anthropic_messages`, and the Messages route additionally requires a positive
`max_output_tokens`. An incompatible provider/protocol pair fails before plugin
effects.

For a one-shot startup override, use the same validation path:

```sh
# docs-check: syntax
cargo run -q -- --provider deepseek --protocol anthropic_messages --model deepseek-v4-flash --restricted-workspace
cargo run -q -- --provider bedrock-mantle --protocol anthropic_messages --max-output-tokens 4096 --restricted-workspace
```

The value belongs to the credential provider chosen by precedence. Environment
is read-only and shadows lower writable stores; command helpers resolve exact
argv at operation time; persistent keys use `~/.heycode/credentials.toml` (or
`$HEYCODE_HOME/credentials.toml`). heycode never opens an OS keychain. Unix directory
and credential file permissions are `0700` and `0600`. Existing legacy text
credentials migrate locally; keys held only in an old OS store must be entered
again through `heycode setup` or `/connect`.
Startup preflight may validate a reference, but production API routes resolve
the value again once per operation so rotation reaches the next request.

AWS routes require a validated `AWS_REGION` or `AWS_DEFAULT_REGION`. Vertex
routes require explicit `GOOGLE_CLOUD_PROJECT` and `GOOGLE_CLOUD_LOCATION`
plus an operation-time OAuth-token credential reference. Composition performs
no cloud request; a caller-owned readiness phase resolves private catalog/GCP
profile evidence before the durable request target. Maintained Vertex model
rows do not claim that the account can call them.

Azure OpenAI routes persist a validated resource and deployment with an API-key
credential reference. Readiness probes the exact deployment through
`/openai/v1/models/<deployment>` and inference uses
`/openai/v1/responses`, placing the deployment in `model` and the current key
in the `api-key` header. Model capabilities remain Unknown unless a separate
source proves them. Microsoft Entra ID token acquisition is not implemented.

See the generated [configuration reference](../reference/configuration.md) for
current root keys/defaults. Do not infer capability support from a configured
provider/model string.

## Capability and support evidence

The generated [capability reference](../reference/capabilities.md) defines the
static vocabulary and route classes. Per-model facts stay in the live catalog
and picker by design; this guide does not freeze them.

Interpret evidence conservatively:

- `Supported`, `Unsupported`, and `Unknown` are separate states.
- Protocol compatibility does not prove model capability or product
  reachability.
- A gateway can evidence a gateway-provided feature, but cannot manufacture a
  model-intrinsic one.
- A deterministic fixture proves represented behavior, not an account or live
  endpoint.
- A composition proof shows that a route is reachable without proving a
  request succeeded.

The required live matrix is maintained in
[QUALITY_AND_RELEASE.md](../engineering/QUALITY_AND_RELEASE.md#live-provider-matrix).
Current implementation evidence and gaps are in
[PROVIDERS.md](../engineering/PROVIDERS.md), not duplicated here.

## Custom and additional providers

Use `llm.base_url` only for an endpoint whose protocol and authority you have
reviewed. A compatible URL is not enough to promise replay, reasoning, tools,
catalogs, errors, token counts, or native features. Unknown combinations must
fail before network I/O.

To implement a route rather than configure one, follow the
[provider-authoring guide](provider-authoring.md). Its completion unit is the
auth → catalog → exact replay → native feature → composition → evidence
vertical, not merely a client that can emit text.
