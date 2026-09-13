# @heycode/sdk

Transport-neutral TypeScript client for heycode app-server protocol v1.
The runnable `.ts` example requires Node 24.12 or newer, where built-in type
stripping is stable; published consumers use the compiled `dist` output.

The caller supplies a `HeycodeTransport` that exchanges one raw JSON-RPC request,
streams raw notification frames to a callback, honors an optional
`AbortSignal`, and returns one raw response. The SDK owns JSON serialization,
response-id checks, closed event decoding, session correlation and contiguous
per-operation notification validation.

`start()` opens the new session selected by the host. `resume(expectedId)`
opens the session selected by the host and refuses a different id. `turn()`
streams typed events through a callback while its promise remains owned;
`cancel()` may run from the same client concurrently and both promises must be
awaited. Raw frames may contain a transient authorization answer and must never
be logged by a transport.

`startWithConfiguration(configuration)` and
`resumeWithConfiguration(expectedId, configuration)` can request an exact
system prompt, ordered tools, model and reasoning effort. An empty `tools` array
is preserved as an explicit catalog. Session/runtime rows expose the effective
configuration and tri-state per-field capability evidence; `configure()`
applies supported between-turn updates. `runtimeModels()` returns
provider-native model ids with their exact advertised effort choices and
default. Missing fields from an older v1 host default to an empty configuration
with `unknown` evidence.

Runtime/workspace capability flags default to false when an older v1 host omits
them. Current clients can list/select agent runtimes and request a workspace;
the host remains the authority that canonicalizes the path and decides whether
the selected runtime is relocatable.

Hidden ATT04 output arrives as `assistant_audio` with validated
duration/rate/channel/depth attachment metadata only. The decoder rejects
unknown attachment members and invalid audio bounds, so a raw `data`/`bytes`
field cannot be smuggled into the typed event. No initialize flag or provider
catalog capability advertises this experimental path.

The installable extension in `editors/vscode` bundles this package and provides
the explicit child-process stdio transport. The SDK itself still owns no child,
socket, endpoint permission or logging policy.

```ts
const client = new HeycodeClient(transport);
const session = await client.start();
const controller = new AbortController();
const turn = client.turn("hello", [], event => render(event), controller.signal);
await client.cancel();
await turn;
```

For `question_requested`, render `header`, `choices`, and aligned
`choice_descriptions` when present. Submit a chosen label or custom text with
`respondQuestion(requestId, answer)`, or reject the dialog with
`cancelQuestion(requestId)`.

Build and test with the project-pinned compiler:

```sh
npm ci
npm test
```
