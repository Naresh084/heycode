# heycode-http

Provider-neutral HTTP execution, bounded buffered responses, raw SSE framing
and the deterministic P13 streaming-transport seam.

## HTTP and SSE

`HttpService` owns validated credential-free URLs, opaque headers/bodies,
cancellation, response-size/deadline bounds and URL/body-free transport errors.
The reqwest/rustls implementation follows no redirects. `SseDecoder` frames
arbitrary byte splits, BOM/line-ending variants, ids, retry hints and joined
`data` fields under a one-MiB event cap; provider JSON and `[DONE]` semantics
belong to protocol adapters.

## Dynamic response ownership

`HttpTransport::stream_response` is the additive provider-neutral seam for a
protocol that cannot know whether a successful response is JSON or SSE until
the response head arrives. `HttpStreamingResponse` exposes status,
content-type and bounded header lookup, while retaining one pull-driven
`HttpBodyStream` owner.

The reqwest implementation returns after the response head, then reads at most
one network chunk per body poll. The caller's token owns both header acquisition
and every body read; cancellation yields one terminal `Cancelled`, cumulative
bytes enforce the original request cap, and dropping the body drops the
reqwest response. No drain task or detached handle exists. Debug renders header
names and response shape, never values or body bytes.

The trait default adapts an existing buffered transport into one body chunk so
external/test providers remain source-compatible without an empty-body lie.
This fallback is compatibility, not streaming evidence; the built-in reqwest
provider has real response-head/body separation and backpressure tests.

## WebSocket preference and fallback

`StreamTransport` expresses three explicit plans: HTTP-only, prefer WebSocket
with an equivalent HTTP+SSE fallback, or require a bidirectional WebSocket.
Fallback happens only before a connection opens and records a closed reason in
both the session outcome and shared counters. A required bidirectional channel
never silently degrades to one-way HTTP.

Reconnect covers only the handshake. `WebSocketConnector` receives an opaque
`WebSocketConnectRequest` containing URL and headers but structurally no
application frames. After one handshake succeeds, the transport sends at most
16 bounded opening frames exactly once. A setup-send or established-stream
failure is terminal: reconnecting or falling back could duplicate provider
work. Connector cancellation is terminal rather than retryable.

No configured inference protocol currently offers a WebSocket alternative for
the request shape heycode sends. OpenAI Realtime and Gemini bidi are separate
protocols, while Bedrock bidi is HTTP/2. Accordingly this crate supplies the
connector seam, deterministic real-socket tests, fallback behavior and exact
reconnect metrics, but intentionally chooses no WebSocket library and claims
no provider WebSocket reachability.

## Focused verification

```sh
cargo fmt -p heycode-http -- --check
cargo clippy -p heycode-http --all-targets -- -D warnings
cargo test -p heycode-http --no-fail-fast
```
