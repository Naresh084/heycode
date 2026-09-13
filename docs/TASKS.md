# TASKS — bug-fix and law-reconciliation backlog

This file records the completed 18-task audit-remediation round. The active product program is the dependency-aware [engineering implementation tracker](engineering/TASKS.md), supported by the [master plan](engineering/README.md). Do not add new product work to this historical remediation list.

Opened after a full end-to-end audit of `AGENTS.md`, `README.md`, `FEATURES.md`,
`docs/STATUS.md`, `docs/GOTCHAS.md` against all 79 `.rs` files in the workspace.

Every task below is TDD: **write the failing test, watch it fail for the right
reason, then implement**. A task is done when its test passes AND
`cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings
&& cargo test --workspace` is green.

Two scope decisions were taken by the maintainer before work started:
- **Plan mode and the sandbox plugin get MOUNTED** (not doc-demoted). → T14
- **Config-driven composition gets WIRED UP**, factory registry included. → T15

Status legend: ` ` todo · `~` in progress · `x` done.

**All 18 tasks are done.** Verified green together: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`
(239 passed / 0 failed / 26 suites), and `HEYCODE_HOME=$(mktemp -d) cargo run -q --
--fake run "smoke"`.

Two fixes are deliberately NOT covered by a dedicated test, and are recorded as
such in STATUS.md rather than claimed as verified:
- **T12** (`Context::shutdown()` in every run mode) — the run modes have no test
  harness; disposal itself is covered by heycode-core's unit tests.
- **T13** (ACP `cwd` threading) — observing it needs a scripted-provider-in-world
  hook that STATUS.md deferral #3 already tracks.
- **T1**'s Linux `apply_and_exec` path still cannot run here; what the fix DID
  buy is that the argv half now compiles and is tested on every host.

---

## P0 — the gate is red

### [x] T0. `cargo fmt --all --check` fails
**Evidence:** two diffs in `crates/heycode-tui/src/app.rs` at `:267` and `:295` —
rustfmt wants the `if let … && …` let-chain bodies re-braced.
**Why it matters:** `AGENTS.md` §0's definition of done is a conjunction that
short-circuits here, so *nothing* is currently "done". `GOTCHAS` #12: run fmt
BEFORE clippy or reflows mask real lints.
**Fix:** `cargo fmt --all`.
**Test:** the gate command itself, exit 0.

---

## P1 — correctness bugs

### [x] T1. Landlock `confine` does not compile on Linux
**Evidence:** `crates/heycode-sandbox/src/landlock.rs:210` binds the parameter as
`_argv`, but the `#[cfg(target_os = "linux")]` branch at `:223` calls
`linux_impl::launcher_argv(&rules, argv)` — `argv` is not in scope. The non-Linux
branch early-returns, so macOS CI never compiles the failing arm.
**Also:** `:176` uses `argv.split_first().expect("checked non-empty")` in
production Linux-gated code — `clippy::expect_used` is denied workspace-wide, so
a Linux `-D warnings` build fails there too.
**Fix:** rename `_argv` → `argv`; replace the `expect` with a let-else returning
`SandboxError::new("empty argv")`.
**Test:** a compile-gate test that exercises the Linux path shape without a Linux
host — extract the argv-threading into a `#[cfg(test)]`-visible helper and assert
it forwards argv into the launcher rules. Plus `cargo check --target
x86_64-unknown-linux-gnu` if the target is installable; otherwise document that
this remains host-unverified (STATUS.md deferral #2 already says so).

### [x] T2. `PlanGuard` never delegates to `next`
**Evidence:** `crates/heycode-agent/src/plan.rs:83-97` — `handle` binds `_next` and
returns `Ok(())` unconditionally, including on the allow path. Only the deny
short-circuit is justified by the comment.
**Why it matters:** violates principle #6 and `GOTCHAS` #3. Currently inert only
because the plugin is never composed — T14 mounts it, so this MUST land first.
Once mounted, every layer registered after `PlanGuard` on `seam/pre_tool` is
skipped for every tool call.
**Fix:** call `next.run(input).await` on the allow path; return without it only
when the deny verdict was set.
**Test:** register `PlanGuard` plus a downstream tally layer on a `Waterfall`;
assert the tally increments for a non-mutating tool with plan mode ON, and does
NOT increment for `write` with plan mode ON.

### [x] T3. `/skills` prints nothing
**Evidence:** `crates/heycode-skills/src/lib.rs:261` — `SkillsList::execute` builds
the listing into `let _text = …` and drops it, returning `Ok(())`. No `UiEvent`,
no output. Untested.
**Fix:** emit `UiEvent::Info { text }` on the agent's bus, matching how every
other command reports (`commands.rs` `Help`/`Model`/`Provider`).
**Test:** subscribe to `UiEvent`, run `/skills` against a registry with two
skills, assert an `Info` event whose text names both skills and tags the
`disable_model_invocation` one `[user-only]`; and that an empty registry emits
"no skills discovered".

### [x] T4. Config section defaults silently become zero
**Evidence:** `CompactionSection::threshold_ratio` / `context_window` and
`SubagentSection::max_depth` are `#[serde(default)]` on the FIELD. Per
`GOTCHAS` #6, a field default only applies when the parent section exists — so a
TOML containing `[compaction]` with no `threshold_ratio` deserializes to **0.0,
not 0.8**, and auto-compaction then fires on every single turn. `impl Default`
exists for these sections (`lib.rs:115,133`) and disagrees with serde.
**Fix:** point the field attributes at the section's own defaults
(`#[serde(default = "…")]` helpers derived from `impl Default`), so the
hand-written `Default` is the single source of truth. Audit every
`#[serde(default)]` in the file for the same divergence.
**Test:** parse `"[compaction]\nauto = true\n"` and assert `threshold_ratio ==
0.8` and `context_window == 128_000`; parse `"[subagent]\n"` and assert
`max_depth` equals the documented default. These fail today.

### [x] T5. `--set` boolean patches coerce garbage to `false`
**Evidence:** `ui.auto_title` / `compaction.auto` patches accept anything and map
values outside `{true,yes,1}` to `false` instead of erroring. `AGENTS.md` §1.4
says malformed config fails loud naming the offender.
**Fix:** parse strictly; return `ConfigError::Parse` naming the path and the bad
value.
**Test:** `--set ui.auto_title=maybe` errors and the message contains both
`ui.auto_title` and `maybe`.

### [x] T6. `subagent` plugin's `inject()` omits `"session"`
**Evidence:** `crates/heycode-agent/src/subagent.rs:520-528` declares six keys;
`apply()` at `:551` also requires `"session"`. A missing session therefore
surfaces as a runtime `CoreError::other("session missing")` instead of the
declared-inject failure principle #4 promises.
**Fix:** add `"session"` to the inject list.
**Test:** compose the subagent plugin without a session plugin; assert the error
is the unsatisfied-inject variant naming `subagent` and `session`, not
`other`.

### [x] T7. `/help` omits every late-registered command
**Evidence:** `crates/heycode-agent/src/commands.rs:140-149` — `builtin_help_lines()`
is a hardcoded six-line vec. `/skills`, `/skill` and (after T14) `/plan` register
late via `register_shared` and never appear. `CommandRegistry::help_lines()`
(`:89`) is the method that would fix it, but it ignores the `late` vec and has
zero callers.
**Fix:** make `help_lines()` merge early + late commands from their own
`name()`/`help()`, and have `Help::execute` call it.
**Test:** register a late command, run `/help`, assert its name appears in the
emitted `Info` text.

### [x] T8. A default `cargo test` hits the live network
**Evidence:** `crates/heycode-tools/tests/web.rs::keyless_ddg_search_returns_live_
results_when_network_allows` makes a real DuckDuckGo request. `AGENTS.md` §9
mandates real-network e2e sit behind `HEYCODE_E2E=1` and skip silently without it.
**Fix:** gate on `HEYCODE_E2E`, early-return when unset.
**Test:** the test itself — assert it is skipped (no network syscall) with
`HEYCODE_E2E` unset.

---

## P2 — architecture: make the law true

### [x] T14. Mount `plan_plugin()` and `sandbox_plugin()`
**Evidence:** neither appears in the plugin vector at
`crates/heycode-cli/src/lib.rs:236-289`. `plan_plugin()` occurs workspace-wide only
in its definition and `crates/heycode-agent/tests/plan.rs:80`. `sandbox_plugin()`
(`crates/heycode-sandbox/src/lib.rs:141`) has **no callers at all** — confinement
reaches `bash` through `AgentOptions.sandbox` instead, a live exception to
principle #1.
**Depends on:** T2 (PlanGuard `next`) — do not mount a broken guard.
**Fix:** add both rows to `compose_world`. Provide `"sandbox"` as
`Arc<dyn Sandbox>` from the same backend `sandbox_backend(cfg)` already resolves,
and have `AgentOptions` read the service rather than owning a second copy.
Register `plan_plugin()` after `commands_plugin()` so `/plan` and the PLAN MODE
prompt section land.
**Test:** compose a default world; assert `ctx.get::<PlanHandle>("plan")` and
`ctx.get::<Arc<dyn Sandbox>>("sandbox")` are both `Some`; assert the tool
registry contains `exit_plan_mode` and the command registry contains `plan`;
assert a `write` call is denied while plan mode is active end-to-end.

### [x] T15. Wire config-driven composition + the factory registry
**Evidence:** `Config::resolve_plugins` (`crates/heycode-config/src/lib.rs:473`) is
called only from its own unit tests (`:509`, `:547`). `compose_world` hardcodes
the vector and never reads `cfg.profile.plugins`. The setup wizard *writes* such
a list to `~/.heycode/config.toml` (`crates/heycode-cli/src/main.rs:373-382`) that
nothing reads back. There is no factory registry and no `Composition` type,
despite `AGENTS.md` §2 crediting heycode-config with both and §10's "new plugin" /
"new crate" recipes instructing you to register there.
**Fix:** introduce a factory registry keyed by plugin name mapping to a
constructor closure, populated by the composition root (heycode-config owns the
*type*; heycode-cli owns the *table*, because only the bin may know every crate —
§2's "libraries never reach up"). `compose_world` resolves `cfg.profile.plugins`
through it, failing loud with the available list on an unknown name. Keep the
current hardcoded order as the built-in default profile so behavior is unchanged
when no `[profile] plugins` is given.
**Test:** a config naming an unknown plugin fails composition with a message
containing the bad name and the available list; a config reordering two plugins
composes in that order; omitting `[profile]` reproduces today's exact vector.

---

## P3 — principle violations & hygiene

### [x] T11. `CallId` is both a #10 violation and #11 dead code
**Evidence:** defined and exported at `crates/heycode-core/src/id.rs:45` with **zero
uses outside heycode-core**. Every boundary carrying a tool-call id uses a bare
`String`: `crates/heycode-session/src/event.rs:45,142,153`,
`projection.rs:44`, `crates/heycode-llm/src/vocab.rs:42,95`,
`crates/heycode-agent/src/mapping.rs:31`. Principle #10 mandates newtypes; #11
forbids dead code — it currently fails both.
**Fix:** adopt `CallId` across the internal boundaries (session events, tools
`ToolCallInput`, agent mapping). **Leave `heycode-llm::vocab` as `String`** — that
is the wire boundary, and principle #12 says validate at boundaries, trust types
internally; `mapping.rs` is the single validation point.
**Test:** serde round-trip of a `tool/call` event fixture still produces the
identical JSONL line (the newtype must be `#[serde(transparent)]`), and an
existing on-disk log still replays.

### [x] T12. `Context::shutdown()` is never called
**Evidence:** no run mode calls it; `main.rs` exits via `std::process::exit`, so
the LIFO disposers principle #2 is built around never unwind. MCP child
processes are killed by an effect that therefore never runs.
**Fix:** unwind before exit in every run mode (TUI, headless, ACP, setup).
**Test:** register a plugin whose disposer sets a flag; run a headless turn end
to end; assert the flag is set after the run returns.

### [x] T13. Dead knobs that look live
**Evidence:** `WorldOptions::cwd` is never read by `compose_world`
(`AgentOptions.cwd` is hardcoded `None`); ACP `session/new` reads `params.cwd`
then discards it (`crates/heycode-cli/src/acp.rs:182`).
**Fix:** thread both into `AgentOptions.cwd`, or delete the fields. Prefer
threading — an ACP client setting a workspace root and being silently ignored is
a real bug.
**Test:** a headless run with an explicit cwd resolves a relative `read` against
it; an ACP `session/new` with `cwd` makes tools resolve there.

### [x] T9. Stale `#[allow(dead_code)]`
**Evidence:** `crates/heycode-cli/src/acp.rs:57` on `request_client`, which IS called
at `:205`; nine Landlock constants at `landlock.rs:45-61` are likewise live.
**Fix:** remove the attributes; let clippy prove they are reachable.
**Test:** the clippy gate.

### [x] T10. Unused declared dependencies
**Evidence:** `unicode-width` (heycode-tools); `uuid`, `tempfile` (heycode-llm);
`tokio`, `dirs` (heycode-session — the `tokio` dep is a leftover from the Mutex
migration `GOTCHAS` #2 records); `heycode-core` (heycode-config, unused in any
signature). Also `[workspace.dependencies]` declares 11 internal crates but omits
`heycode-cli` despite it shipping a lib target.
**Fix:** drop the unused, add the missing workspace entry.
**Test:** `cargo build --workspace` still green; `cargo tree -i <crate>` no longer
lists the removed edges.

### [x] T17. `edit` gating is path-only
**Evidence:** `ObservationLog` records canonicalized read paths but no mtime or
hash, so a stale read still authorizes an edit; `write` marks the path observed
*after* writing, so create-then-overwrite needs no read at all.
**Fix:** record `(path, mtime, len)` at read time and re-check at edit time;
reject with a message telling the model to re-read. Keep `write`'s
create-then-overwrite path honest.
**Test:** read a file, mutate it out of band, attempt an edit → refused with a
re-read instruction; read-then-edit with no external change → allowed.

---

## P4 — documentation reconciliation

### [x] T16. Bring the docs back in line with the code
Per `AGENTS.md` §0, this lands in the SAME change as the code above.

**`AGENTS.md`**
- §2 table has 9 rows for 12 crates — add `heycode-sandbox`, `heycode-mcp`,
  `heycode-skills` with their allowed arrows. Ratify the two real-but-unauthorized
  edges: `heycode-tui → heycode-llm` (`Cargo.toml:10`, used at `app.rs:10` to downcast
  `anyhow::Error` to `LlmError`) and `heycode-agent` dev-dep on `heycode-sandbox`
  (`tests/sandbox.rs:34`).
- §3 service-key table, five corrections: `"session"` is
  `Arc<std::sync::Mutex<Session>>` **not** tokio's (`GOTCHAS` #2 records why —
  and `ctx.get::<T>` matches the concrete type exactly, so this lie returns
  `None` rather than failing to compile); `"approval"` is the `ApprovalHandle`
  newtype, and the ask-mode replacement lives in heycode-agent, not the TUI;
  delete the `"web"` row (web tools are `register_shared` into `"tools"` —
  belongs in §6); collapse the two contradictory `"mcp"` rows into one non-key
  note; add the missing `"tui"` row.
- §3 line 103: strike "Seams are synchronous" — `Layer::handle`, `Next::run` and
  `Waterfall::run` are all `async` (`crates/heycode-core/src/events.rs:62-145`) and
  human-parking approval dialogs depend on it.
- §3: delete the `"seam/request"` / `LlmRequestDraft` row — zero hits in the
  tree. §2 line 42 names the seam `"tools/pre"`; the constant is
  `SEAM_PRE_TOOL = "seam/pre_tool"` (`exec.rs:9`).
- §5: OpenRouter's "model id string from config" is really a hardcoded silent
  fallback `DEFAULT_MODEL = "openai/gpt-4o-mini"` (`openrouter.rs:28`) — document
  it or remove the fallback and fail loud (prefer failing loud, principle #4).
  `http_referer`/`x_title` are env vars `HEYCODE_HTTP_REFERER`/`HEYCODE_X_TITLE`
  (`openrouter.rs:12-14,85`), not config keys.
- §5: "exactly one `Usage`" is not enforced — ordering is, count is not
  (`FakeProvider` emits a bare `Finish`). The parser's own doc says "at most
  one". Align the law to "at most one, immediately before `Finish`".
- §6: seven built-ins → **nine** (`web_fetch`/`web_search` register when
  `web_enabled`, which defaults **true** at `crates/heycode-tools/src/config.rs:28`).
  Fix `crates/heycode-tools/src/lib.rs`'s own "the seven built-in tools" doc comment
  too. Note the model actually sees 13 in a default world once `load_skill`,
  `task`, `send_message`, `list_tasks`, `interrupt_task` are counted.
- §7: five documented sections → **ten**. `[approval] [compaction] [subagent]
  [web] [mcp.servers]` are undocumented. Drop "(tier-2)" from `[sandbox]` (§11
  already calls it DONE). `[ui] theme` does not exist; `[ui] auto_title` does.
- §7: `--profile <name>` → `~/.heycode/profiles/<name>.toml` is **not implemented**
  (no branch in `main.rs:107-140`, `grep -rn profiles crates/` is empty), and
  `HEYCODE_PROVIDER` / `HEYCODE_MODEL` do not exist anywhere. Either implement or
  strike. Document the env vars that DO gate behavior: `HEYCODE_FAKE`,
  `HEYCODE_HOME`, `HEYCODE_ACP_APPROVAL=ask` (`main.rs:472`), `BRAVE_API_KEY`,
  `HEYCODE_HTTP_REFERER`, `HEYCODE_X_TITLE`.
- §8: `docs/ui-language.md` **does not exist** (`docs/` holds only GOTCHAS.md and
  STATUS.md), and README says the UI language lives in AGENTS.md — the two
  disagree. Either write the file or point at §8. Also §8 vs `heycode-tui`: the
  spinner is asterisk glyphs `✢✳✶✻✽`, not braille; the permission dialog is a
  fixed 6-row region carved by `render::draw`, not inline in transcript flow;
  "second Esc clears input" is not implemented.
- §10: the "new plugin" and "new crate" recipes point at a factory table that did
  not exist — rewrite against whatever T15 actually builds.
- §0: "13 indexed mistakes" → GOTCHAS.md has **15**.
- §3: the trait is written `fn inject(&self) -> &[&'static str]`; the code is
  `&'static [&'static str]`.

**`docs/STATUS.md`**
- `:3` says "213 tests" and `:14` says "226 tests / 19 suites" — ten lines apart,
  contradicting each other. Real: **226 passing**, across **24** suites reporting
  a non-zero count (27 test binaries + 12 doc-test targets). No reading yields 19.
- `:7` "~31k lines" → **17,903** lines across 79 `.rs` files (15,089 `src/`,
  2,814 `tests/`). Even counting every `.md` and `.toml` only reaches ~18.8k.
- `:7` "9 model tools" → 9 is heycode-tools' own registry; the model sees 13.
- Per-crate counts: `heycode-tools` 66 → **69**; `heycode-agent` 21 → **27**;
  `heycode-config` 7 → **8**; `heycode-tui` 14 → **18**.
- `:3` "All gates verified green" is false — fmt fails (T0).
- `:40` registry lists `sandbox` and `plan`, which T14 makes true; until then it
  was wrong.

**`README.md`**
- `:60` "213 tests" and `:107` "# 167 tests" contradict each other and reality
  (**226**). `:60` "rustfmt clean" is false until T0.
- `:50` claims four slash commands; **eight** are reachable today
  (`/help /model /provider /compact /title /quit /skills /skill`), nine after T14
  adds `/plan`.
- Layout table lists 9 crates for 12.

**`FEATURES.md`**
- Self-contradictory: marks "live meter 🔌 T2" and "spawn/fork subagents 🔌 T2"
  as unshipped, then three tables later marks subagents ✅ with three modes. The
  context meter renders at `crates/heycode-tui/src/render.rs:485-497`.

**`docs/GOTCHAS.md`**
- #1 claims "Forgetting ANY arm fails compilation". Only two of five places are
  compiler-enforced (`name()` at `event.rs:191`, `derive_messages` at
  `projection.rs:99`). `KNOWN_KINDS` (`event.rs:12`) is a plain `&[&str]` const
  and the drift fixture is a hand-written `Vec` — omit either and it compiles,
  then hard-errors at resume on your own logs. Soften the claim, or make
  `KNOWN_KINDS` derive from an exhaustive match so it IS enforced (preferred).
- #4 claims `undocumented_unsafe_blocks` is denied — it is not configured
  anywhere (harmless: `unsafe_code = "forbid"` at `Cargo.toml:59`). It omits four
  lints that ARE configured: `dbg_macro`, `unimplemented`, `print_stderr`
  (denied), `todo` (warn). And `crates/heycode-cli/src/main.rs:6` carries a
  file-wide `#![allow(clippy::print_stdout, clippy::print_stderr)]` covering 18
  print sites — defensible for a CLI, but undocumented.
- New entries earned by this backlog: the `_next` non-delegation class (T2), the
  `let _x =` discard class (T3), and `#[serde(default)]`-on-field vs section
  `Default` for the three fields #6 did not cover (T4).

---

## Execution order

T0 → T2 → T4 → T3 → T6 → T7 → T5 → T8 → T1 → T14 → T15 → T12 → T13 → T11 →
T17 → T9 → T10 → T16.

Rationale: unblock the gate first; land the guard fix before the mount that
would arm it; do cheap isolated bug fixes while the tree is quiet; take the two
architecture tasks together; hygiene last; docs in the same change as the code
they describe.
