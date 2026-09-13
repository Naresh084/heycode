# heycode-tools

`heycode-tools` owns the model-callable Tool contract, insertion-ordered registry
and the single guarded execution pipeline.

Ordinary tools return plain JSON. `Tool::run_output` adds an optional bounded
`PendingRichToolResult` for external protocols without changing every built-in
implementation. Rich blocks retain ordered text, media, resource links,
embedded resources, annotations, structured JSON and honest schema-check
evidence. Raw bytes never enter the JSON/UI value or Debug output. The Agent is
the only durable Consumer: it admits media through the attachment service in
the ordered commit cursor before publishing a session result.

`Tool::effect` states whether a call only observes. The batch scheduler
overlaps `ToolEffect::ReadOnly` calls and treats everything else as a barrier,
so the claim that two concurrent invocations cannot corrupt each other now
lives with the tool that can make it instead of in a name list beside the
scheduler — a list that could never speak for a tool this crate has not heard
of. The default is `Mutates`: a tool that says nothing serializes.

Plugin `lsp-tools` contributes `lsp_servers`, `lsp_definition`,
`lsp_references` and `lsp_diagnostics` over the composed replaceable LSP
service. Paths resolve through the filesystem capability before the LSP
Provider independently confines them to its registered workspace. Results up
to 64 KiB stay structured JSON; larger results are retained as complete bytes
under the host-owned `lsp-tools` scope and return the retained-output service's
whole bounded preview unchanged. Server-authored output is classified as
`UntrustedContentSource::Lsp`, distinct from Web and MCP. A default registry
with no trusted definitions returns an empty server list and starts no child.

Plugin `interactive-tools` contributes `browser`, `artifact`, `notebook_read`,
`notebook_edit`, `computer`, and `transcribe_audio` on the same guarded path.
Browser sessions use an optional trusted Playwright installation and route HTTP
through the live web policy. Artifact previews and notebook edits use filesystem
capabilities and content revisions. The fixed macOS helper supplies app-targeted
capture, accessibility and input; local speech accepts an explicitly selected
PCM WAV through an optional installed recognizer. Neither captures microphone
input. See [setup, limits and evidence](../../docs/plans/audit-interactive-integrations-implementation.md).

Focused verification:

```sh
cargo clippy -p heycode-tools --all-targets -- -D warnings
cargo test -p heycode-tools
```


`Tool::supports_background` is an explicit cancellation contract for dispatch
through native execution jobs. `Tool::rebind_workspace` replaces local file or
process authority when a host creates a child workspace. Shell and terminal
launch tools use the rebound subprocess service; terminal management retains
registry identity and resolves its owner from the host's caller context.
All six terminal tools are registered together by the default execution plugin.
