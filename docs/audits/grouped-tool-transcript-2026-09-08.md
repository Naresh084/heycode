# Grouped tool transcript

The supplied Claude screenshot makes assistant messages the primary content
and reduces successful tool activity to short, dim summaries. The dshx terminal
now follows that hierarchy instead of displaying every successful invocation
as a full card by default.

Successful read, grep/glob, edit/write and shell calls group between assistant
messages. Summaries count read/changed paths, search calls, shell calls and
available diff additions/deletions. A failure, pending call, todo update,
workflow or other distinct tool breaks the group. Nonzero shell status is not
folded into a successful group even when its transport returned successfully.

Clicking the summary or using Alt+Up/Down and Enter expands the original calls
with their full retained output and available reasoning. Completed reasoning
associated with a collapsed group folds with it. Running reasoning, pending
approvals and failures remain visible. Ordinary pending tool calls are hidden
from the transcript while the live activity line reports current work; their
settlement reveals the resulting group or failure. Durable call IDs, input, output, status,
and provider context are retained; grouping is a presentation projection.

The projection refreshes on relevant changes, including delayed settlement and
replay. Spinner ticks and streamed text do not rebuild the historical grouping.
Group visibility and expansion participate in transcript cache identity and
height indexing. Keyboard navigation skips folded member records, and flat
screen-reader output presents the same summaries and expansion state.

Validation: 358 TUI tests passed, including grouped mixed calls, failure/live
visibility, nonzero status, diff counts, mouse/keyboard expansion, long-group
scrolling, delayed results and replay. Both local terminal journeys passed:
conversation summaries with read expansion/scrolling, and real tool approval,
denial, file writes, retained-output retrieval and group expansion/collapse.
The screenshot is from local fixture responses, not a paid model session.
