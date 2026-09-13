# Agent conversations and user questions

Named native agents run in the background by default. The parent can continue
other work or end its turn; new agent messages and completed results are
admitted automatically. A normal completion produces one named transcript
receipt. Opening that receipt shows its retained result and run information.
Routine agent inspection and waiting are not required for delivery.

## Switching and inspecting agents

The footer shows active agents with a colored ring, their name, and elapsed
time at the right. Only the current conversation has a filled circle. Keyboard
focus and conversation selection are separate: moving through the list does
not select another conversation until you open it. Returning to main restores
its draft and normal footer.

Completed agents leave the active list. `/agents` opens retained history.
Failures have a readable reason and an issue indicator; reviewing or dismissing
attention preserves the diagnostic. A recovered tool error does not turn a
successful agent into a failed run. Settled durations stop increasing.

A failed retained native conversation can offer Retry. This is an explicit new
run that first reconciles partial effects. Startup failures, lost conversations,
one-shot agents, and unsupported runtimes do not offer that capability. Their
error remains inspectable; a new task requires an explicit prompt. Stopped
agents are never automatically restarted by an incoming message.

## Model-facing messaging

`agent` returns a structured receipt containing the stable agent ID and display
name. Native agents retain their conversations by default where authority
permits it. Explicit one-shot presets keep their existing lifecycle.

`send_message` accepts `to` and `message`. The destination can be an authorized
agent ID, an unambiguous name, `parent`, or `main`. A scoped roster is supplied
with the tool. Names never grant control authority: messaging a sibling does
not grant permission to interrupt or archive it. Ambiguous or reused aliases
require an exact agent ID. Sender identity comes from the runtime, and an agent
message never becomes a human authorization.

`agent_control` offers explicit interrupt, archive, and restore operations.
Legacy inspection/wait dispatch remains compatible for diagnostics, but those
operations are not advertised as the normal model workflow and do not return
successful result bodies through a second delivery path.

## Asking for input

Use `ask_user_question` when an answer is required to proceed. Use
`ask_user_question_async` for optional clarification while useful independent
work can continue. Both tools accept one to four `questions`:

```json
{
  "questions": [
    {
      "id": "scope",
      "question": "Which areas should the review cover?",
      "header": "Scope",
      "mode": "multiple_choice",
      "options": [
        { "label": "Runtime", "description": "Message delivery and lifecycle" },
        { "label": "Interface", "description": "Navigation and presentation" }
      ]
    },
    {
      "id": "constraints",
      "question": "Are there any additional constraints?",
      "mode": "free_text"
    }
  ]
}
```

`single_choice` selects one option. `multiple_choice` uses Space to toggle
options and Enter to submit explicitly. Choice questions also offer custom
text. Required batches show progress and return answers by stable question ID;
optional cards retain their own durable identities. Closing an optional panel
keeps the question pending. Dismissing or cancelling does not select an answer.
A highlighted option, silence, or elapsed time never counts as submission.

Answers return to the conversation that asked, including native subagents.
The managed prompt directs agents to use these tools when user input is needed
and to avoid asking questions already answered by the user's instructions.
Older flat tool arguments remain accepted for compatibility.

The shared host question broker supports the complete contract. Provider-native
question protocols have separate limits: the Codex adapter preserves supported
batches and typed selections; the pinned Claude scalar dialog protocol rejects
batch or multiple-selection shapes it cannot represent. Unsupported secret or
restricted-custom-answer requests fail explicitly. These adapter contracts do
not establish live-provider behavior; controlled test evidence is tracked in
the [implementation tracker](../engineering/agent-message-delivery-tracker.md).
