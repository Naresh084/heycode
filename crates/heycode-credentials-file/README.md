# heycode-credentials-file

The built-in persistent credential provider stores schema-v1 TOML at
`~/.heycode/credentials.toml` or the explicit `HEYCODE_HOME/credentials.toml`.
It follows read-only environment and explicitly configured command providers.
New keys, replacements and deletions all use this file; no OS secret store is
opened. Unix directory/file modes are `0700`/`0600`, replacements are atomic,
and symlinks and malformed stores fail before mutation.

The historical `credentials` text file migrates only after both the new TOML
store and byte-exact `credentials.legacy.bak` are durable. OS keychain entries
are never imported. A key saved only in an old keychain must be entered again.

Verification: `cargo test -p heycode-credentials-file`; the CLI integration suite
covers default-home persistence, isolation, setup and plugin lifecycle.
