# Interactive integrations: workspace authority, acoustic QA and native prerequisite

This follow-up fixes captured workspace authority, adds an optional real acoustic canary and records the precise native GUI blocker. It is based on committed integration snapshot `d1608a9c60dcadf3d63e3232329eec0285fb3dd4`. It does not add public tool schemas or change default composition.

## Workspace rebinding

All six interactive tools now implement the existing `Tool::rebind_workspace` contract. Notebook and artifact tools use the supplied filesystem capability, preserving its policy and observations. Browser, computer and speech also use the supplied shell's subprocess provider; an unsupported provider yields an explicit unavailable tool, never `None` followed by registry fallback to parent authority.

The lifecycle wrapper forwards rebinding and retains parent plugin retirement. Each child browser gets fresh session state and a descendant cancellation token; dropping a child does not cancel a shared parent token. Only the browser ID allocator is shared across the family, so parent/sibling numeric handles cannot collide with a different live child session. Artifact handles start empty in each child. Every operation resolves through the actual child `ToolCtx.cwd`.

Three nonoptional scope tests passed through real rebound registries:

- Child notebook read/edit and artifact register/preview succeed within the child; absolute parent paths, foreign artifact handles and a native screenshot output path outside the child are denied. The native path denial occurs before any helper or desktop capture. Parent bytes remain unchanged, nested read-only policy stays read-only and parent shutdown retires held child tool handles.
- A shell backend without a subprocess service cannot use the parent's browser, native helper or speech executor; filesystem-only child tools remain usable.
- A configured speech subprocess runs in the exact child cwd and the actual macOS WorkspaceWrite Seatbelt profile denies its attempted parent-file write. Parent audio paths also fail at child filesystem admission.

The optional real headless-browser scope canary passed: parent/child/sibling handles are distinct; foreign handles fail; browser launch specs carry exact child cwds; generated HTML and screenshot files use child authority; parent paths fail; and dropping the child leaves parent and sibling browsers operational. This successful interaction canary uses an **explicit Off process policy** for each scoped child while retaining a strictly child-rooted filesystem capability. It also verifies that ReadOnly fails before spawning or creating a private directory, and that WorkspaceWrite fails with the specific sandbox diagnostic without falling back to the parent executor.

Each launch creates a fresh `.heycode-browser-*` directory with mode `0700` underneath the authorized cwd. Playwright profiles, downloads and cache use that directory; the fixed adapter sets `TMPDIR` and `MAC_CHROMIUM_TMPDIR` for its own process. The latter is the macOS override used by [Chromium's temporary-directory implementation](https://github.com/chromium/chromium/blob/main/base/files/file_util_apple.mm). No model-supplied environment or launch flags are accepted. The canary verifies one private directory in each live scope, successful normal-close removal in parent and sibling, and cleanup following failed sandbox launch. Cleanup waits for any active launch/action and `browser.close()` before removing that one owned directory. A forced kill or owner drop can leave temporary files; they are retained instead of attempting deletion before tree settlement or through a broader executor. This is a disk-residue limitation, not a surviving browser process.

The private-directory change resolved Chromium's singleton socket failure. The next WorkspaceWrite launch reached Chromium subprocess startup and failed with `sandbox initialization failed: Operation not permitted`. Chromium's own sandbox remains enabled. macOS cannot nest these Seatbelt sandboxes; [Bazel's official sandbox documentation](https://bazel.build/docs/sandboxing) also documents that OS restriction. The tool now returns an actionable, sanitized diagnostic. The provided child policy is preserved: no root-executor fallback, extra filesystem grant, automatic Off policy or `--no-sandbox` fallback is introduced. **Installed Chrome under this host's WorkspaceWrite profile remains unavailable**; the verified Off/full-access configuration is explicitly host-selected and must not be cited as WorkspaceWrite support. Diagnostics also showed denied host Crashpad settings writes; the browser did not acquire permission to write those files.

Validation after rebinding: the expanded real scoped browser canary passed in 5.73 seconds, including cleanup and fail-closed policy checks. The three existing live browser tests (local interaction, public URL, cancellation/reopen) passed in 5.05 seconds. Final `cargo test -p heycode-tools` passed 78 unit and 14 integration tests, with seven optional tests ignored in that standard run; this includes the three nonoptional scoped authority tests. `cargo clippy -p heycode-tools --all-targets -- -D warnings`, `node --check` for the fixed driver, and `git diff --check` passed. The real acoustic canary is recorded below.

## Native GUI verification is paused

CUA was directed only to the test-owned `HeycodeComputerFixture.app`. It reported: “The Mac is locked and automatic unlock could not unlock it.” No personal app was inspected or interacted with. The temporary app was terminated. The user subsequently instructed us to pause this check until they manually unlock the Mac and let us know; no further GUI retries or unlock attempts are authorized by that pause.

This explains the earlier combination of successful OS permission preflight, no AX windows, off-screen window metadata and blank captures. Those results establish transport/refusal behavior, not successful visible-window or AX input. No helper change is justified solely to work around the lock screen.

After the user reports that the Mac is unlocked, run the existing strict owned-app canary:

```sh
HEYCODE_REQUIRE_NATIVE_INPUT=1 cargo test -p heycode-tools --lib interactive::tests::macos_owned_app_capture_and_available_input -- --ignored --nocapture
```

The remaining prerequisite is an **unlocked interactive Mac desktop** with the already checked Accessibility and Screen Recording permissions. This check remains open.

## Real acoustic recognition

An isolated temporary directory held a source build of [whisper.cpp](https://github.com/ggml-org/whisper.cpp) at commit `52a939a2a762224e255d366c1182b2af4dd1a032` and its public [tiny.en model](https://huggingface.co/ggerganov/whisper.cpp/tree/main). The build used four CPU threads, Accelerate and no Metal/GPU. No recognizer was installed globally, no microphone was recorded and no paid API was used.

Model SHA-256: `921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f`.

macOS `say`, Samantha voice at rate 150, generated this reference into a file:

> The quick brown fox jumps over the lazy dog. Please save the meeting notes in the local project folder.

FFmpeg converted it to mono, 16 kHz, 16-bit PCM WAV: 6.127125 seconds, 196,146 bytes, SHA-256 `892d1ccfae6bc46cd5cc4e87b7efb8ad2f6ba350b3495059e1407cfaf2a2b60c`.

The canary copies that file into its owned workspace and dispatches `transcribe_audio` through `execute_tool`, the ordinary pre-tool waterfall, the interactive plugin, the filesystem capability and the owned subprocess adapter. It verifies the returned source, sample rate, absence of recording/insertion effects and normalized word error against the reference. Its smoke-test acceptance threshold is at most 10% word error; it prints the measured error count and transcript.

The observed transcript was:

> quick brown fox jumps over the lazy dog. Please save the meeting notes in the local project folder.

The first recognition took 7,164 ms end to end through the tool and omitted the initial “The”: **1 deletion / 19 reference words = 5.26% word error**, ignoring case and punctuation. The completed canary passed with the same transcript and error count on the next run (284 ms). These are two observed timings, not a steady-state performance measurement. This demonstrates actual acoustic recognition through the integration. It is one short synthetic-voice sample, not an accent/noise benchmark, a latency guarantee, or a claim of perfect dictation.

A configuration defect was identified during this exercise: at the tested whisper.cpp revision, `--file -` alone selects stdin but produces no transcript stdout without an output format. The adapter correctly rejected the empty result. **`--output-txt --output-file -` are required** for the transcript-only stdout contract; a separate wrapper is unnecessary.

## Reproduce without recording a microphone

Prerequisites: Git, CMake, a C++ toolchain, curl, Python 3, FFmpeg, and the installed macOS Samantha voice. Run from the heycode repository. Everything below is contained in a new temporary QA directory; it does not modify the user's persistent heycode configuration.

```sh
STT_QA="$(mktemp -d -t heycode-stt-qa)"
git init "$STT_QA/whisper.cpp"
git -C "$STT_QA/whisper.cpp" fetch --depth 1 https://github.com/ggml-org/whisper.cpp.git 52a939a2a762224e255d366c1182b2af4dd1a032
git -C "$STT_QA/whisper.cpp" checkout --detach FETCH_HEAD
cmake -S "$STT_QA/whisper.cpp" -B "$STT_QA/build" -DGGML_METAL=OFF -DBUILD_SHARED_LIBS=OFF -DWHISPER_BUILD_TESTS=OFF -DCMAKE_BUILD_TYPE=Release
cmake --build "$STT_QA/build" --target whisper-cli -j 4
curl -fL --output "$STT_QA/ggml-tiny.en.bin" https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-tiny.en.bin
(cd "$STT_QA" && printf '%s  ggml-tiny.en.bin\n' 921e4cf8686fdd993dcd081a5da5b6c365bfde1162e72b08d75ac75289920b1f | shasum -a 256 -c -) || exit 1
say -v Samantha -r 150 -o "$STT_QA/generated.aiff" 'The quick brown fox jumps over the lazy dog. Please save the meeting notes in the local project folder.'
ffmpeg -nostdin -hide_banner -loglevel error -i "$STT_QA/generated.aiff" -ar 16000 -ac 1 -c:a pcm_s16le "$STT_QA/generated.wav"
export HEYCODE_STT_CANARY_WAV="$STT_QA/generated.wav"
export HEYCODE_STT_COMMAND="$(python3 - "$STT_QA" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1]).resolve()
print(json.dumps([
    str(root / 'build/bin/whisper-cli'),
    '--model', str(root / 'ggml-tiny.en.bin'),
    '--file', '-', '--output-txt', '--output-file', '-',
    '--no-prints', '--no-timestamps', '--language', 'en',
    '--threads', '4', '--no-gpu',
]))
PY
)"
cargo test -p heycode-tools --lib interactive::tests::speech_real_generated_speech_canary -- --ignored --nocapture
```

For routine use, install the trusted executable and model in durable host-owned paths and use the same JSON argv shape for `HEYCODE_STT_COMMAND`. Do not retain temporary QA paths as a permanent configuration. No expected transcript is passed to the recognizer; the reference lives only in the canary's comparison.
