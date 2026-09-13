# PL10 guest fixtures

This nested, non-workspace crate is the source for
`code_plugin_component.b64` and `code_plugin_scoped_component.b64`. The default
feature builds the pure `code-plugin` world; `--features scoped` builds
`code-plugin-filesystem-network` and retains filesystem/socket imports so the
host test exercises real WASIp3 linking.

The WASI dependency WIT is copied from `wasmtime-wasi` 48.0.1 and advances its
package references from 0.3.0 to the compatible, tracker-pinned 0.3.1 release.
Regeneration requires the `wasm32-unknown-unknown` target and an official
`wasm-tools` release capable of Component Model async:

```sh
cargo build --manifest-path Cargo.toml --target wasm32-unknown-unknown --release
wasm-tools component new target/wasm32-unknown-unknown/release/heycode_wasi_guest_fixture.wasm -o plugin.component.wasm
```

Use an isolated target directory when running both feature variants. The
base64 files are test transport only; production loads exact binary bytes from
the verified PL02 package cache.
