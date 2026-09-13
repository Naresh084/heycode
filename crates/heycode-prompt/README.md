# heycode-prompt

`heycode-prompt` assembles the system prompt. Plugins register named, ordered
sections on the `prompt` service; `PromptRegistry::render` folds them into the
deterministic standing instructions sent ahead of every request. Sections are
pure functions of the `RenderContext`, empty sections are dropped, and
duplicate names fail loud.

The plugin itself owns three sections: `identity` (-100), `environment` (0)
and `project-instructions` (50). Other plugins add theirs through
`section_shared` on the live registry (`skills-catalog` at 90, plan mode, …).

## Project instructions

`prompt_plugin_with_instructions(sources)` renders the user's `AGENTS.md` /
`CLAUDE.md` into a `# Project instructions` section at order 50, between the
environment and the skills catalog. Two places are read, in this order:
`$HEYCODE_HOME/AGENTS.md` (the user's own, always), then the trusted workspace
root's `AGENTS.md`, `CLAUDE.md`, `.heycode/INSTRUCTIONS.md`, `AGENTS.local.md`
and `CLAUDE.local.md`. Nothing above the workspace is read — trust was granted
for the workspace, not its parents. Symlinked files are ignored, files over
64 KiB are cut with a visible marker, and files are read once at composition so
the prompt stays a pure function of its inputs.
`PromptRegistry::instruction_sources` lists what was loaded for `/status`. The
composition root passes the workspace only when workspace trust permits project
instructions (Unknown blocks; Restricted and Trusted permit).
