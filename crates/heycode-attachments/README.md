# heycode-attachments

ATT01–ATT03 provider-neutral attachment service and audited local Provider.

`attachments-local` injects the durable session and publishes service
`attachments`. Admission bounds bytes, validates a portable display basename,
sniffs canonical MIME from content and reads PNG/JPEG/GIF/WebP dimensions
without decoding full pixels. Raster dimensions are capped at 32,768 per side
and 100 million pixels. The caller's MIME claim must match; PDF, UTF-8 text and
opaque binary remain distinct.

Exact bytes are SHA-256 addressed under an owner-only schema-v1 root. On Unix,
process and `flock` ownership serialize writers; a `0600` fsynced temporary is
published with hard-link no-clobber semantics, and directories are `0700`.
Reads revalidate file identity/mode/link count/length, content hash, MIME and
dimensions. Non-Unix local storage fails `UnsupportedSecurity` until an audited
owner-security backend exists.

The byte commit precedes the v2 `attachment/added` session append. Optional
validated HTTP provenance (final URL/title/retrieval time/raw truncation/page
count) is stored beside the content address, not in a live-only web result. Session bus
listeners therefore see readable content; cancellation or append failure never
publishes phantom metadata (an unreachable immutable object may remain for a
future garbage collector).

MCP12 reuses the same commit boundary for rich tool-result media and embedded
blobs. The Agent's ordered tool-result cursor admits bytes first, then appends a
`tool/rich-result` containing only validated immutable metadata references. A
failed/cancelled admission becomes a tool error; raw media never enters the
session result JSON, UI value or Debug output.

`admit_image_path` accepts an explicit absolute regular-file selection, refuses
symlinks and rechecks file identity/length/timestamps around the bounded read.
It admits only PNG/JPEG/GIF/WebP before any event. Optional plugin
`agent-attachments` effect-binds this store to Agent and owns `/attach`; TUI
staging clears only after the paired `user/attachments` + `user/message` commit.
Headless `--image` uses the same path boundary. Agent re-verifies bytes and
selected-model `image_input=Supported` before association, then projects exact
OpenAI Responses/Chat or Anthropic Messages blocks. X03 still owns delegated
and ACP media events.

`admit_document_path` applies the same path/identity boundary and admits only
content-sniffed PDF or HTML before publication. Optional `agent-documents`
binds the composed `document-extractor`, owns `/document`, and headless
`--document` preserves ordering with image flags. Exact document capability
selects native PDF bytes; otherwise the bounded extractor's UTF-8 output is
admitted as a distinct immutable `text/plain` object. The adjacent session
selection records source→selected and `native|extracted`, so later projection
never re-decides from current catalog state.

ATT04 adds hidden PCM-WAV groundwork, not a public `/audio` command. RIFF/WAVE,
chunk boundaries, exact container length, one PCM `fmt`, one nonempty aligned
`data` chunk, byte rate/block alignment, duration, 8–384 kHz rate, 1–8 channels
and 8/16/24/32-bit depth are checked before publication. `admit_audio_path`
uses the same no-symlink/race-checked boundary; ordinary admission also sniffs
valid WAV bytes so provider and MCP output can reuse it. Reads reparse and
compare every audio fact with the durable record.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-core -p heycode-session -p heycode-attachments -p heycode-llm -p heycode-agent -p heycode-tui -p heycode-config -p heycode-cli --all-targets -- -D warnings
cargo test -p heycode-core -p heycode-session -p heycode-attachments -p heycode-llm -p heycode-agent -p heycode-tui -p heycode-config -p heycode-cli --no-fail-fast
cargo check -p heycode-attachments --target x86_64-unknown-linux-gnu
cargo check -p heycode-attachments --target x86_64-pc-windows-gnu
```
