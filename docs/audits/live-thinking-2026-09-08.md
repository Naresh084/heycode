# Live thinking visibility

The activity line now distinguishes awaiting a model response from observed
reasoning and includes the effective effort, falling back to `default` when
unspecified. Tool execution, approval, and response phases keep their own labels.
Live reasoning cards preview two rows of readable provider text; Ctrl+R or a
header click expands the full available text. Completed blocks remain collapsed.

The Chat Completions parser previously retained structured `reasoning_details`
only for continuation. It now emits public `reasoning.text` and
`reasoning.summary` content to the display stream while preserving the original
provider state. Same-delta aliases do not produce duplicate display text.
The first encrypted-only detail emits an empty reasoning delta as an explicit
activity signal. Runtime normalization accepts this signal without weakening
commentary validation. The UI explains that readable text was not supplied.
Encrypted data is never rendered or replaced with invented thinking text.

The inspected Muse session contained only `reasoning.encrypted` details.
The app cannot know the provider is thinking before it sends a reasoning event,
and selecting an effort does not establish when reasoning begins.

Validation: 801 LLM/TUI tests passed (two ignored), followed by 39 runtime tests
including empty-reasoning replay. Targeted clippy, formatting, and the debug CLI
build passed. The terminal fixture streams structured reasoning before tool
execution, checks the live preview and effort label, and asserts that its opaque
fixture state is absent from the screen.
