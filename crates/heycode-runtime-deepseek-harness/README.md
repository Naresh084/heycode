# heycode-runtime-deepseek-harness

`heycode-runtime-deepseek-harness` is the R12 delegated runtime Provider for a
local DeepSeek Harness SDK server. It registers runtime id `deepseek-harness`
and starts one full Harness process per heycode runtime session through the common
`heycode-exec` raw-interactive subprocess service. The child owns its Cordis
composition, model loop, tools, persistence and credentials; heycode supplies only
an exact explicit environment and never opens a Harness credential file or
copies a token.

Registration starts no process. A missing optional `dsh-jsonrpc-agent`
installation leaves the descriptor visible with account state Unavailable and
requires recompose after installation. Denied, malformed or otherwise broken
executable resolution still fails plugin activation.

The resolved launcher is SHA-256-bound at plugin activation and rechecked before
every session. Interpreter-based deployments additionally register reviewed
script/config paths through `with_artifacts`; those files are bounded, hashed at
activation and rechecked at the same pre-spawn boundary. This is local
replacement detection, not publisher or reproducible-build authentication.

## Pinned wire

The bridge pins SDK server identity `deepseek-harness-sdk-runtime`, protocol
version `0.0.1`, and the repository-known session-event union from DeepSeek
Harness commit `528c682e061696f5a160f363f236ecbf53cbd006`. The reviewed wire has
exactly three client requests:

- `initialize` binds absolute cwd, provider, model and an optional safe-integer
  output cap;
- `session/prompt` returns a durable user-message enqueue id;
- `shutdown` begins quiescent process teardown.

Server notifications are the closed `session.event`, `session.status`,
`subagent.started` and `subagent.finished` set. Frames are strict UTF-8 NDJSON,
bounded at 4 MiB with independent JSON depth/node limits. Responses correlate
by exact request id. Root and child session ids and event sequences are tracked
independently; a known event is explicitly handled or ignored, an unknown
required event is protocol drift, and only an upstream `ignorable:true` event
may be skipped.

One prompt owns the documented receipt-to-running-to-idle interval. Root turn,
assistant text/reasoning, complete tool calls/results, disjoint cache-aware
usage and settlement are validated and emitted through the R02 normalizer.
Provider error bodies and Harness failure strings never enter runtime errors or
Debug output.

## Truthful capabilities and lifecycle

SDK v0.0.1 has no model-catalog, resume, fork, steer, queued follow-up,
prompt-cancel, permission/question callback or compaction method, so every one
of those descriptor fields is `Unsupported`. A caller may select the exact
process-wide model used by `initialize`; this is route input, not model-discovery
evidence. Ephemeral start is refused because the child composition may persist
its own sessions.

Because the wire has no prompt cancel, cancelling an admitted `send` reaps the
whole child process and closes that runtime session; it never pretends the
session remains reusable. Ordinary `close` requests `shutdown`, then settles
the raw process tree, and is idempotent. Plugin generation shutdown cancels all
session lifecycle tokens before the registry row is withdrawn.

Deterministic tests drive a real contained local process through initialize,
one receipt/status/turn/tool/usage/final cycle, shutdown, version drift, sequence
drift and mid-prompt cancellation. A source checkout or fixture is protocol
evidence; a separately run local Harness composition is required for live R12
acceptance.

The explicitly gated local canary was reassessed on 2026-08-31 against checkout
`528c682e061696f5a160f363f236ecbf53cbd006`. It is not a reproducible runnable
artifact on this host: `packages/examples/jsonrpc-demo/lib/bin.js` and
`node_modules/.bin/tsx` are absent; the repository requires `pnpm@11.7.0` while
the installed pnpm is `9.15.0`; and the working lockfile is already modified in
a way that removes pinned overrides/patch metadata. No install or build was
attempted and no real Harness turn or R12 live acceptance is claimed.

The revised canary accepts only an absolute Node executable, a built reviewed
server bundle and a reviewed Cordis file, each accompanied by its expected
SHA-256. The process Provider then binds Node while the runtime binds the two
additional artifacts. The turn uses only a loopback mock model plus a fixed
non-secret key, and provider content is neither printed nor persisted. Until a
clean `pnpm@11.7.0` frozen build is reviewed and those exact artifacts exist,
the missing runnable SDK artifact is the blocker.

Upstream protocol references:

- <https://github.com/deepseek-ai/deepseek-harness/blob/528c682e061696f5a160f363f236ecbf53cbd006/packages/sdk/protocol/README.md>
- <https://github.com/deepseek-ai/deepseek-harness/blob/528c682e061696f5a160f363f236ecbf53cbd006/packages/sdk/server/README.md>

## Verification

```sh
cargo fmt -p heycode-runtime-deepseek-harness -- --check
cargo test -p heycode-runtime-deepseek-harness
cargo clippy -p heycode-runtime-deepseek-harness --all-targets -- -D warnings
```
