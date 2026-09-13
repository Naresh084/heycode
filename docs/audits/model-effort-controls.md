# Model and effort controls

Native and delegated model selection now accept one choice containing a model, optional effort and persistence scope. The choice carries its catalog, backend owner and routing revision. The TUI applies the displayed effort for the highlighted model in the same operation as the model. Enter saves a default; `s` changes the current composition without writing the settings file. Search text containing `s` cannot trigger a session selection.

The model picker loads effort choices for its highlighted model without changing the active route. A later highlight, owner or routing revision rejects old metadata. Escape discards the preview. Native settings publication uses one revision-checked commit; delegated controls apply one combined configuration, verify both effective values, then publish one settings revision. A stale or failed delegated application retires the exact affected backend generation according to the existing routing contract.

## Verification

- Routing tests (local-only evidence: `tmp/verification-logs/20260912T024423Z-routing.log`): 33 passed. Combined native and delegated choices, unsupported values, stale revisions, one delegated configure call, unchanged files for session scope and independent saved defaults are covered.
- Terminal and theme tests (local-only evidence: `tmp/verification-logs/20260912T024728Z-terminal-theme.log`): 167 TUI unit, 379 TUI integration/standalone and 62 UI tests passed; two existing tests remain ignored. New tests cover one scoped model/effort choice, accepting the displayed effort without arrow adjustment, Escape, and rejecting an earlier highlighted model's metadata.
- Actual CLI model/effort journey (local-only evidence: `tmp/terminal-checks/20260912T030523Z-model-effort-scope/result.json`): all 11 assertions and 15 terminal captures passed against binary `5c87abbcd6a2e44089237754fc6254d3ad59737a24cb76edfd926663d0696487`. The journey verifies session-only model and effort changes, exact saved file preservation, Enter persistence, restart restoration, search input and preview cancellation. It made zero inference requests.
- Strict lint and build (local-only evidence: `tmp/verification-logs/20260912T030350Z-lint.log`) passed for the integrated source.

## Per-model OpenRouter effort vocabulary

The normalized model descriptor now retains the reasoning evidence its source published: the effort ids in published order, the published default, `default_enabled` and `mandatory`. The catalog rejects a generation whose block is malformed, duplicated, or names a default outside its own vocabulary, and the file catalog round-trips the retained evidence so a cached generation restores the same values.

The OpenRouter route resolves effort choices from that per-model evidence. A model that published a vocabulary offers exactly it; the verified `z-ai/glm-5.3-flash` route keeps the `max/high/low` list this adapter proved for it, including when no catalog row is available; a reasoning-capable model that published no vocabulary offers no effort control instead of the guessed static list. The same per-model list validates request resolution, `/effort`, and the combined model/effort picker, so an effort belonging to another model is refused before any request is built. A saved effort a model no longer publishes is applied as unset with an explicit receipt, and the settings file is left exactly as written.

- Strict lint and focused tests (local-only evidence: `tmp/verification-logs/20260912T042820Z-openrouter-model-effort.log`): clippy clean on the three crates; 359 dshx-llm integration, 54 OpenRouter and 15 routing tests passed, plus 35 routing CLI tests. `cargo fmt --all -- --check` reports only concurrently edited `crates/heycode-tui` and `crates/heycode-status` files and none of the files changed here.
- Actual per-model picker journey (local-only evidence: `tmp/terminal-checks/20260912T042039Z-openrouter-model-effort/result.json`): all 13 assertions and 18 captures passed against binary `87d9f3745736fa2fd655866c11d667cfa8f356bda0388111cc8a537c2ae8e10e`, with zero inference requests. A fixture model publishing `low/medium/high` shows `medium effort (default)` and never `max`; a reasoning model publishing no vocabulary shows `Effort unavailable for this model` while still reporting reasoning support.

## Remaining boundaries

Live upstream acceptance per model is still unproven. Every check here is a loopback fixture or a recorded catalog shape; nothing establishes that OpenRouter or an upstream provider accepts a given published effort id on a real request, because that needs a paid call. `reasoning.effort` is sent verbatim as the published id inside `{"reasoning":{"effort":...}}`, so an unusual published value (`xhigh`, `minimal`, `none`) reaches the wire unaltered and untested.

The retained `default_enabled` and `mandatory` flags are evidence only: no control yet turns reasoning off for a model that publishes `mandatory: false`, and no control refuses to disable it for one that publishes `mandatory: true`. Providers other than OpenRouter keep their route-wide configured vocabulary, which remains unverified per model.

The refreshed layout is paired with the [current reference baseline](terminal-reference-baseline.md). Shared idle footer, command-result presentation and additional viewport/failure comparisons remain open. These tests do not close the full `/model` or `/effort` parity rows.
