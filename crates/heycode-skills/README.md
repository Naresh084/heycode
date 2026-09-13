# heycode-skills

`heycode-skills` discovers `SKILL.md` instruction packs and contributes the
`load_skill` tool, skills prompt catalog, `/skills`, `/skill`,
`/skill-doctor`, `/reload-skills`, and the `"skills"` service.

`/skills` emits a validated human-only `skills` panel request. The TUI renders
the same snapshotting `SkillSet` service used by `load_skill` and `/skill`, so
the catalog cannot drift into a second filesystem scan. Discovery seeds the
registry immutably; verified declarative plugins may add token-owned rows that
withdraw on rollback/shutdown. User-only/model-invocable
state remains explicit, and file metadata is bounded and control-stripped by
the front-end boundary. `/skill` remains the model-scheduling command.

`/reload-skills` rescans only the roots bound by the existing composition. It
builds and validates the complete candidate before replacing filesystem-owned
rows under one registry lock; live declarative contributions survive, and a
name collision with one retains the entire prior generation. The result reports
added, removed, updated and skipped counts. It says the next request receives a
new prompt prefix only when the model-visible catalog changed; a user-only body
update does not falsely claim prompt-cache invalidation. `/skill`, `load_skill`,
the prompt section and the TUI catalog all read the same published generation.

## Discovery contract

Discovery is ordered and deterministic:

1. `<workspace>/.heycode/skills`
2. `<workspace>/.agents/skills`
3. `$HEYCODE_HOME/skills`

Directory entries are sorted by their platform filename order. The first
successfully loaded skill with a given frontmatter name wins across both a
root and the ordered root list. Missing optional roots are skipped. Initial
composition remains fail-soft, while a live rescan treats an unstable or
unreadable existing root as a source-attributed failed attempt and retains the
old generation. A skill whose declared name is invalid — empty, over 128 characters,
or carrying whitespace or control characters — is a **skipped row**, not a
reason to refuse to start: `discover_report` returns it with its root,
directory and reason, `SkillSet::from_discovery` retains it, and the `/skills`
panel lists it as `skipped — <reason>` beside the admitted skills. The plain
`discover` accessor returns only the admitted skills.

Callers must construct roots with `SkillRoot::bind(authority, relative)` or
`default_roots(workspace, home)`. A `SkillRoot` holds the nearest existing
authority directory and keeps any absent authority suffix in its no-follow
relative path; binding has no write side effects. Renaming or replacing an
ambient path cannot redirect later discovery. Every relative component,
skill directory, and `SKILL.md` is opened through that capability with
no-follow semantics. Symlinks and multiply linked documents are not admitted.
A document is limited to 1 MiB, must be a stable regular UTF-8 file, and is
revalidated through its opened handle and directory entry before publication.

Every `Skill.body` is an immutable in-memory snapshot. Neither the
model-facing tool nor `/skill` reopens a filesystem path, so a later path swap
cannot change the instructions they load.

## Trust integration

Workspace trust decides which authorities are supplied; this crate does not
infer trust from a path. Trusted or policy-approved project instructions use
`default_roots` and a dynamic workspace host uses
`skills_plugin_with_workspace_reload_guard`, so a later `/cd` or worktree
transition cannot make `/reload-skills` rescan the earlier project silently. A
home-only world binds exactly:

```rust,ignore
let roots = vec![heycode_skills::SkillRoot::bind(heycode_home, "skills")?];
let plugin = heycode_skills::skills_plugin(roots);
```

Do not join a project path and pass the result as a new authority. The
workspace/home directory is the authority; `.heycode/skills`, `.agents/skills`,
or `skills` is always the untrusted relative portion.

## Verification

```sh
cargo fmt --all --check
cargo clippy -p heycode-skills --all-targets -- -D warnings
cargo test -p heycode-skills
```

The integration suite covers live prompt/loader/command refresh, atomic
collision rollback, source-scoped delta/partial-failure reporting, root
precedence, sorted collision resolution,
tool/command behavior, symlinked directories/files/ancestors, hard links,
ambient workspace replacement, missing-home creation/symlink races, immutable
post-discovery loading, and unsafe relative-root rejection.

`/skill-doctor` reports admitted/skipped skills, model-versus-user invocation
eligibility, body sizes, approximate context cost, delivered-body counts and
bodies still retained after compaction. It uses the current trusted catalog and
durable conversation, makes no provider request, and does not claim the model
followed an instruction merely because it received the body.
