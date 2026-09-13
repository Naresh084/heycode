# Workflow, execution, and context audit

## Implemented

- Removed composer cursor-line underlining and highlighted standalone workflow
  keywords in purple. Shared native/delegated instructions request workflows only
  when explicitly requested in the current turn, including `ultracode`.
- Default execution limits are unlimited: loop requests, cumulative tokens,
  elapsed time, tool calls, goal rounds/wakes, job admissions, and subagent
  request/output policy limits. Explicit positive policies remain available.
  Model context capacity, cancellation, and resource concurrency still apply.
- Successful `job_output` pages merge into their original command card by job
  ID, including durable replay. Errors and unmatched retrievals remain visible.
- Normal tool calls wait in the foreground; generic foreground jobs no longer
  also emit background completion notices. Explicit background mode and manual
  promotion retain their completion behavior.

## Verification

`cargo test --workspace --all-targets`: 4,499 passed, zero failed, nine ignored.
Final debug CLI build, clippy, formatting, and diff checks passed. A local PTY
fixture verified keyword styling without underline, approval/denial behavior,
merged retained output, and mouse expansion/collapse against the final build.

The fixture declared 300,000 input tokens per response over seven requests.
Its 2.1 million cumulative tokens are **synthetic usage metadata**, verifying
only that the former cumulative token guard does not stop execution. They are
not actual model tokens, spending, or evidence of efficient context handling.

## Real-session context findings

Read-only inspection of local session
`7246afc3-12bb-4779-920b-cc8687960e88`, turn two:

- 24 provider responses reported 1,057,837 cumulative prompt tokens and 11,495
  completion tokens. The first request reported 9,424 prompt tokens; the last
  reported 72,592. Cumulative input is not simultaneous context occupancy.
- Request headers exposed 47 tools throughout, with approximately 33 KB of
  serialized header metadata. This is not a precise wire-token measurement.
- Tool results contained 101,452 characters from reads, 42,619 from shell
  commands, 38,908 from workflows, and 17,504 from tool search. A workflow list
  alone returned 32,795 characters. Large early results remain in later inputs.
- The shell reread overlapping agent/session source excerpts. Two retained
  output requests began at offset zero; job-11's 7,816-character page shared
  approximately 6,002 exact characters with its existing inline output.
- Recorded provider state contains about 89 KB of serialized reasoning details
  over the session. These are opaque provider state, not additional visible
  assistant prose; their necessity must be checked per provider before removal.
- Estimated final context was 86,892 against a recorded 1,048,576-token model
  window. Automatic compaction was set to 838,860, so this run never compacted.
  Capacity-based compaction alone does not control irrelevant context buildup.
- The inspected request projection replaces generic assistant messages with
  matching provider state and ignores streaming chunks. It does not blindly
  replay every durable event as another message. This is code inspection, not
  an independent capture of the historical HTTP request body.

The trace shows avoidable context growth but does not establish a measured
quality decline from context rot. UI card merging does **not** deduplicate the
provider transcript. Context efficiency remains follow-up work: smaller
workflow/tool discovery results, targeted file reads, retrieval that avoids
already-seen bytes, and deliberate compaction of stale evidence while retaining
task state and recoverable source references. Unlimited execution must not be
treated as unlimited useful context.
