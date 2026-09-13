# Tool transcript and filesystem repair

The screenshots exposed a misleading successful-result renderer on failed
searches, arbitrary source fragments on collapsed reads, ambiguous todo update
status, escaped retained-output JSON, and task counts inflated by completed
executions.

The terminal now labels todo updates `Plan updated · N/M complete` and the
checklist footer `Plan`. Collapsed reads show returned line counts; expansion
retains their text and turns tabs into spaces so line numbers do not join code.
Failed searches render the error directly without counting its lines as
matches. Search previews begin with the first result, shell previews use the
last two nonempty lines, and expanded retained output displays its text field.
Repeated click hints appear only on focused cards. Completed/cancelled tool
and job records remain in task history but do not inflate the collapsed strip.

The original recorded filesystem error lacks an OS error code, so its precise
historical cause is not established. Current glob succeeded against this
workspace normally, but reproduced the same generic error with a 64-handle
process limit: OS code 24 (too many open files). The traversal eagerly held
directory handles for pending siblings. It now queues relative paths and opens
them through the retained capability root when visited. Root restrictions,
ignored directories, sorting, cancellation, and result caps are preserved.
No permission policy was relaxed.

Validation: 536 targeted executor/tool/TUI tests passed, nine ignored. TUI
tests were rerun after the final output-spacing refinement. A regression with
512 sibling directories also passed in a process limited to 64 open handles.
Targeted clippy, formatting, diff checks, and the debug CLI build passed.
The local terminal fixture exercises actual todo, read, glob and shell tools,
including a missing-directory error and expansion/scrolling of retained file
output. Its model responses are fixtures; no paid model call was required.
