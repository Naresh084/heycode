# Interactive integrations implementation

Workstream: interactive-integrations. Base snapshot: `174740a54b664ae2df2150ea2373f0718ea4201b`. Branch: `codex/interactive-integrations-20260908`. No changes in the root checkout, no push or deployment.

## Scope and audit relation

The 37-finding source audit's A35 describes runtime capability parity. This work adds provider-independent interactive capabilities on the ordinary native tool path; it does not claim to close all external-runtime parity in A35. The expanded integration request names browser gap L11. Notebook editing, generated-file previews, native computer interaction and optional local speech recognition are the companion requested capabilities. No separate general JavaScript runner or remote automation was added.

## Shipping composition and public APIs

Default CLI plugin `interactive-tools` injects the existing tools/filesystem/subprocess/web services and registers six ordinary tools: `browser`, `artifact`, `notebook_read`, `notebook_edit`, `computer`, `transcribe_audio`. They use the same pre-tool approval/guard pipeline as the existing tools. Every tool is conservatively effectful; external browser text carries the existing Web untrusted-content boundary. Plugin retirement unregisters tools and cancels their lifecycle. Held tool handles reject further calls. [Workspace rebinding follow-up](audit-interactive-integrations-qa-followup.md) makes child tool services and browser/artifact state independent, with explicit failure when a child process provider is unavailable.

- `heycode_tools::interactive::interactive_tools_plugin(Option<BrowserConfig>)` composes browser/artifact/notebook/native controls with no STT command.
- `interactive_tools_plugin_with_speech(Option<BrowserConfig>, Option<SpeechCommandConfig>)` additionally binds a trusted local recognizer.
- `BrowserConfig { node, module, executable }` holds absolute host-owned paths. `from_environment()` reads the three explicit `HEYCODE_BROWSER_*` variables below. No model-supplied program, module, profile, script or launch flags are accepted. Missing installation has an actionable `browser(action="status")` result; it does not prevent notebook/artifact use or start a child at composition.
- `SpeechCommandConfig::new(program, args)` and `from_environment()` admit exact literal argv without a shell. The environment format is a JSON array, with an absolute executable first.
- `ReadFileSpec::new_binary(path, max_bytes)` opts a media consumer into exact byte reads through the same filesystem capability and race checks. Ordinary `new` text reads still reject binary input. No filesystem backend signature changed.
- `heycode_attachments::validate_pcm_wav(bytes)` exposes the existing pure PCM parser without writing an attachment/session event.
- `WebRegistry::browser_request(BrowserHttpRequest, Option<&BrowserLocalOrigin>, CancellationToken)` is the narrow raw HTTP transport. `BrowserHttpRequest` contains URL/method/header pairs/body; `BrowserHttpResponse` contains status/header pairs/complete bytes. Both redact bodies from Debug. `BrowserLocalOrigin::new` validates one explicit numeric loopback HTTP application origin.

Neighboring changes: CLI default plugin/factory wiring; binary-read opt-in in heycode-exec; pure WAV validation export in heycode-attachments; raw policy-mediated HTTP in heycode-web. Root integration must reconcile default plugin/tool inventory expectations with all parallel contributions.

## Browser operation and policy

Example local app workflow:

```json
{"action":"open","url":"http://127.0.0.1:3000/","allow_local":true}
{"action":"type","session":1,"element":"e1-0","text":"Ada"}
{"action":"click","session":1,"element":"e2-1"}
{"action":"screenshot","session":1,"path":"output.png"}
{"action":"close","session":1}
```

Use the actual session and fresh element refs returned by preceding calls. `open`, `navigate`, `inspect`, `click` and `type` return URL, title, text, accessibility snapshot and bounded DOM element refs. One isolated session is owned per composed world; a second open requires closing the first. Errors/cancellation retire that session, so reopen with a new ID. Every operation has a 45-second deadline; teardown is bounded and owns the whole subprocess tree. Normal close gives Playwright a bounded cleanup opportunity before hard process settlement. Context shutdown and dropped owners retain the subprocess provider's kill-on-drop guarantee. Running sessions do not survive process shutdown.

Public URLs work without `allow_local`. Chromium is launched with a nonpersistent isolated context and a dead-end proxy. Every HTTP request is intercepted and fulfilled over the Rust broker, which reuses the existing public address checks, mixed/private DNS refusal, pinned socket client, no ambient proxy, restricted ports and live fetch-domain policy. Redirects return to Chromium for independent re-admission, with five hops maximum. Browser cookies belong only to the isolated context; no host credentials or personal browser profile are loaded. The local exception is exactly one explicitly opened numeric loopback HTTP origin, still subject to domain policy; any main-frame navigation to another origin, including link/script navigation and redirects, revokes it before admitting the new document.

No network authorization survives a finished tool operation: idle page requests are aborted. Service workers, WebSockets, downloads, popups, local file URLs and arbitrary evaluation are unsupported. The broker admits GET/HEAD/POST/PUT/PATCH/DELETE/OPTIONS through the enclosing approved effectful tool call. Body limits are 1 MiB request and 4 MiB response, 128 requests and 32 MiB responses per operation, and 32 pending requests in the driver. The result reports broker request/block counts. DOM/text output has a 64 KiB envelope; screenshots are viewport PNGs capped at 8 MiB. The process uses the ordinary composed sandbox. Each browser owns a private temporary profile/cache beneath its authorized cwd, removed after normal browser settlement; forced kills can leave disk residue. macOS WorkspaceWrite Seatbelt cannot nest Chromium's own sandbox, so launch fails explicitly without bypassing either policy. The [follow-up](audit-interactive-integrations-qa-followup.md) distinguishes this OS limitation from verified usability with explicitly configured Off/full-access execution. `[web] enabled=false` leaves the browser installation unbound in the CLI.

Setup example (trusted paths appropriate to the machine):

```sh
npm install --prefix /absolute/trusted/browser-deps playwright
export HEYCODE_BROWSER_NODE=/absolute/path/to/node
export HEYCODE_BROWSER_MODULE=/absolute/trusted/browser-deps/node_modules/playwright
export HEYCODE_BROWSER_EXECUTABLE='/absolute/path/to/Chrome-or-Chromium'
heycode
```

Use Playwright >=1.51 and an installed compatible Chromium/Chrome. No downloads or installation are performed by heycode. The local canary used Node v25.2.1, bundled Playwright 1.62.1 and installed Google Chrome on macOS. `include_image=true` explicitly returns rich PNG media for attachment admission and requires a vision-capable model. With the default false, text models can use all DOM controls and create screenshots without being given image input.

## Local artifacts and notebooks

`artifact` actions are `register(path)`, `preview(id, include_image=false)`, `list`, and `remove(id)`. Registration stores a path plus content revision, up to 64 handles; preview reauthorizes and rereads the file and rejects changed contents. Removing a handle never deletes the file. File reads are capped at 8 MiB. Text/HTML previews are inert source text capped at 16 KiB; PNG images can be explicitly returned on the rich attachment plane. Other binary formats return a local path and metadata for human opening. Handles are world-local, while generated files remain ordinary workspace files.

For a rendered HTML preview, use `browser(action="preview", session, path)` on an open session. It reads through filesystem authority, uses an opaque page origin, and denies external requests throughout preview mode, including later inspect/click calls. Only explicit navigation exits preview mode. `browser screenshot` can then capture the rendered local file. Nothing is uploaded to a hosting service or publicly deployed.

`notebook_read(path, start_cell=0)` returns nbformat 4 cell indices/IDs/types/source plus a SHA-256 whole-file revision. Paging returns at most 32 cells, each with a 1 KiB source preview and explicit truncation; ordinary `read` can inspect longer source. `notebook_edit(path, expected_revision, action, cell_index, cell_id?, source?, cell_type?)` supports source replacement, insertion and deletion. Insertion before `cell_index` permits length for append and requires code/markdown/raw type. An optional expected cell ID adds a second conflict check.

The complete file is bounded to 8 MiB and validated before editing. Existing top-level/cell metadata, attachments, unknown fields and untouched cells survive; source string-vs-lines representation is preserved. Edited code outputs/execution_count are cleared. Unique cell IDs and v4.5 IDs are enforced. The old complete JSON is replaced through the existing atomic exact-edit operation, so external changes before or during commit fail. No notebook kernel or cell execution is implied.

## Native macOS computer helper

The embedded fixed Swift helper uses AppKit, Accessibility, CoreGraphics and ScreenCaptureKit through the existing subprocess/sandbox service. There is no model-supplied source/eval. Apple Command Line Tools are required; screenshot capture requires macOS 14+. Other OSes return unsupported platform.

`computer(action="status")` checks Accessibility and Screen Recording without prompting or capturing. Optional `bundle_id` diagnoses only that target's PID/window availability. `inspect(bundle_id)` returns the target app's window accessibility tree, refs and content revision. System menu/Recent Items traversal is excluded. `click(element)` uses AXPress; `type(element)` sets the referenced text element. These calls require the exact `expected_revision` from a fresh inspection. Password element values are redacted. `screenshot(bundle_id, path, include_image=false)` captures a normal window owned by that app, preferring on-screen windows, then titled windows, then largest area (stable window ID tie-break). It returns the window ID, on-screen state, frame, geometry revision and PNG dimensions. Off-screen capture can be blank. Without an element ref, `click(x,y)` and `type(text)` accept that screenshot geometry revision and send mouse/Unicode input to the target PID; these fallback inputs require an on-screen window, and clicks must lie inside the captured frame. `key` posts a bounded key/modifier combination to the selected PID using an inspection or geometry revision. Geometry checks detect window changes, not content changes; screenshot and inspect again before each input decision. This is app-targeted native interaction, not an implicit whole-desktop recorder. No microphone access is used.

OS permissions are checked on every actual operation and missing permissions produce specific System Settings guidance. The fixed helper has a 45-second operation deadline, bounded JSON/PNG output, and owned cancellation/settlement. It does not automatically request or grant OS permissions. Existing MCP services can supply alternate platform adapters through the same ordinary approval boundary; provider-hosted computer schemas are not treated as evidence that a desktop adapter exists.

## Local speech adapter

`transcribe_audio(action="status")` reports configuration without claiming a working recognizer/model. `transcribe_audio(action="transcribe", path)` accepts an explicitly selected PCM WAV, validates it with the attachment parser, sends exact bytes to the configured local command's stdin, closes input, and returns its UTF-8 stdout transcript. The tool does not capture a microphone, type into another app, use provider audio support, or require audio-capable inference.

```sh
export HEYCODE_STT_COMMAND='["/absolute/path/to/local-recognizer-wrapper","literal-argument"]'
```

The recognizer or wrapper must read PCM WAV stdin and emit transcript-only UTF-8 stdout; diagnostics belong on stderr. Install the engine/model separately. The adapter has a 32 MiB audio cap, 32 KiB transcript cap and 120-second deadline; nonzero exit, malformed audio, invalid/empty text, excessive output, cancellation and unconfirmed cleanup fail explicitly. Fixture tests exercise the actual subprocess transport using generated PCM data; they do not establish acoustic recognition accuracy. A later [acoustic QA follow-up](audit-interactive-integrations-qa-followup.md) exercised a real isolated whisper.cpp/tiny.en recognizer with generated speech and documents the exact stdin/stdout command. No microphone recording was used.

## Validation

Validation uses no personal application interaction, user microphone recording, paid provider or public deployment.

- `cargo test -p heycode-web --lib`: 31 passed, including the existing SSRF matrix and three raw browser admission/redirect tests.
- `cargo test -p heycode-tools`: 75 unit tests and 13 integration tests passed; five installed-browser/native tests are explicitly optional. This includes six nonoptional interactive tests.
- `cargo clippy -p heycode-tools -p heycode-web -p heycode-exec -p heycode-attachments --all-targets -- -D warnings`: passed.
- With explicit installed `HEYCODE_BROWSER_*` paths, all three real browser canaries passed: local navigation/type/click/text/screenshot/preview/close, stalled navigation cancellation plus reopen, and public `https://example.com/` through the Rust broker. The preview canary checks that later input cannot re-enable its HTTP broker; the public canary also verifies that clicking a public link revokes the earlier local-origin grant.
- `cargo test -p heycode-exec --test main bounded_text_read_marks_only_successful_observations`: passed text refusal, explicit bounded binary read, truncation and observation freshness.
- `cargo test -p heycode-cli --test main default_world_reports_exact_live_inventory`: passed with the added plugin, six tools and matching declared/actual contribution order.
- Swift helper type checking and readiness passed. The owned AppKit fixture exposes no AX windows and its windows are off screen on this host. Capture transport returns a correctly scoped PNG and geometry metadata, but off-screen pixels can be blank. The available-capability canary passed with explicit refusal of all three fallback input actions in that state. **Visible-window pointer/key input and AX element actions remain unverified here.** Preliminary background Unicode dispatch was observed but was focus-dependent and is not an end-to-end correctness claim; fallback inputs now require a visible window.

Run the strict native GUI canary on an unlocked interactive desktop with OS permissions:

```sh
HEYCODE_REQUIRE_NATIVE_INPUT=1 cargo test -p heycode-tools --lib interactive::tests::macos_owned_app_capture_and_available_input -- --ignored --nocapture
```

It creates, owns and cleans up a temporary AppKit app, checks target screenshot/window selection, then verifies actual edited text and button results via AX or visible-window input. The default optional canary can verify off-screen refusal; the strict environment flag makes that state fail instead of accepting partial capability evidence. Native input parity is not closed by this implementation's host-limited QA. The [follow-up](audit-interactive-integrations-qa-followup.md) confirmed that the Mac is locked; GUI testing is paused until the user unlocks it and lets us know. The STT test validates the generated-PCM subprocess transport, errors/bounds, cancellation and child exit settlement; acoustic recognition quality remains dependent on the separately installed recognizer/model.

Primary API references consulted before use: [Playwright BrowserContext](https://playwright.dev/docs/api/class-browsercontext), [Page](https://playwright.dev/docs/api/class-page), [Locator](https://playwright.dev/docs/api/class-locator), [Route](https://playwright.dev/docs/api/class-route), [Jupyter nbformat](https://nbformat.readthedocs.io/en/latest/format_description.html), [Apple SCScreenshotManager](https://developer.apple.com/documentation/screencapturekit/scscreenshotmanager), [CGEvent postToPid](https://developer.apple.com/documentation/coregraphics/cgevent/posttopid(_:)), and [whisper.cpp CLI](https://github.com/ggml-org/whisper.cpp/tree/master/examples/cli).
