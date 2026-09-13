# heycode-runtime-opencode

`heycode-runtime-opencode` is the R10 production Provider for OpenCode's ACP
subprocess. It registers delegated runtime id `opencode` into the shared
effect-owned runtime registry and launches every connection through the
composed `heycode-exec` subprocess service. OpenCode remains an external agent
loop; this crate never implements an inference Provider and never reads an
OpenCode credential file or token.

The default child environment is a name-sorted allowlist of home/XDG, locale,
TLS-certificate and temporary-directory variables. API-key/token names, proxy
URLs, interpreter search paths, plugin controls and arbitrary ambient values
are excluded. Embedders may replace that complete environment explicitly after
reviewing any launcher and PATH authority.

The current boundary is pinned to OpenCode `1.18.21`. Every catalog probe or
session connection first runs the exact resolved executable with `--version`
under the same complete explicit environment, accepts only that version as one
bounded line, and then launches exact argv `opencode acp`. The resolved
executable is SHA-256-bound at plugin activation and rechecked before and after
the version probe, so a replaced binary cannot inherit the reviewed version.
This is local replacement detection, not publisher authentication. The ACP
layer also requires initialized `agentInfo` to be exact `OpenCode/1.18.21` and
owns strict protocol-version negotiation, 4-MiB UTF-8 NDJSON frames, response
and permission correlation, session-scoped provider/model options, R02 event
normalization, caller cancellation and idempotent quiescent close.
The process adapter preserves raw byte framing and process-tree ownership
through `spawn_interactive_raw`.

Plugin registration starts no process. It resolves the configured executable
through the subprocess Provider; a genuinely missing optional installation
registers the same descriptor with account state Unavailable, while denial,
invalid executable state and other resolution failures still fail activation.
Installing OpenCode requires recompose. Runtime-generation shutdown cancels all
connection lifecycle tokens before the registry contribution is withdrawn;
each session's async `close` remains the quiescent reap boundary.

The descriptor claims only the operations implemented by the reviewed ACP
profile: models, resume and permission callbacks are Supported; fork, steer,
follow-up, questions and compaction are Unsupported. The deterministic fixture
proves the executable/version/process/catalog path and effect withdrawal. Its
selection fixture uses the current official
`opencode-go/glm-5.3-flash` identity, never the retired anonymous Ox preview
alias, but fixture naming is not a live claim. R10 still requires that exact
route to complete a trustworthy authenticated turn.

On 2026-08-31 the credential-blind canary passed against installed OpenCode
`1.18.21` with fresh isolated HOME/XDG roots: exact executable/version binding,
initialize, session-scoped model catalog and quiescent process close completed
without exposing or reading a stored credential. A second check against the
installed user's catalog found no `opencode-go/glm-5.3-flash` row, so no model
turn was sent. The content-withheld `HEYCODE_OPENCODE_GLM_E2E=1` gate will run only
after that exact id is visible. It isolates global/project configuration,
writes an all-tools-denied temporary config, passes only OpenCode's own account
store authority to the child, and fails if any tool/permission event appears.
The heycode test never opens a credential file or prints an environment value,
provider body, prompt response or event payload.

The same installed credential-blind catalog now exposes exact official Zen row
`opencode/mimo-v2.5-free`. A separate
`HEYCODE_OPENCODE_FREE_E2E=1` canary uses fully isolated HOME/XDG roots,
`OPENCODE_DISABLE_PROJECT_CONFIG=1`, an inline all-tools-denied config and no
credential variables. It passed exact catalog selection, ACP session start,
nonempty final, `Stop`, no tool/permission activity and quiescent close; final
content was inspected only for non-emptiness and never printed or persisted.
That is live evidence for R10's session/event/model/provider bridge, but not a
substitute claim for GLM-5.3-Flash or authenticated OpenCode account health.

OpenCode documents full selections as `provider/model-id` and lists
`opencode-go/glm-5.3-flash` as the current GLM-5.3-Flash route. Z.ai documents
Ox Alpha as its pre-release anonymous identity, so it is not a supported canary
name: [OpenCode Go models](https://opencode.ai/docs/go/) and
[Z.ai GLM-5.3-Flash release](https://z.ai/blog/glm-5.3-flash).
OpenCode's official Zen catalog separately documents MiMo-V2.5 Free as model id
`mimo-v2.5-free` under the `opencode` provider:
[OpenCode Zen models](https://opencode.ai/docs/zen/).

Official transport reference: <https://opencode.ai/docs/acp/>.

## Verification

```sh
cargo fmt -p heycode-runtime-opencode -- --check
cargo test -p heycode-runtime-opencode
cargo clippy -p heycode-runtime-opencode --all-targets -- -D warnings

HEYCODE_OPENCODE_FREE_E2E=1 \
HEYCODE_OPENCODE_EXECUTABLE=/absolute/path/to/opencode \
  cargo test -p heycode-runtime-opencode --test installed_canary \
  installed_opencode_official_free_turn_is_credential_blind_and_content_withheld \
  -- --exact
```

## Connection setup update — 2026-09-05

Plugin-owned discovery uses an effect-owned temporary workspace; session start retains the caller’s requested working directory.
