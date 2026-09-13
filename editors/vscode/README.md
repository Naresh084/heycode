# heycode for Visual Studio Code

Installable VS Code client for heycode app-server protocol v1. The extension
bundles the fixture-locked `@heycode/sdk`; it does not duplicate the JSON-RPC event
or control schema.

## Host contract

Set `heycode.executablePath` to an absolute heycode executable. `heycode.hostArguments`
defaults to:

```text
app-server --stdio-v1
```

The extension appends `--workspace <absolute-folder>` and, for resume,
`--resume <exact-session-id>`. It launches the executable directly with
`shell: false`. The child owns one composed app-server/session until Close or
extension deactivation; no socket, endpoint file or network listener is used.

The shipping host composes the normal product world, obtains the effect-owned
`AppServer`, calls `serve_stdio_transport(server, stdin, stdout, cancellation)`,
and always shuts the Context down after that call settles. It defaults to
restricted workspace authority; add `--trust-workspace` before `app-server` in
`heycode.hostArguments` only after reviewing the project inputs that grants.

## Commands

- `heycode: Start Session`
- `heycode: Resume Session`
- `heycode: Send Message`
- `heycode: Cancel Turn`
- `heycode: Close Session`
- `heycode: Show Output`

Runtime permission requests show exactly three mapped choices: allow once,
allow for this runtime session, or deny. Dismissal is deny. Request ids are
insert-once within a turn, and the response uses the exact id emitted by the
host. Questions follow the same exact correlation. Raw protocol frames and
child stderr are never written to the VS Code output channel; failures shown to
the user are closed SDK messages.

## Verification

```sh
npm ci
npm test
npm run package
npm run test:host
npm run test:installed-host
```

`npm test` uses a deterministic child protocol fixture for the Node/extension
boundary. Rust tests in `heycode-app-server` independently run the stdio bridge
against the real `AppServer` host/session implementation, including correlated
permission choice, duplicate refusal and malformed-frame teardown. Both sides
parse `sdks/fixtures/app-server-stdio-v1.json` to lock operation/inner-request
correlation and envelope casing.

`npm run test:host` launches an isolated Extension Development Host. Set
`HEYCODE_VSCODE_EXECUTABLE_PATH` to the VS Code Electron executable to avoid a
download; on macOS the script also detects the standard installed application.
The host test activates the real extension, checks all commands and proves
activation does not start a process before the user explicitly connects.

`npm run test:installed-host` requires `target/debug/heycode`. It packages and
installs the VSIX into an isolated extension directory, configures the absolute
shipping binary with `--restricted-workspace --fake app-server --stdio-v1`, and
runs against an isolated `HEYCODE_HOME`. The journey opens a native session,
overlaps a cancel request with its first admitted fake turn, closes, resumes the
exact UUID in a fresh host process, proves a healthy follow-up turn, and closes
again. The shipping fake emits no permission or question requests; exact
allow/deny correlation remains covered by the deterministic protocol fixture.
