# heycode-credentials

`heycode-credentials` owns non-secret references, safe descriptors, explicit
zeroizing secret values, provider precedence and operation-time resolution.
Plugin `credentials` publishes the effect-owned registry and a Settings
namespace containing references only.

Providers inspect configuration without resolving. Resolution walks the exact
query in precedence order; the first configured provider is authoritative and
read-only shadowing blocks lower writes. Validation records are TTL-bounded and
privately fingerprinted, so rotation clears stale validity without serializing
or displaying the fingerprint.

Provider failure strings are untrusted boundary data. The registry retains the
safe provider id but discards the free-form body before constructing
`CredentialsError` or `CredentialResolutionError`; a provider that accidentally
returns its secret cannot route it into logs. `CredentialSecret` is neither
serializable nor displayable and exposes bytes only through the explicit
operation method.

QSEC02 joins these lower contracts to a product canary: a deterministic
environment value reaches a real strict adapter's Authorization header while
remaining absent from request bodies/prompts, UI/debug, physical session JSONL,
request projections, process diagnostics and redacted support export.

Focused verification:

```sh
cargo clippy -p heycode-credentials --all-targets -- -D warnings
cargo test -p heycode-credentials
```

Built-in persistence uses only the heycode home credential file; no OS keychain provider ships. The historical `Keychain` source enum is metadata compatibility, not an available store.
