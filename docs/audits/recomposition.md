# Same-session renderer switching and plugin reload

Validated on 2026-09-11 through the actual native CLI and pseudo-terminal, using a disposable home/workspace and a fake inference adapter. No inference request was sent. This is controlled lifecycle evidence; the paired Claude presentation gate remains open.

`/tui [auto|screen-reader]` switches presentation, with no argument toggling the current mode. `/reload-plugins` rebuilds the session composition from durable configuration. Both commands wait for foreground work and require an attached terminal. They acquire the agent's shared recomposition permit, refuse pending drafts/dialogs/queued commands, close admission, shut down the old composition, and reopen the same durable session. An initially unused session is preserved through this path rather than discarded during teardown.

Renderer activation can restore the prior presentation if startup fails. Plugin reload cannot reconstruct an old in-memory plugin generation after shutdown. A failed reload instead reports the retained session id and the configuration error. The typed reload failure propagates past earlier successful session/display restarts, preventing them from retrying the broken configuration or repeating the recovery message. No atomic rollback of plugins is claimed.

## Evidence

`scripts/recomposition_pty.py` exercises an initially unused session through `/tui screen-reader`, `/rename Renderer boundary preserved`, `/tui auto`, `/reload-plugins`, and `/release-notes`. It verifies terminal teardown/reentry, the same journal path and original event prefix, the durable title, and zero `user/message` or `request/header` events.

The optional `--failure` case then makes the disposable configuration invalid and reloads again. It verifies process exit, exactly one recovery message naming the saved session, retention of the complete earlier journal prefix, and zero model requests.

- `tmp/terminal-evidence/recomposition-20260911T081503Z-47a128`: successful two-switch/one-reload journey, including clean process teardown.
- `tmp/terminal-evidence/recomposition-failure-20260911T082037Z-fc1006`: full successful journey followed by failed activation; all assertions pass. Uses immutable `tmp/cli-snapshots/72a09ba760a8d669/dshx` built from the current source checkpoint.
- Each directory retains decoded terminal text, PNG frames, raw terminal bytes, session events, and machine-readable results. The earlier flat/automatic screenshots and failure output were inspected.
- `tmp/terminal-evidence/recomposition-shared-20260911T084903Z-96f94d`: the full success-and-failure journey passes again on the shared CLI build with structured findings and command transcript labels. The retained-session failure frame was inspected. The same binary also passes the strengthened local command checks in `tmp/terminal-evidence/command-reference-shared-20260911T084917Z-4f8469`, including actual context/help content and zero inference events. This checkpoint precedes the subsequent self-paced schedule admission change.

The first successful run exposed a macOS harness teardown hang: the parent waited to reap a killed terminal child while retaining its PTY peer. Closing the peer before reaping resolves it. Legacy terminal fixtures now pass `--no-background` so their cleanup owns the complete test process. Whole-session background hosting has its own dedicated lifecycle fixture.

The first failure run retained the session but repeated its error through nested restart handlers. The typed failure fix and second failure run close that issue. No provider fallback, live plugin-server replacement, or full visual parity is inferred from these tests.
