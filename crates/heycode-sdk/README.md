# heycode-sdk

Low-dependency Rust client and authoritative wire vocabulary for heycode
app-server protocol v1.

`AppClient<T>` accepts an `Arc<T: AppTransport>`. A transport exchanges one raw
JSON request, streams ordered raw JSON notifications through a bounded channel,
honors the caller `CancellationToken`, returns one raw response, and settles all
owned resources before returning. Raw frames must never be logged because an
authorization answer can contain a transient credential.

The client owns request ids, 4-MiB wire bounds, JSON-RPC correlation, closed
event decoding, session identity checks, and contiguous notification validation
within each operation. `start` opens the new session selected by the host;
`resume(expected_id)` refuses a different host-selected session. A turn streams
typed notifications through a caller-owned channel. Concurrent cancellation
uses a cloned client, and the caller awaits both futures.

The additive optional `usage.context` object carries exact latest-request
occupancy and the provider-reported context window when a delegated runtime
supplies both. Older v1 producers and consumers continue to use the required
turn-usage totals without this metadata.

Session open accepts `AppRuntimeConfiguration`: an optional exact system
prompt, an optional ordered tool catalog (where `Some([])` is distinct from
omission), model and reasoning effort. Session info/runtime rows expose the
effective configuration plus tri-state support evidence, and `configure`
performs a between-turn update. `runtime_models` returns provider-native model
ids with their exact advertised effort choices and default. Older v1 hosts that
omit the added fields decode to an empty configuration with Unknown evidence
rather than inventing support.

ATT04 extends the closed event union with metadata-only `assistant_audio` and
the shared attachment record with optional validated audio facts. Encoded
bytes have no Rust field, and unknown attachment members (including hostile
`data` or `bytes`) fail deserialization. The feature remains hidden: no
initialize capability or ordinary provider/model row advertises it.

The optional control generation also exposes closed provider/model/runtime
catalog and selection methods plus `workspace/select`. Runtime rows preserve
native/delegated ownership and tri-state capability evidence. Workspace input
is an absolute path request only: the host remains responsible for canonical
allowed-root validation and returns the effective runtime/workspace after the
selection, so a client never treats its own requested path as committed state.

The crate depends only on `heycode-core` plus serialization/async utilities. The
host crate implements `AppTransport` for its local `AppServer` and re-exports
the same types; it does not maintain a second Rust wire schema. The shared
fixture at `sdks/fixtures/app-server-v1.json` is parsed by both Rust and
TypeScript tests, and both assert the same exact ordered sequence of decoded
event type names, so renaming or reordering a closed event reddens the Rust
twin as well as the npm package.

The TypeScript client is consumed by the installable extension under
`editors/vscode`. That extension uses these same initialize, host-selected
start/resume, streamed turn, permission/question response, cancel and close
methods; it does not maintain a second app-server schema. The SDK also preserves
the optional runtime/workspace capability flags and typed discovery/selection
responses added after the original v1 initialize shape, defaulting absent flags
to false.

Question events preserve the optional `header` and the
`choice_descriptions` array aligned with `choices`; older v1 events that omit
both still decode. `respond_question` submits selected or custom text, while
`cancel_question` sends an explicit cancellation rather than a sentinel answer.

The SDK remains transport-neutral. The child-process NDJSON framing used by the
extension is implemented by the app-server host and extension transport rather
than added to this crate, so embedders do not inherit Node/process policy.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-sdk --all-targets -- -D warnings
cargo test -p heycode-sdk
```
