# heycode-trust

Canonical workspace identity, trust decisions and project-authority gates. The
service is opened before automatic project config, profiles, settings, skills,
MCP or executable contributions are discovered.

## Decision contract

- `Unknown` blocks project executable/settings authority and requires the
  highest-priority interactive modal; headless/ACP callers must supply an
  explicit session-only choice.
- `Restricted` permits only the explicit non-executable content policy.
- `Trusted` permits project-scoped contributions.
- Session choices never become durable. Persistent trust and reset use
  revision CAS; stale writers publish nothing.
- A `WorkspaceTrustPrompt` binds typed UI state and actions to the exact live
  service/revision. A successful action returns a typed recomposition outcome;
  the pre-trust world must be torn down before project inputs are loaded.
- Composition verifies that the service's canonical workspace identity matches
  the requested cwd. Relative `$HEYCODE_HOME` is rejected by the CLI/config
  boundary rather than becoming repository-owned authority.

## Store boundary

The Unix file store is capability-rooted and descriptor-relative. Directory,
store, lock and staging files enforce ownership, mode, regular-file type,
single-link identity and pre/open/post identity checks. One cross-process file
lock covers generation read, CAS mutation, file sync, atomic rename and parent
directory sync. Unsafe modes are repaired only after ownership proof;
symlinked parents/files and multiply-linked stores fail closed.

The memory backend is portable for tests/embedders. Persistent file trust is
currently `UnsupportedSecurity` outside Unix. An audited Windows implementation
still needs handle-relative protected-directory creation with owner DACL and
lock semantics; path-based create-then-tighten is not accepted. Windows release
gates must therefore remain open.

## Verification

```sh
cargo fmt -p heycode-trust -- --check
cargo test -p heycode-trust
cargo clippy -p heycode-trust --all-targets -- -D warnings
```
