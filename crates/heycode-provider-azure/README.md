# heycode-provider-azure

Provider-owned Microsoft Azure OpenAI v1 catalog/readiness and inference
boundaries.

This crate implements one deliberately narrow product route:

- provider id `azure-openai`;
- a validated Azure resource name used only to construct
  `https://<resource>.openai.azure.com/openai/v1`;
- one exact deployment name used as both the readiness target and the
  Responses request `model`;
- an operation-time `api-key` credential reference; and
- the shared strict OpenAI Responses request, SSE, provider-state and replay
  implementation.

It does not treat Azure as an arbitrary OpenAI-compatible endpoint. Azure
resource/deployment coordinates are provider-owned types and the inference
wrapper rejects every model other than the configured deployment before any
network request.

## Catalog and setup readiness

`catalog-azure-openai` is always safe to mount for setup. With no active saved
coordinates its ordinary refresh is credential-blind and returns no rows. A
draft probe requires exactly `resource` and `deployment`, resolves either the
explicit masked draft key or the configured credential reference, and sends
one bounded bodyless:

```text
GET https://<resource>.openai.azure.com/openai/v1/models/<deployment>
api-key: [operation credential]
```

Only an exact model-object response for the requested deployment becomes a
row. The response does not establish tool, reasoning, structured-output or
hosted-web support, so those capabilities remain `Unknown`. Draft generations
are never inserted into the active catalog cache.

Status, cancellation, malformed response and transport failures use bounded
body-free classifications. Resource and deployment validation happens before
credential lookup or I/O.

## Inference and credentials

`inference-azure-openai` contributes the exact selected deployment through the
normal provider registry. It uses `/openai/v1/responses`, sends the deployment
in the JSON `model` field and sends the current operation credential in the
`api-key` header. The shared Responses adapter defaults to bearer
`Authorization`; Azure opts into the distinct header explicitly.

The plugin retains a credential reference, not a key value. Each inference
operation resolves the reference once, retries reuse that operation's value,
and a rotation reaches the next operation without recomposition. Registration
of both catalog and inference rows is effect-owned and unwinds at Context
shutdown.

Microsoft Entra ID authentication is not implemented here. Supporting it
requires an owned token-refresh/identity provider rather than accepting a
short-lived bearer token as though it were a durable API key. No live Azure
account or credential is required by this crate's tests; fixtures prove exact
wire behavior and production composition proves reachability.


Azure deployments and custom OpenAI endpoints explicitly attempt ordinary
function-tool requests when tool capability evidence is Unknown. The catalog
continues to report Unknown, including after a successful call. Explicit
Unsupported still rejects locally; other unknown capabilities and other
provider routes retain strict evidence requirements. Requests preserve their
tool schemas and durable tool-result replay, and endpoint failures surface
without removing tools or switching protocols.
