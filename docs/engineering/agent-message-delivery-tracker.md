# Agent experience implementation tracker

Goal: complete the [implementation plan](agent-message-delivery-plan.md), including structured user questions (AM12), and rebuild heycode for end-to-end testing. AM12 is required for completion of the active goal.

Status: active. Quality and integrated evidence determine completion. Work directly in the existing checkout and preserve unrelated edits.

| ID | Task | Status | Owner | Evidence / remaining work |
| --- | --- | --- | --- | --- |
| AM01 | Reproduce and trace reported symptoms | In progress | Root | Source causes recorded in plan; capture baseline binary and regression evidence |
| AM02 | Durable agent/run/message/completion identity | In progress | Runtime | Stable provenance, retry and replay tests |
| AM03 | Automatic delivery and recipient scheduling | In progress | Runtime | Busy/idle/nested admission, one wake owner, no duplicate calls |
| AM04 | Named asynchronous messaging | In progress | Root | Parent/child/sibling routing, retained context, no receipt loops |
| AM05 | Model tools and delegation behavior | In progress | Root | Remove routine polling schema, preserve diagnostic compatibility |
| AM06 | Concise completion and message presentation | In progress | Transcript | One named receipt; explicit raw-history disclosure |
| AM07 | Navigation focus and conversation switching | In progress | Navigation | Refresh-safe focus, consistent main return, preserved drafts |
| AM08 | Lifecycle, permission and resource integration | Pending | Root | Cancellation, questions, ownership and bounded delivery |
| AM11 | Failure evidence and settled-row cleanup | In progress | Failure UI | Typed readable diagnostics, correct active/issue counts, retry/history |
| AM12 | Structured user questions and model guidance | In progress | Questions + Root | questions[]; single/multiple choice; custom answers; required/optional ownership and actual terminal interaction |
| AM09 | Integrated terminal acceptance | Pending | Root | Five agents, messages, failures, resize, themes, restart |
| AM10 | Review, required checks and runnable build | Pending | Root | Check results and tested binary hash |

## Completion rules

- Mark a task complete only when its implementation and relevant verification pass.
- Record failing or incomplete checks explicitly; do not substitute screenshots for event/request assertions.
- Distinguish controlled local-provider evidence from live model behavior.
- Preserve prior evidence and identify the binary used for each integrated run.
- No commits, external publishing, or user-session restarts are implicit.

## Work log

- Tracker and active goal created before implementation dispatch continues. Detailed task checklists and dependencies remain in the linked plan.

- Baseline binary SHA-256: `54859973a386260dcd97c7a0da82a2f77d0150487ee4b2b7458d86cb9121e635`; manifest and dirty patch in `tmp/terminal-checks/agent-message-baseline-54859973a386260d`.

- First session/agent compile passed (`/tmp/dshx-agent-message-check.log`). UI compile exposed three local type/import errors; fixes applied, verification pending.
- Navigation, typed failure UI, durable completion outbox, and asynchronous named messaging are implemented in source; none are marked complete before integrated tests.

- User expanded the active goal acceptance criteria to structured question UX: one/many questions, explicit answer type, option selection, custom answers, and prompt guidance. AM12 must pass before goal completion.

- User is concurrently rebranding packages and the executable to `heycode`; subsequent checks use `heycode-*` package names and `target/debug/heycode`. The workspace remains at its existing path. A compile started before the rename is excluded from regression evidence.

- Renamed aggregate: agent library 143 tests passed; agent integration 330 passed with five failures. Three are old string-receipt/schema expectations; one explicit interrupt/archive/restore resume regression was corrected. New tests passed for durable duplicate suppression, restart-before-admission recovery, storage-error retry, idle batching, scoped named routes, exact pending Steer admission, and no message-consumption reply. Reverification includes the expanded question contract and retained-native retry.

- Runtime/session aggregate passed all nine executed suites (489 tests in total); UI library reached 289 pass / 1 narrow optional-question interaction failure / 2 ignored before its integration suite. The failure is being repaired. Agent integration reached 334 pass / 2 old background-delivery timing assumptions / 1 ignored; fixtures are being updated to wait on automatic completion rather than expect an unconsumed inbox.
- Review found and repaired an unregistered task-notification wait and introduced a foreground presentation wake barrier to prevent idle display during a newer automatic response. Durable delivery retries retain the same completion occurrence. These additions require renewed aggregate verification.
