# Human dictation to the composer (L13)

Base: `92ffd2a`, with interactive scope/acoustic prerequisite `94d4d18`.
Owner: session-controls. This delta adds a human recording-to-draft journey;
the existing `transcribe_audio(path)` model tool remains a file transcription
surface. Neither route depends on provider audio support.

## Journey

1. Configure an installed local recognizer and model, then restart heycode.
2. Run `/voice status` to inspect installation and macOS microphone permission.
   This action never requests permission or opens the microphone.
3. With an existing draft, press Ctrl+P and run `/voice start`. The full text and
   cursor are preserved. The composer shows starting, RECORDING, stopping and
   transcribing states; the flat screen-reader projection reports them too.
4. Run `/voice stop`, again through Ctrl+P if retaining a draft. Recording also
   stops at 60 seconds. The validated WAV is transcribed by the local command.
5. If text and cursor are unchanged, the result is inserted at that cursor with
   separating spaces. It is never submitted or queued to the model/inbox. If
   the draft changed, one transcript waits for `/voice insert` or cancellation.
6. `/voice cancel` cancels capture or transcription and leaves the draft alone.

Dictation can run while a model turn is active. It does not interrupt that turn
or change model history. Opening a child task cancels the parent recording;
dictation commands require the original parent composer. A session switch
returns the existing `RecomposeSession` outcome, tears down the old controller,
and constructs a new controller from the new composed Agent/session. Operation
identities reject stale events, including after cancellation or a session switch.

## Local setup

`HEYCODE_STT_COMMAND` is a JSON array containing an absolute executable and literal
arguments. The executable reads PCM WAV from stdin until EOF and emits only a
UTF-8 transcript on stdout; diagnostics belong on stderr. Installation presence
does not prove model availability or recognition quality. Errors are visible.

For the tested whisper.cpp CLI, the stdout flags are necessary:

```sh
export HEYCODE_STT_COMMAND='["/absolute/whisper-cli","--model","/absolute/ggml-tiny.en.bin","--file","-","--output-txt","--output-file","-","--no-prints","--no-timestamps","--language","en","--threads","4","--no-gpu"]'
```

The built-in macOS capture helper is fixed Swift/AVFoundation source. It needs
`/usr/bin/swift` and installed Command Line Tools. On an explicit `/voice start`,
it requests microphone permission only when not yet determined; denied or
restricted permission returns guidance for System Settings > Privacy & Security
> Microphone. There is no automatic permission request at application startup.

Swift's default temporary/module-cache locations are outside WorkspaceWrite on
this host. The helper now uses a private, owner-cleaned `/private/tmp` directory
for `TMPDIR` and `-module-cache-path`, under that policy's existing temporary-file
grant. It never widens policy or retries with sandboxing disabled. The scratch
directory contains compiler artifacts; captured audio remains in memory.

Other platforms require an explicitly configured capture implementation:

```sh
export HEYCODE_VOICE_CAPTURE_COMMAND='["/absolute/capture-helper","literal-argument"]'
```

The helper must emit `HEYCODE_VOICE_READY\n` only after capture starts, record until
stdin EOF, then emit one PCM WAV and exit. A bounded `ERROR ...\n` before
readiness reports setup/permission failure. There is no shell expansion or
model-supplied executable. Native Windows/Linux microphone adapters are not
implemented by this delta.

## Ownership and bounds

- All launches use the frontend's composed `SubprocessService`; exact argv,
  cwd, process-tree ownership and sandbox mediation are preserved.
- One operation and at most one waiting transcript per controller. Cancellation
  retains the operation until cleanup completes; a replacement cannot start
  while the previous process is still being settled. Dropped frontend/task
  owners also cancel their token and retire their process handles.
- Readiness: 45 seconds and a 1 KiB line. Recording: 60 seconds, 12 MiB of WAV,
  and a validated maximum 60-second duration. Stop/cleanup: 8 seconds before
  process ownership is retired. No unbounded audio buffering or disk recording.
- Shared STT: 32 MiB maximum valid PCM WAV, 120-second inference deadline,
  32 KiB UTF-8 transcript, successful child exit required. Empty, invalid,
  failed and interrupted output never becomes an inserted transcript.
- `LocalSpeech` and `SpeechTranscript` are exported from
  `heycode_tools::interactive`. Both the model file tool and TUI capture call the
  same validation/transport/settlement implementation. The optional duration
  ceiling lets capture impose its shorter recording bound without another parser.

## Validation

The composed tests use real slash-keyboard submission, controller events,
generated PCM, owned fake capture processes and the actual local STT subprocess
transport. They check cursor/draft retention, visible and screen-reader state,
no automatic submission, active-turn preservation, explicit insertion after
editing, capture/STT PID settlement, teardown, session/child switches, stale
results, missing permission/recognizer, invalid audio, duration/audio/transcript
bounds and a fresh controller after session recomposition.

The optional real-recognizer canary used the interactive workstream's generated
6.127-second Samantha sample and whisper.cpp `52a939a` with tiny.en. Both the
original **16 kHz** file and a generated **48 kHz mono 16-bit PCM** version
passed through fake capture, shared STT and insertion into the original composer
with **1 deletion / 19 words (5.26% word error)**. No message was submitted.
The pinned recognizer's `read_audio_data` initializes its miniaudio decoder at
`WHISPER_SAMPLE_RATE`, handling resampling. This is one synthetic acoustic smoke
test in two input formats, not an accent/noise benchmark or a dictation-quality
guarantee. Other recognizers must accept the microphone's input sample rate.

The optional macOS `status` canary passed through a composed **WorkspaceWrite**
executor and reported the current OS permission without requesting access or
recording. Private compiler scratch cleanup was also checked. Swift source
type checking passed independently.

Focused validation: nine voice tests and two shared speech tests passed;
strict `cargo clippy -p heycode-tui -p heycode-tools --all-targets -- -D warnings`
passed. The real recognizer and macOS status tests are deliberately optional;
they were explicitly run as the microphone-free canaries described above.
The full TUI suite passed (59 unit and 258 integration tests; two optional
canaries excluded from the default run), and the default CLI's exact live
command inventory test passed with `/voice` registered as an immediate TUI
command. The status canary also confirmed private compiler-cache deletion.

Actual microphone recording, first-use OS permission prompts and locked-desktop
interaction remain **paused at the user's request**. These checks require an
unlocked interactive desktop and should be consolidated with the main audit's
remaining desktop requirements. No microphone was accessed by these tests, no
recognizer was installed globally, and no persistent environment was changed.

Primary API references: [Apple AVAudioEngine inputNode](https://developer.apple.com/documentation/AVFAudio/AVAudioEngine/inputNode),
[Apple AVAudioNode recording taps](https://developer.apple.com/documentation/AVFAudio/AVAudioNode),
and [whisper.cpp CLI](https://github.com/ggml-org/whisper.cpp/tree/master/examples/cli).
