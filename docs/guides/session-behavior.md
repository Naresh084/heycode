# Session behavior and evidence

## Command help

`/help` opens a local help panel. General shows the active route and the current
keyboard bindings. Commands lists the registered command catalog, including
aliases and unavailable-command reasons. Custom commands lists installed
package commands and skills with their actual invocation and source. Each
command tab has its own search; an empty custom inventory is shown explicitly.

Use Tab or Left/Right to switch tabs, type to search commands, and Up/Down,
Page Up/Page Down, or the mouse wheel to scroll. The tab labels also accept mouse
clicks. Escape closes help and preserves the draft. Browsing help does not submit
a command or add a model turn. The screen-reader view presents the same catalog
and controls as text.

## Context evidence and restart

`/context` identifies the current agent session and latest admitted request, including
its model route. Cache details belong to that request; missing details never inherit
cache counts from an earlier request. `/usage` remains the historical aggregate.

Projected growth estimates retained assistant text, tool arguments and tool results.
Billed output tokens and display-only reasoning do not increase that estimate.
Provider continuation and audio contributions without a count make it a lower bound.
The live display and history replay use the same retained-content calculation.

After restart, `/context` restores the latest saved request budget and subsequent
retained-content growth without making a provider call. Per-contributor token
allocations are saved with the request when available. Legacy requests without
retained allocations explicitly report them unavailable.

## Structured work inspection

The model-facing `task_get` tool returns the saved work revision and full fields,
plus `blocked_by` prerequisite IDs and a page of `blocks` dependent IDs. It shows
up to 64 dependents by default; `blocks_limit` accepts 1–200. Follow
`blocks_next_cursor` by passing it as `blocks_after`. Deleted dependents are excluded.
The list is derived from the same board snapshot as the work record and creates no
agent or job. Create/get/list/update receipts retain complete JSON and revisions
under a separate 512 KiB serialization safety ceiling; field and paging limits
remain enforced by the work service.

## Team lifecycle facts

Team snapshots include `lifecycle`: `active`, `shutting_down`, `stopped`, or
`archived`. A roster entry is retained identity, not evidence that its conversation
is running. Shutdown settles owned work before closing members; archival preserves
work and mail. Resume permits explicit admission and does not resurrect closed
workers. Bootstrap defaults to a native worker and reviewer, accepts `roles:[]`
for a lead-only team, and limits one bootstrap request to eight roles. Further
member admission follows the runtime's resource limits.

## Request configuration diagnostics

Native requests sort client tool declarations by name and canonicalize JSON object
keys while preserving arrays and all schema values. Tool schemas stay in their
protocol-defined request fields; they are not copied into conversation messages
or dropped after the first call.

`/context` and `/usage` show the latest tracked configuration revision, its SHA-256
fingerprint, system/tool component fingerprints, and changed component names.
System/guidance, tool definitions, route/binding, and request options are tracked
separately. Ordinary appended conversation does not change these configuration
fingerprints. They are stored alongside the request header, survive restart, and
are absent for older untracked records. A matching fingerprint is not evidence
that the provider served a cache hit or that the whole conversation prefix matches.

### Agent activity and foreground handoff

`agent` starts background work by default, including presets that omit a
background preference. It returns task/job IDs immediately; completion is
reported through the durable inbox. `background: false` explicitly requests a
foreground wait, and an explicit preset preference is respected.

Ordinary tools start foreground. Native background-capable tool executions
(including Bash, connected MCP tools, and explicitly foregrounded agents) and
foreground `run_tool` executions move to background after
`tools.foreground_timeout_secs` (default **120 seconds**, `0` disables automatic
handoff). The original execution and job identity continue; promotion does not
cancel or restart the operation. Use the returned job ID to inspect output or
wait before starting dependent work. Tools without background lifecycle support
retain their foreground contract.

This handoff threshold is separate from execution deadlines. Bash defaults to
**600,000 milliseconds**, configurable through `tools.bash_timeout_ms`; its
`timeout_ms` argument overrides the default for an individual command. MCP request
budgets also default to 600,000 milliseconds; explicit server timeout settings
remain authoritative. Existing explicit shorter deadlines are not extended by
promotion.

Click an agent in the bottom navigator to see its current/last tool, tool error
count, recent output and failure details. The preview follows recent activity and
shows compact tool results; `f` opens the full foreground transcript. Returning
to the parent preserves its draft and lets its other work continue.

### Attributed guidance and cache measurement

`/context` separates guidance (project instructions, skills, tool and workflow guidance, custom agent instructions, and response style) from base identity/environment instructions when the dispatched system text exactly matches its assembled provenance. These two local estimates allocate the existing system total by disjoint UTF-8 byte spans; their sum does not change the context estimate. This is heuristic attribution, not a separate provider tokenizer measurement. If a request hook replaces the system text, heycode keeps the unsplit system measurement. Both forms survive restart; older six-contributor records remain readable.

The OpenRouter Anthropic route adds ephemeral message cache breakpoints without removing tools, rewriting text, or changing the configured provider routing. Existing explicit message markers remain authoritative. Cache reads/writes shown in `/context` and `/usage` come from the response associated with that request. Missing counters are unknown; a configuration fingerprint or stable prefix does not establish a cache hit.
