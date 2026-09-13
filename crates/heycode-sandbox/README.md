# heycode-sandbox

`heycode-sandbox` supplies argv-rewriting confinement providers behind the
provider-neutral `SandboxService` contract in `heycode-exec`.

- macOS uses Seatbelt profiles.
- Linux selects Landlock when the running ABI is sufficient, otherwise bwrap.
- unsupported platforms report Unsupported rather than pretending confinement.

Workspace-write paths entering a Seatbelt profile are encoded as string
literals; quotes and backslashes remain data, while control-bearing roots fail
closed. Every backend refuses a non-UTF-8 root before representing it in a
profile/JSON/argv string, so a lossy replacement character cannot grant a
different on-disk name. Read-only does not serialize the root and therefore
does not reject an otherwise unused path. The native macOS matrix covers
traversal, symlink roots/escapes,
descendant inheritance, mutation classes, read-only scope and crafted profile
injection. Seatbelt is path-authorized, not inode-authorized: a pre-existing
hard link inside the workspace can still alias an outside inode, and the matrix
keeps that limitation visible. QSEC03 therefore remains open for a stronger
answer plus native Linux/Windows evidence.

Focused verification:

```sh
cargo clippy -p heycode-sandbox --all-targets -- -D warnings
cargo test -p heycode-sandbox
cargo test -p heycode-sandbox --test main -- --nocapture
```
