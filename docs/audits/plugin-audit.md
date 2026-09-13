# Phase 2 plugin command completion audit

Date: 2026-09-12 (Australia/Melbourne)

## Verdict

`P2-C-reload-plugins` remains complete for its accepted local command/UI contract.
`P2-C-plugin` remains in validation: the earlier completion verdict was reopened
after direct comparison exposed a different Installed-list/detail layout.

The corrected production view is captured in direct-plugin-20260912T011727Z-135b4f (local-only evidence: `tmp/terminal-evidence/direct-plugin-20260912T011727Z-135b4f/result.json`),
using immutable SHA256 `a457993754f3dca9cb9b9bd3f9cee6de45ca2f0d1f9dae54d3ec7a9162999726`.
Eight captures cover the compact Installed list, separate selected detail, provenance,
permissions, filtering, and narrow list/detail states with zero inference. Root
inspected these against actual Claude 2.1.269
installed-package captures (local-only evidence: `tmp/terminal-evidence/plugin-installed-claude-reference-20260911T200207Z-572831/result.json`).
Existing lifecycle tests remain applicable; remote marketplace capabilities are
not inferred from this local Installed view. The remaining per-state comparison
is tracked separately from functional lifecycle validation.

`/plugin` resolves through the central alias table to canonical `/plugins` and
inherits its immediate timing. With no argument it opens the installed-package
lifecycle panel; `/plugins verbose` remains the exact attributed composition
inventory. The panel operates on the shared verified package lifecycle rather
than an independent UI list. It exposes install, enable/disable, update,
rollback, and remove operations, plus manifest provenance, requested
permissions, credential requirements, cache versions, availability reasons,
validation, and cancel-default removal.

`/reload-plugins` is a separate queued command. It acquires the Agent's
quiescent recomposition permit, closes admission, reopens the same durable
session with a newly composed plugin world, and never becomes a model prompt.
It is dynamically unavailable without an attached terminal. A failed
post-shutdown activation does not invent an in-memory rollback: it exits with
one explicit error naming the still-saved session so the user can repair the
configuration and resume it.

Claude's Discover/Marketplaces surfaces depend on its own remote marketplace
ecosystem. dshx does not claim that external catalog or silently contact it;
the accepted local contract is verified installed-package management over its
own package store/cache. Live remote marketplace acquisition remains an
integration boundary, not an advertised result of these command checks.

## Current paired terminal evidence

The earlier functional native result is
`plugin-skills-ui-states-20260911T191337Z-827e49/result.json` (local-only evidence: `tmp/terminal-evidence/plugin-skills-ui-states-20260911T191337Z-827e49/result.json`).
It used immutable dshx SHA256
`14fc1b07ac6415a36d863dc4a436b0e2c8a9b26e1880ab98db6130d14f6e7f3d`,
two declarative packages installed under a disposable `DSHX_HOME`, an isolated
workspace, and a localhost catalog whose inference endpoint rejects every
request. All 13 terminal captures and eight assertions passed with zero
provider requests. Those assertions did not establish every visual state:

- the typed `/plugin` alias exposes canonical `/plugins [verbose]` and
  `/reload-plugins` in the real command menu;
- the lifecycle panel renders both installed packages, exact source/revision,
  checksum/signature state, declared contributions, permissions, credential
  reference, enabled state, cached versions, and rollback availability;
- keyboard tab/action navigation, a real mouse-wheel selection, the 60x30
  layout, a concrete unavailable-action result, invalid-version validation,
  and Escape cancellation are retained;
- `/plugins verbose` renders the live attributed composition inventory; and
- contributed skills use their manifest-backed invocation identities
  (`acme/alpha::review`, `acme/visibility::review`) with live-registry source.

The current Claude Code 2.1.268 source is
`plugin-claude-reference-20260911T191446Z-281333/result.json` (local-only evidence: `tmp/terminal-evidence/plugin-claude-reference-20260911T191446Z-281333/result.json`).
It ran with a disposable home/config/workspace, bare/restricted/strict-MCP
settings, nonessential traffic disabled, and HTTP(S) proxy routes pointed at a
closed loopback endpoint. No conversation/model prompt was submitted. Six
captures establish the source command menu, Discover/Installed/Marketplaces/
Errors topology, empty offline states, `/reload-plugins` discovery, and the
completed local receipt:

`Reloaded: 0 plugins · 0 skills · 6 agents · 0 hooks · 0 plugin MCP servers · 0 plugin LSP servers`.

The topology is used as the source comparison, not as evidence that dshx owns
Claude's remote marketplace or bundled-agent counts.

## Reload lifecycle and regressions

`recomposition-plugin-20260911T191706Z-9149c6/result.json` (local-only evidence: `tmp/terminal-evidence/recomposition-plugin-20260911T191706Z-9149c6/result.json`)
passes the existing actual-CLI success-and-failure journey again on the same
immutable native binary. A successful reload performs the real alternate-screen
teardown/re-entry and remains usable; the session path, original journal prefix,
and title are unchanged. A second reload after deliberately malformed durable
configuration exits with exactly one activation error, preserves the complete
session, and emits no `user/message` or `request/header`. The journey recorded
zero model requests.

`tmp/terminal-evidence/plugin-focused-tests-20260911T191540Z-4c03b2.log` is a fresh current-source
run: one TUI plugin unit test and 29 plugin/panel integration tests passed. The
tests cover exact operation ordering, every lifecycle operation, fresh
store-backed row publication after commit, enable/disable state, cancel-default
remove, bounded long-list rendering, malformed forms, unavailable actions,
permission/provenance honesty, narrow mouse/paste ownership, contribution
registration/disposal, and `/plugins` panel versus verbose routing.

Applicable command states are therefore covered: discovery/alias, empty source
panel, installed rows, narrow layout, keyboard/pointer navigation, forms,
successful lifecycle dispatch in the shared owner, explicit refusal and
validation failure, cancellation, verbose output, reload success, and reload
activation failure. Generic model working/streaming, grouped tool-card,
expanded transcript, and long model-output states do not apply to these local
control commands.

