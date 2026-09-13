# heycode-provider-openai-compatible

Provider-owned boundary for one user-supplied OpenAI Chat Completions server.
The provider id is `custom-openai`; heycode never guesses or detects another
protocol for it.

The configured URL is an absolute HTTP(S) version root, including `/v1` when
the server requires it. It must not contain credentials, a query or a fragment.
heycode appends exactly two routes:

```text
GET  <version-root>/models
POST <version-root>/chat/completions
```

The model-list request is bodyless, JSON-only, bounded to 4 MiB and at most
4,096 unique canonical model rows. A listed model proves identity only. Every
capability, including tool calling, remains `Unknown`; generic compatibility
is not provider evidence. Setup can also retain one explicit model id when the
server does not expose canonical discovery or is temporarily unreachable.

Authentication is an explicit route choice. An unauthenticated route sends no
`Authorization` header. A configured key is retained only as a validated
credential reference and resolves once per operation into
`Authorization: Bearer ...`; rotation reaches the next catalog or inference
operation without recomposition. URL, model and optional reference are staged
and restored together by the routing layer.

`catalog-custom-openai` is setup-safe with no active URL and performs no I/O at
composition. `inference-custom-openai` is registered only for a complete saved
URL/model route. Both contributions are effect-owned and disappear on Context
shutdown.

This crate does not start, stop, install, load models into or otherwise manage
the supplied server. It also does not claim Responses, Anthropic Messages,
provider-native tools, structured output, reasoning, vision, prompt caching or
any other behavior beyond the exact Chat Completions request/stream parser.

The deterministic suite uses injected transports. No local server, network
endpoint or real credential is needed.


Azure deployments and custom OpenAI endpoints explicitly attempt ordinary
function-tool requests when tool capability evidence is Unknown. The catalog
continues to report Unknown, including after a successful call. Explicit
Unsupported still rejects locally; other unknown capabilities and other
provider routes retain strict evidence requirements. Requests preserve their
tool schemas and durable tool-result replay, and endpoint failures surface
without removing tools or switching protocols.
