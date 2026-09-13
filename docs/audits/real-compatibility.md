# Phase 2 real LSP and MCP compatibility evidence

Date: 2026-09-11 (Australia/Melbourne)

## Evidence boundary

This is an opt-in local compatibility canary, not a synthetic protocol fixture and not broad provider certification. It ran one installed production language server and one installed third-party open-source MCP package through dshx's public services. Every source/workspace input was created under a fresh temporary directory. No model, credential, paid API, network request, Xcode build, device, simulator, UI automation, or real user project was invoked.

The installed programs still read their own binaries, packages, dynamic libraries, and platform toolchain. XcodeBuildMCP received both a synthetic cwd and synthetic child home; all of the state it created was under that temporary home. Raw excerpts below replace the temporary path with `<TEMP>` and omit process ids and memory counters.

Primary references:

- [clangd installation and generic-client guidance](https://clangd.llvm.org/installation)
- [XcodeBuildMCP v2.7.0 release and stdio configuration](https://github.com/getsentry/XcodeBuildMCP/releases/tag/v2.7.0)

## Production LSP: Apple clangd 17

Observed executable and version:

```text
/usr/bin/clangd
Apple clangd version 17.0.0 (clang-1700.3.19.1)
Features: mac+xpc
Platform: arm64-apple-darwin25.6.0
```

The test creates one `main.cpp` plus `compile_commands.json` under `<TEMP>`, composes `SandboxService`, the local filesystem/subprocess providers, the LSP registry, and the exact stdio server definition, then invokes only `LspService` methods. The permanent canary is `installed_clangd_serves_navigation_through_the_public_service` in `crates/heycode-exec/tests/it/lsp_service.rs` and is ignored unless the absolute executable is supplied.

Reproduction:

```sh
DSHX_REAL_LSP_CLANGD=/usr/bin/clangd \
  cargo test -p dshx-exec --test main \
  installed_clangd_serves_navigation_through_the_public_service \
  -- --ignored --nocapture
```

Sanitized result:

```text
REAL_LSP_COMPAT version="Apple clangd version 17.0.0 (clang-1700.3.19.1)"
definition=1 references=2 symbols=6 workspace_symbols=1 incoming=1 diagnostics=0
outgoing=protocol_unsupported recovery=ok precancel=cancelled root=<TEMP>
```

Relevant raw protocol evidence, reduced to the fields that determined compatibility:

```json
{"serverInfo":{"name":"clangd","version":"Apple clangd version 17.0.0 (clang-1700.3.19.1) mac+xpc arm64-apple-darwin25.6.0"},"capabilities":{"definitionProvider":true,"referencesProvider":true,"hoverProvider":true,"implementationProvider":true,"documentSymbolProvider":true,"workspaceSymbolProvider":true,"callHierarchyProvider":true}}
{"method":"textDocument/didOpen","params":{"textDocument":{"uri":"file://<TEMP>/main.cpp","languageId":"cpp","version":1,"text":"<SYNTHETIC SOURCE>"}}}
{"id":2,"method":"textDocument/definition","params":{"textDocument":{"uri":"file://<TEMP>/main.cpp"},"position":{"line":14,"character":11}}}
{"id":2,"result":[{"uri":"file://<TEMP>/main.cpp","range":{"start":{"line":8,"character":4},"end":{"line":8,"character":9}}}]}
{"method":"textDocument/publishDiagnostics","params":{"uri":"file://<TEMP>/main.cpp","diagnostics":[]}}
{"id":3,"method":"callHierarchy/outgoingCalls","params":{"item":"<BOUNDED PREPARED ITEM>"}}
{"id":3,"error":{"code":-32601,"message":"method not found"}}
```

The first run found that dshx sent navigation requests before opening the document; clangd rejected them as a non-added document. All document-scoped requests now read the bounded file through `FileSystemService` and synchronize it with `didOpen`/`didChange` while holding the connection lane. The real response also used valid empty optional `containerName` strings, now normalized to absent instead of being rejected. A controlled fixture covers that shape.

Apple clangd does not advertise `diagnosticProvider`; it publishes classic `textDocument/publishDiagnostics`. dshx now selects pull or push diagnostics from initialized capabilities, bounds cached push rows, and waits at most two seconds for a pushed result. A separate exact fixture covers the push path.

This Apple build advertises general call-hierarchy support but returns JSON-RPC `-32601` for `callHierarchy/outgoingCalls`. dshx deliberately maps that server body to the fixed `Protocol` class, retires the failed session, and successfully reconnects for the next hover. Incoming calls work. The canary therefore does not claim that outgoing calls are supported by this server version.

## Third-party MCP: XcodeBuildMCP 2.7.0

Observed installed package:

```text
name=xcodebuildmcp
version=2.7.0
license=MIT
repository=https://github.com/getsentry/XcodeBuildMCP.git
node=v25.2.1
```

The test starts the already installed package using absolute Node and CLI paths, with `<TEMP>` as cwd, `XCODEBUILDMCP_CWD`, `HOME`, and `TMPDIR`. Sentry, session-default hydration, and Xcode auto-sync are disabled. It calls only dshx's model-facing `wait_for_mcp_servers`, `list_mcp_resources`, and `read_mcp_resource` tools. It reads the inert `xcodebuildmcp://session-status` resource; it never calls a server tool.

Reproduction on this host:

```sh
DSHX_REAL_MCP_NODE=/opt/homebrew/bin/node \
DSHX_REAL_MCP_XCODEBUILDMCP=/Users/naresh/.npm/_npx/99336612077b7094/node_modules/xcodebuildmcp/build/cli.js \
  cargo test -p dshx-mcp --test main \
  installed_xcodebuildmcp_resources_cross_the_public_model_tools \
  -- --ignored --nocapture
```

Sanitized dshx result:

```text
REAL_MCP_COMPAT package=xcodebuildmcp version="2.7.0" resources=4
ready=all_ready read=session-status invalid_uri=body_free precancel=cancelled
cwd=<TEMP> home=<TEMP>
```

Sanitized raw server protocol:

```json
{"id":1,"result":{"protocolVersion":"2025-11-25","capabilities":{"tools":{"listChanged":true},"resources":{"subscribe":true,"listChanged":true},"logging":{}},"serverInfo":{"name":"xcodebuildmcp","version":"2.7.0"}}}
{"id":2,"result":{"resources":[{"uri":"xcodebuildmcp://devices","name":"devices","mimeType":"text/plain"},{"uri":"xcodebuildmcp://doctor","name":"doctor","mimeType":"text/plain"},{"uri":"xcodebuildmcp://session-status","name":"session-status","mimeType":"application/json"},{"uri":"xcodebuildmcp://simulators","name":"simulators","mimeType":"text/plain"}]}}
{"id":3,"result":{"contents":[{"uri":"xcodebuildmcp://session-status","mimeType":"application/json","text":"<EMPTY SYNTHETIC SESSION STATUS>"}]}}
```

The real server exposed a contract hole: the model tool described exact listed-URI access, but a not-listed URI still reached the server and returned JSON-RPC `-32602`. `read_mcp_resource` now validates the 2 KiB URI boundary and requires exact membership in the current committed generation before transport dispatch. The permanent fixture asserts the fixed, body-free `MCP resource is not in the committed server listing` error.

The canary verifies ready settlement, a four-resource listing, one JSON resource read, not-listed refusal before dispatch, pre-cancellation, and effect-owned teardown. Mid-request cancellation, binary/truncation limits, pagination, failed readiness, and disappearing-server settlement remain covered by controlled fixtures; XcodeBuildMCP's selected resource is too fast and too small to exercise those paths honestly.

## Remaining compatibility boundary

- This is one Apple clangd build, not rust-analyzer, pyright, TypeScript, or a language-server matrix.
- Apple clangd outgoing-call hierarchy remains server-unsupported despite its broad capability flag; dshx returns a fixed protocol error and recovers.
- This is one installed MCP package and one inert resource. No MCP auth, reconnect, subscription, hostile payload, large body, or third-party network behavior was exercised here.
- Both tests are opt-in so ordinary CI does not depend on machine binaries, npm caches, Apple tooling, or mutable external versions.
- Model-facing and protocol evidence is complete for these canaries; paired Claude/dshx TUI states remain a separate visual and interaction gate.
