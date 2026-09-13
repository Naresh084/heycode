//! Explicit, turn-scoped workflow requests shared with the composer.

/// Instructions apply to native and delegated inference without starting work.
pub(crate) const INSTRUCTIONS: &str = "\n\n# Workflow opt-in\nWorkflow orchestration is off by default. Do not start a workflow just because a task is complex. When the current human message explicitly requests workflow use, including the standalone keywords `ultracode` or `workflow`, use the workflow tool to organize and execute that request. `ultracode` is an alias for requesting a dynamic workflow, not a shell command. Choose the workflow steps for the actual task and report its progress. This request applies only to that turn, not later messages. Respect negations, quoted examples, and questions about workflows: they do not authorize starting one. If the workflow tool is unavailable, explain that rather than claiming a workflow ran. Existing permission and Plan-mode restrictions still apply. Execute normal tool calls in the foreground and wait for their result. Request background execution only deliberately, using an explicit background tool or background=true; do not wrap every call in a background job. Read returned foreground output directly. Retrieve background output or additional clipped output with the available job controls.";

/// Whether a draft contains a workflow trigger, excluding common explicit opt-outs.
#[must_use]
pub fn workflow_requested(text: &str) -> bool {
    let words = text
        .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '\'')
        .filter(|word| !word.is_empty())
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    words.iter().enumerate().any(|(index, word)| {
        if !matches!(word.as_str(), "workflow" | "ultracode") {
            return false;
        }
        !words[index.saturating_sub(4)..index].iter().any(|word| {
            matches!(
                word.as_str(),
                "no" | "not" | "don't" | "dont" | "without" | "disable" | "avoid"
            )
        })
    })
}

#[cfg(test)]
mod tests {
    #[test]
    fn keywords_are_whole_words_and_not_a_default() {
        for text in [
            "ultracode fix this",
            "can you use workflow and inspect the code",
            "WORKFLOW: investigate",
        ] {
            assert!(super::workflow_requested(text), "{text}");
        }
        for text in [
            "fix this",
            "workflows",
            "my_workflow",
            "ultracoder",
            "don't use workflow",
            "do not use a workflow",
            "without ultracode",
        ] {
            assert!(!super::workflow_requested(text), "{text}");
        }
    }
}
