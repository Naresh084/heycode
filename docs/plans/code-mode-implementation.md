# Native programmatic tool execution

`run_code` executes an async JavaScript function body in a fresh QuickJS VM. Use `tools[name](args)`, `text(value)`, `console.log(...)`, and `return`. Normal variables, loops, branches and `Promise.all` work. `tool_search` returns exact schemas from the current agent's tool registry.

The VM exposes no process, filesystem, network, import loader or timers. Defaults are a 60 second deadline, 32 MiB heap, 64 KiB source/output, 1 MiB per tool argument/result, 64 calls and four concurrent calls. Hard maxima are enforced even for caller overrides. Read-only calls may overlap; mutating calls retain a barrier. Every nested call rechecks current approval and pre-tool policy. The captured execution context belongs to the actual child or parent dispatch, so a child cannot borrow root tools or permissions. Script results have a durable untrusted-data boundary.

Actions `save` and `run_saved` manage named session-local scripts; `list` and `status` inspect definitions and run evidence. A run freezes its source and selected tool names. Save does not execute. Running a saved definition requires current tool availability and permissions. Recursive `run_code` is refused.

`code-mode/change` is a v2 session event, independent from provider `tool/call` pairing. Run start and each call's intent commit before execution, and every completed call has a result or explicit failure. Missing settlements after a process crash remain unknown. Scripts never automatically replay after interruption. A failed script can have successful earlier effects: inspect its run id before retrying.

Validation: eight real VM tests cover branching, parallel results, missing ambient APIs, direct bridge allowlist enforcement, denials, memory/output/call caps, CPU/promise deadlines and cancellation. Three composed Agent tests execute nested tools after durable intent, preserve provider pairing and untrusted boundaries, reject a denied inner call despite outer approval, and save/reuse a real script. Two session tests cover crash replay and refused invalid settlements. Shipping composition mounts `code-mode` by default.

Integrated checks now cover shared native write/edit checkpoints, child-owned asynchronous questions, inherited tool/approval authority, background execution and live script controls. The complete workspace rerun remains the final gate.

Live runs also expose owner-scoped `/scripts`, `/scripts pause <run-id>`, `/scripts resume <run-id>` and `/scripts stop <run-id>`, plus equivalent tool actions. Pause blocks new tool calls after already admitted calls settle and retains the live VM; wall-time limits continue. Stop cancels that run without canceling its containing conversation. Completed/crashed runs cannot be resumed or replayed by these controls. Plugin shutdown cancels owned runs, and `run_code` explicitly supports the bounded background execution service.

The composed live-control regression holds a real script inside its first tool, pauses before the second call, proves a different session cannot stop it, then resumes with JavaScript local state retained. A second path stops while paused, proves the second effect never occurs and confirms a durable error settlement.
