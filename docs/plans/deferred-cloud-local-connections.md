# Cloud and custom-server connections — separate implementation task

User decision, 2026-09-05: implement these features now in a separate task; the originating task waits and reviews the result. Keep unfinished features out of the current product UI. No mock options, disabled future-provider rows, teasers, hints or placeholder forms. Implement separately in an isolated Codex task using GPT-5.6 Sol with xhigh reasoning. Do not merge or publish that work into the primary checkout automatically.

## Sequential phases

Finish and record evidence for one phase before starting the next. Read AGENTS.md, STATUS.md and relevant GOTCHAS first. The primary checkout has substantial uncommitted implementation: treat it as read-only reference and reconcile the necessary baseline into the isolated worktree before implementation. Never reset, overwrite or commit the primary checkout.

1. **Amazon Bedrock.** Provider-owned region form, exact draft-region credential validation and model discovery, atomic route/region/reference persistence, startup recovery and a fixture-backed production composition/PTY check. Existing saved-coordinate plumbing is the starting point. Align catalog, authorization, inference and status; never mutate process environment. Do not add SageMaker or unrelated AWS products.
2. **Google Vertex.** Provider-owned project/location form and real OAuth/ADC/token readiness. Preserve exact model adapter admission. Account, project and location are independent facts. Explain external prerequisites honestly; do not present cloud credentials as generic API keys or invent token minting.
3. **Azure.** Verify official current authentication, resource/deployment configuration and inference protocol. Implement the real provider/adapter before adding its picker entry or forms. Persist non-secret coordinates and credential references atomically. No catalog-only claim of working inference.
4. **Custom local servers.** Add an exact supported protocol, URL, optional key and explicit/discovered model flow. Unknown model capabilities remain unknown; do not promote generic OpenAI-compatible listings to tool support. Keep LM Studio and Ollama behavior intact. No runtime-management buttons without an actual reviewed API.
5. **Final integration.** Audit production-reachable UI for mock/placeholder/unfinished claims, run required quality gates and focused PTY flows, update STATUS/GOTCHAS/READMEs, and give one completion report with observed evidence, remaining limitations and a reviewable branch or patch. Keep live account checks unobserved where no account exists. Never read or expose credential values in diagnostics.

## Stop rules

Use a small written acceptance checklist for each phase. Passing checks are not rerun without a code change, failure or unresolved concern. A failed approach gets at most two materially different attempts; then document the concrete blocker and continue independent work if available. Do not loop on unavailable accounts, repeated unchanged polling, non-compiling mutations or unrelated refactors. Do not ask the user for secrets. Report required user action once, rather than repeatedly retrying it. Do not expand this backlog into all cloud vendors or all local servers.

The work is complete. The separate task proceeded from one completed phase to the next without asking for confirmation. Completion evidence below covers both behavior and production reachability and distinguishes fixture-tested implementation from live observations. The originating task can now review the branch and notify the user.

## Phase evidence

### 1. Amazon Bedrock — complete in `codex/cloud-local-connections`

- [x] The shared provider picker includes the implemented Amazon Bedrock profile.
- [x] Bedrock owns one required `region` field and no fabricated default model.
- [x] Fixture discovery validates the exact draft region, sends an explicitly entered key only as a bearer header, rejects missing/extra/malformed coordinates before HTTP and does not publish a cached generation.
- [x] Model selection stages region/model/reference through one expected-revision routing transaction; startup restores the tuple without changing process environment.
- [x] A production-loader composition with a saved `ap-southeast-2` route observes the same custom credential reference in AWS authorization, the connection-origin region in status, the Bedrock catalog contribution and the Converse inference provider.
- [x] A production-binary OS pseudo-terminal run drives arrow-key selection and bracketed paste from Welcome into the Bedrock region form; screen-reader and full-screen reducer tests cover field metadata/value, clear, selection and confirmation.
- [x] `cargo fmt --check` and affected all-target Clippy pass. The affected-crate test run passed all behavior tests; its two expected exact-inventory/journey snapshot drifts were updated and their focused reruns pass.

No live AWS account or credential was used. The live Bedrock canary remains skipped unless its existing explicit environment gates are supplied, so this phase proves fixture-backed behavior and production reachability, not a live account observation. The phase-specific TUI and CLI totals are now 233 and 194 tests respectively.

### 2. Google Vertex — complete in `codex/cloud-local-connections`

- [x] The shared provider picker contains Google Vertex AI beside Bedrock; the provider-owned form collects only `project` and `location`.
- [x] Vertex explicitly disables generic masked credential entry. Its help names ADC, billing, the Vertex AI API, the Vertex AI User role and the externally supplied `cloud-platform` OAuth token; heycode makes no token-minting claim.
- [x] Draft validation requires the exact two-coordinate map before account, credential or readiness work, preserves independent ADC/project/location states, and resolves the OAuth token at operation time.
- [x] The readiness probe sends one bounded bodyless bearer-authenticated `GET ...:fetchPublisherModelConfig`; deterministic fixtures cover success, 403, 503, invalid JSON shape, timeout, cancellation, absent ADC and absent token without rendering response or secret bodies.
- [x] Ordinary catalog refresh remains credential-blind maintained metadata. Draft probing is uncached, and both inactive picker composition and the active lazy inference route own the same readiness-aware source without a duplicate registration.
- [x] Exact model admission remains Gemini 3.7 Flash. One routing transaction and startup recovery retain project/location/model/reference; a production-loader composition observes the Vertex catalog, lazy inference adapter, Agent route and custom OAuth reference together.
- [x] A compiled production binary was driven through Welcome → Cloud → Vertex in a real OS pseudo-terminal. Project and location accepted bracketed paste, only the discovery action was present, and the process was stopped before the probe; no credential or network was used.
- [x] `cargo fmt --check`, affected all-target Clippy and all affected crate tests pass. A compiling mutation that promoted undetermined ADC state was killed, the exact source restored, and the focused regression passed again.

No live Google Cloud account, ADC document or OAuth token was used. The authenticated result is fixture-backed protocol behavior plus production reachability, not an observed account entitlement or live Gemini turn. The phase-specific provider-Google, TUI and CLI totals are now 149, 234 and 198 tests respectively.

### 3. Microsoft Azure OpenAI — complete in `codex/cloud-local-connections`

- [x] Current Microsoft contracts were checked before implementation: the GA Azure OpenAI v1 resource base is `https://<resource>.openai.azure.com/openai/v1`, request `model` is the deployment name, API-key authentication uses the `api-key` header and v1 has no `api-version` query parameter.
- [x] The shared provider picker includes Microsoft Azure OpenAI. Its provider-owned form requires exactly validated `resource` and `deployment` coordinates, has no fabricated default model and names the default API-key reference without retaining a value.
- [x] Draft readiness validates the exact coordinate map before credential or network work, resolves the masked draft or configured key per operation and sends one bounded bodyless `GET /openai/v1/models/<deployment>`. Only the exact returned model object is admitted; all unsupported capability claims remain Unknown and draft results never enter the active cache.
- [x] The production provider uses the shared strict Responses protocol with an explicit Azure `api-key` header mode, the deployment in `model`, operation-time credential rotation and pre-I/O rejection of any other model. Microsoft Entra ID is explicitly not implemented because no owned token refresh provider exists yet.
- [x] One routing transaction and startup recovery retain resource/deployment/model/reference atomically. A production-loader composition observes the Azure catalog, exact inference adapter, Agent route and custom reference together.
- [x] Catalog and inference plugins declare exact inventory rows and remove them on Context shutdown. The default production inventory includes the setup-safe catalog; the inference row is conditional on a complete selected Azure route.
- [x] A compiled production binary was driven through Welcome → Cloud → Microsoft Azure OpenAI in a real OS pseudo-terminal. Resource and deployment accepted bracketed paste, and the process was stopped before final confirmation; no credential or network was used. The existing Bedrock and Vertex PTY paths were adjusted to the new sorted provider list and still pass.
- [x] `cargo fmt --all --check`, affected all-target Clippy, generated documentation checks and all affected tests pass. A compiling mutation that changed the Azure header back to bearer authorization was killed by the exact wire regression; the source was restored and that test passed again.

No live Azure account, API key or Entra identity was used. The 11-test Azure provider suite proves fixture-backed request/response behavior, production composition proves the route is reachable, and the 202-test CLI suite proves all three cloud PTY paths plus exact inventory. Live deployment entitlement and an actual inference turn remain unobserved.

### 4. Custom OpenAI-compatible servers — complete in `codex/cloud-local-connections`

- [x] The reachable local hierarchy keeps LM Studio and Ollama intact and adds one custom OpenAI-compatible server. Its provider-owned profile has no fabricated URL, model or credential default and advertises only Chat Completions.
- [x] The version-root boundary accepts only bounded credential-free absolute HTTP(S) URLs with no query or fragment, normalizes trailing slashes, and appends exactly `GET /models` and `POST /chat/completions`.
- [x] Canonical model discovery is one bounded bodyless request with optional bearer authentication. The whole generation rejects malformed, duplicate or oversized rows, and every listed model capability remains Unknown.
- [x] Setup can select a discovered id or retain one validated explicit id when canonical discovery is unavailable. Authorization and invalid-response failures do not bypass validation; the explicit fallback does not mint capability evidence.
- [x] The shared Chat adapter now has an explicit unauthenticated binding that sends no authorization header. A selected optional key remains a reference resolved once per operation; fixture inference proves no-auth, bearer rotation, exact selected-model admission and the exact `/chat/completions` route.
- [x] One routing transaction and startup recovery retain URL, model and optional reference together. A selected optional reference must still resolve at startup and never silently falls back to no auth. Production composition exposes the setup-safe catalog by default and conditionally publishes the inference provider only for a complete selected route, with effect-owned teardown.
- [x] A compiled production binary was driven through Welcome → Local model → Custom OpenAI-compatible server in a real OS pseudo-terminal. The URL form exposes discovery, the optional masked bearer action and exact Chat route; it was stopped before confirmation, so no credential or network was used.
- [x] `cargo fmt --all --check`, affected all-target Clippy, generated-reference checks and all affected crate tests pass. A compiling mutation that promoted tools from Unknown to Supported was killed by the catalog regression; exact source restoration and the regression rerun pass.

No live custom server or credential was used. The 12-test provider suite proves fixture-backed catalog/inference behavior, while the 235-test TUI and 207-test CLI suites prove the new path is production-reachable without regressing the existing local/cloud flows. No runtime-management control was added. Live server dialect quirks and an actual inference turn remain unobserved.

### 5. Final integration — complete in `codex/cloud-local-connections`

- [x] Production-reachable connection copy and selectors were audited for mock, placeholder, teaser and unfinished claims. The one surviving Azure future-auth sentence was removed; documentation retains the limitation without advertising an unavailable choice in setup.
- [x] The default world contains 62 crates and 120 Unix plugins with exact setup-safe catalog inventory. No new service key or session event kind was introduced.
- [x] All four compiled production-binary PTY flows pass together: Bedrock region, Vertex project/location, Azure resource/deployment and custom Chat URL/optional-key setup. Each run stops before credential lookup or network I/O.
- [x] `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings` and `cargo test --workspace --no-fail-fast` pass: 4,049 unit/integration tests plus 7 doctests, 4,056 total, zero failed or ignored. An isolated restricted-workspace `--fake` run also settles with the expected offline reply and stop reason.
- [x] All four generated references are fresh; 55 Markdown links, 18 shell examples with one deterministic execution, and three accessible diagrams pass the documentation verifier.
- [x] AGENTS, STATUS, GOTCHAS, root/provider READMEs, feature parity and the phased evidence record describe the final implemented boundary and its limitations.

No cloud or custom-server live account check was attempted because no account or endpoint was supplied. The implementation evidence is deterministic fixtures plus production composition and PTY reachability. Live entitlement, provider-specific compatible-server quirks and actual cloud/custom inference turns remain unobserved. The reviewable branch is `codex/cloud-local-connections`; it has not been merged or published into the primary checkout.


## Primary-checkout review corrections — 2026-09-05

The user requested direct fixes after review. The primary checkout now preserves exactly three welcome choices: subscription, local model and provider. Cloud entries share the provider picker; the internal cloud category does not add a fourth welcome row. The production PTY paths were updated to search this shared picker.

Review also reproduced a pre-HTTP `Unproven(Tools)` error on Azure and custom Chat: normal Agent requests include tools, while the original inference fixtures used empty tool lists. Both providers now explicitly opt into attempting unknown ordinary function tools. No model or catalog fact is promoted to Supported; known Unsupported remains a local refusal, unknown reasoning remains rejected, and the default shared adapters remain strict. AGENTS.md records the narrow admission exception in the inference-resolution contract, with GOTCHAS #352 explaining the evidence distinction.

The Azure/custom inference regressions now send real tool schemas and retain exact route/authentication/credential-rotation checks. Four real-composition Agent tests execute a workspace read and replay its logged result on each protocol, or observe an endpoint rejection with one logged request and no tool-free fallback. The complete provider suites pass 12 Azure and 13 custom tests. A compiling mutation that also admitted Unsupported was killed by both provider regressions and restored byte-for-byte before the passing rerun.

These corrections remain fixture-backed. A live Azure deployment or custom server must support function tools for a coding turn to succeed; neither identity discovery nor an attempted request establishes universal capability support.


Final primary-checkout verification: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`, and
`cargo test --workspace --no-fail-fast` pass. The 197 test suites report
4,055 unit/integration tests plus 7 doctests: 4,062 passed, zero failed or ignored.
Documentation verification reports four fresh references, 55 valid links,
18 valid shell examples with one executed, and three accessible diagrams.
No service key or session event kind changed in these review corrections.
