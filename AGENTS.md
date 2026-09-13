# File naming

Name files and directories for their behavior or responsibility. Do not use
development phases, milestones, workstream labels, or progress words such as
`current`, `final`, `fixed`, or `wip` as names.

Use descriptive names for scripts, modules, reports, and verification artifacts.
When immutable evidence needs a unique identity, use a timestamp or content hash.
Protocol and persisted-schema versions describe compatibility contracts and may
remain explicit in fixtures that test those versions.

When renaming a file, update its imports, module declarations, commands, links,
and other references in the same change. Preserve captured evidence contents.
