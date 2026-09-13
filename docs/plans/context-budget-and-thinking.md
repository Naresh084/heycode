# Context budgets and expandable thinking

Internal implementation task. Scope: model-specific context limits, shared request budgeting, automatic compaction, context visibility, and individually expandable reasoning blocks.

## Tasks

- [x] Resolve active model capacity and share measured request budgets with compaction and UI; label unknown and estimated evidence.
- [x] Check complete requests before dispatch, reserve response space, compact and remeasure with bounded recovery.
- [x] Preserve context evidence across native runtime/app-server transport and model switches; expose remaining capacity and compaction threshold.
- [x] Retain muted reasoning headers, individual expansion, actual elapsed timing, full supplied text, and keyboard/mouse interaction.
- [x] Add drag/double-click copying of rendered text and a 2.5-second right-aligned copy notice above the composer.
- [x] Verify boundaries, failures, switching, streaming, replay, and terminal layout; document results.

## Contract

The footer and compactor use the same active model, measurement, and threshold. Model metadata takes precedence over the legacy assumed window; unknown capacity is explicit. A collapsed reasoning block changes only presentation. Only provider-supplied reasoning is displayed. Timings are measured, never reconstructed without evidence. Existing unrelated worktree changes are preserved.

## Behavior and controls

The footer shows context pressure, remaining input capacity after reserves, and the automatic compaction threshold. `/context` explains capacity provenance, measurement confidence, reserved output, safety allowance, and the latest compaction reduction. `compaction.context_window = 0` selects the active model's published capacity; a positive value caps known capacity or supplies an explicit fallback for unknown models. The default threshold remains 80%, bounded by available input space.

Requests are measured after preparation, including tools and provider state. Automatic compaction runs once before rebuilding and checking the request again. Requests that still exceed known capacity fail with an actionable error. Portable summaries cover the complete prefix through bounded chunks and hierarchical reduction; failed reduction leaves original history intact.

Reasoning is collapsed by default and retains its header after completion. Click a header or press Ctrl+R to toggle the focused/latest block. Alt+Up/Down focuses reasoning headers; Enter or Space toggles the focused block, and Esc returns to composing. Streaming headers show elapsed time; replay restores timing only where durable timestamps support it. Expansion displays all supplied reasoning.

Drag displayed text or double-click a word to copy it automatically. The notice `Copied N characters` appears above the input at the right for 2.5 seconds. Selection uses rendered cells, including their masking, and stays stable during streaming. Local macOS copying uses the shared subprocess service and the system pasteboard; remote sessions and other platforms use terminal OSC 52 support.

## Verification

Unit and integration coverage checks model capacities, output reserves, unknown limits, complete-request admission, bounded compaction, summary tail preservation, runtime transport, replay, reasoning interaction, Unicode selection, and notice placement/expiry. Real PTY checks use a local streaming provider fixture to verify the model-specific footer, live timing, collapsed and expanded reasoning, header clicks, native clipboard contents for drag/double-click, idle notice expiry, `/context`, and resumed budget/timing. Terminal screenshots were inspected at 110 columns by 40 rows.

Final validation: `cargo test --workspace --quiet -- --test-threads=4` passed (4205 tests). Affected-crate Clippy checks with warnings denied passed, and the final debug CLI build passed. Earlier parallel runs hit cancellation and telemetry timing failures; both passed in isolation and in the final workspace run. The real PTY flow passed again after routing clipboard execution through the shared process service.
