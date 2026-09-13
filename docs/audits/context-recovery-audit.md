# Phase 2 context configuration and recovery completion audit

Date: 11 September 2026

Scope: bounded controlled verification for P2-CX03 and P2-CX05. This audit covers durable request-configuration identity, model-input projection, reopen, retry/fallback boundaries, and original-event recovery around compaction. It makes no native-provider cache-hit, Claude-runtime parity, live-provider quality, or cost claim.

## Outcome

The owned session-domain path has no discovered production defect. A missing combined regression was added as `it::context_recovery_completion::configuration_and_prefix_boundaries_survive_resume_fallback_and_compaction`.

The regression proves all of the following in one physical JSONL session:

- Reopen retains the same configuration revision and SHA-256 identity when only conversation content has appended.
- A retry policy is part of the options fingerprint. A changed output option advances only the options component; a changed canonical tool catalog advances only tools; model and fallback-provider changes advance only route.
- A no-output primary request and its same-turn fallback request reconstruct identical conversation inputs, contain the initiating user message once, and have distinct route/configuration identities. The separate production-composition fixture drives the actual failure and fallback effect.
- Same-route opaque provider state replaces its neutral assistant copy once. After a model/provider change, incompatible provider state is excluded and the neutral assistant copy is used once.
- Portable compaction preserves every original event and value in the reopened archive while the next model projection contains exactly the compacted summary and post-compaction user message. It contains neither the shadowed original messages nor shadowed provider state, so original and compacted context are not sent together.
- Configuration revision 5 remains stable after compaction even though earlier request headers are no longer model-visible; configuration lineage remains derived from durable history, not the compacted presentation.

This complements rather than duplicates the existing 1,000-turn stress, native-compaction route-isolation tests, request-snapshot lineage validation, and production-composition retry/fallback fixtures.

## Existing coverage audited

| Boundary | Existing controlled evidence | Audit result |
|---|---|---|
| Stable request configuration | `request_configuration_tracks_exact_components_and_survives_reopen` verifies component fingerprints, unchanged revisions, route/tool changes, reopen, legacy headers, tamper refusal, and revision-regression refusal. | Passed in this workstream. |
| Transport retry | `production_loader_retries_503_and_replays_durable_deepseek_state` drives two transport attempts for the first logical request, then one continuation request; it asserts two projected requests total, correlated provider output, same-route continuation state, and exact reopen projection. | Passed in this workstream. No network call is made by this fixture. |
| Explicit fallback | `configured_fallback_persists_route_and_continues_same_turn_once` drives primary then fallback in one turn and asserts one durable user message, two step settlements, one fallback application, and published route selection. Neighboring tests refuse fallback after partial output, unsafe replay, explicit model pins, unavailable targets, and repeated fallback. | The target path passed in this workstream. No network call is made by this fixture. |
| Protocol/state replay | Request-projection tests cover OpenAI Responses, Chat Completions, Anthropic Messages, Gemini, and incompatible-route neutral fallback without duplicate assistant messages. | All passed in the full session suite. These fixtures do not establish provider/runtime parity. |
| Native and portable compaction | Native-compaction tests cover exact-route replay, incompatible-route original history, boundary refusal, and later portable replacement. The 1,000-turn stress alternates native/portable compaction and compares every original event/value after reopen. | All passed in the full session suite. |
| Combined reopen/change/fallback/compaction path | No prior test combined configuration lineage with input equality at fallback and original-versus-compacted projection after reopen. | Covered by the new regression; passed. |

## Commands and exact results

```text
cargo test -p dshx-session --test main configuration_and_prefix_boundaries_survive_resume_fallback_and_compaction -- --nocapture
1 passed; 0 failed; 147 filtered out

cargo test -p dshx-session --test main request_configuration_tracks_exact_components_and_survives_reopen -- --nocapture
1 passed; 0 failed; 146 filtered out

cargo test -p dshx-session --test main one_thousand_turns_survive_repeated_portable_and_native_compaction -- --nocapture
1 passed; 0 failed; 147 filtered out

cargo test -p dshx-session
75 unit passed; 148 integration passed; 0 failed; 0 ignored

cargo test -p dshx-cli --test main production_loader_retries_503_and_replays_durable_deepseek_state -- --nocapture
1 passed; 0 failed; 203 filtered out

cargo test -p dshx-cli --test main configured_fallback_persists_route_and_continues_same_turn_once -- --nocapture
1 passed; 0 failed; 203 filtered out

cargo test -p dshx-cli --test main it::provider_retry_composition -- --nocapture
4 passed; 0 failed; 200 filtered out

cargo test -p dshx-cli --test main it::model_fallback -- --nocapture
7 passed; 0 failed; 197 filtered out

cargo clippy -p dshx-session --tests -- -D warnings
passed
```

The first neighboring retry-module sweep exposed a stale shared test expectation after the intentional OpenRouter caching-option addition:

```text
cargo test -p dshx-cli --test main it::provider_retry_composition -- --nocapture
3 passed; 1 failed; 200 filtered out
failure: production_loader_dispatches_verified_openrouter_policy_and_default_reasoning
crates/heycode-cli/tests/it/provider_retry_composition.rs:344
actual provider_options length: 3; stale expected length: 2
```

Production now records a third OpenRouter provider option, kind `caching`, with `{"type":"ephemeral"}`. The shared test had still assumed only the routing and transform/plugin rows. Its owner was sent the exact failure and advised to assert all three rows by kind or update the ordered expectation. The target retry proof passed in the same sweep. This workstream did not edit the shared test.

The coordinator updated that shared assertion to cover the three exact rows. The retry-composition module then passed 4/4. A transient compile interruption from concurrent TUI work also cleared before the final evidence run; the model-fallback module passed 7/7. Neither transient state is presented as a product failure.

## Completion boundary and unresolved criteria

- P2-CX03 controlled session coverage is complete for the owned boundaries. The actual-provider reusable-prefix/cache benchmark, provider usage telemetry, paid cost evidence, and any live fallback route remain root-owned and are not established here.
- P2-CX05 exact recoverability and no-double-send projection are established by controlled storage/projection tests. Whether a model retrieves the right archived fact after a lossy summary, and whether quality is preserved across a held-out task set, remain unverified. No universal quality claim is justified.
- The new combined test uses deterministic OpenAI/Anthropic-shaped provider-state fixtures. It does not prove OpenAI, Anthropic, OpenRouter, Claude Code, or Codex behavior against a live service.
- Controlled retry and fallback composition are freshly green in their complete scoped modules. Live provider/runtime behavior remains outside this result.
- No live calls, external sends, commits, pushes, resets, tracker edits, or reference-command edits were performed.
