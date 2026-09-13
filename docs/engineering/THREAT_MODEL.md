# heycode Repository Threat Model

## Overview

heycode is a same-user, local terminal coding agent written in Rust. Its primary runtime composes plugins for configuration, credentials, authorization, model catalogs and inference, durable sessions, prompts and skills, model-callable tools, MCP child processes, subagents, sandboxing, a TUI, headless execution, and an ACP stdio server. The process can intentionally read and modify files, execute shell commands, call remote providers and web sites, and start configured MCP servers on behalf of its operator.

The most important assets are:

- API keys, home-file credential records, ambient cloud identity, and future subscription-runtime authorization.
- The operator's source tree and any files or processes reachable with the heycode process's OS privileges.
- Human intent: provider/model choice, workspace trust, approval decisions, sandbox policy, plan state, and plugin/profile selection.
- Durable session history, attachment bytes/metadata, exact rendered prompts, tool arguments/results, provider continuation state, and catalog/config/settings files. These may contain private source code or user data even when they contain no credential.
- Integrity of the plugin service graph, request-resolution/dispatch invariant, tool registry, approval waterfall, and lifecycle teardown.
- Availability and bounded resource use of the terminal process and its child processes.

Primary product/runtime code is under `crates/`. `docs/`, tests, fixtures, and local development scripts are evidence and build inputs, but are not independent deployed services. The project does not currently expose an authenticated internet server or a multi-tenant data plane. ACP is newline-delimited JSON-RPC over process stdio and assumes the launching client is operating with the same local user's authority.

## Threat Model, Trust Boundaries, and Assumptions

### Actors and input ownership

- **Operator-controlled:** CLI flags, explicit/home configuration, TUI choices, pasted credentials, approval answers, selected workspace, locally installed binaries, and intentional MCP/provider endpoints.
- **Attacker-controlled or potentially hostile:** cloned repository files; project `heycode.toml`; project `.heycode/.agents` skills and instructions; user/provider/MCP-supplied attachment bytes, MIME claims and names; model/provider stream bytes and tool calls; web pages, redirects, DNS answers, search results, and response headers/bodies; MCP server descriptions, schemas, results, JSON-RPC frames, and process behavior; resumed session/config/cache bytes; ACP client frames and supplied cwd; filenames, symlinks, hard links and concurrent filesystem changes.
- **Developer-controlled but supply-chain sensitive:** built-in plugin code, Cargo dependencies and lockfile, release artifacts, future plugin manifests/marketplaces, CI workflows, and migration logic.

Model output is never trusted merely because it came from the configured provider. Model-generated tool names/arguments cross the same policy boundary as hostile structured input. Likewise, repository instructions and skills are content for the model, not authority to weaken approval, sandbox, credential, or plugin policy.

### Trust boundaries

1. **Repository/config → composition and prompt.** U01/K12 now opens canonical trust before automatic `./heycode.toml`, project settings/profiles/skills/MCP/executables. Unknown blocks and interactive actions recompose; headless/ACP require explicit session choices. Relative home and trust/cwd mismatch fail. Residual risk: Windows durable trust is unavailable, so Windows startup/release support stays blocked rather than silently loading project authority.
2. **Human/ACP → agent turn.** TUI input and a local ACP client can schedule model work. ACP has no independent authentication because stdio is assumed to be an already-authorized same-user channel; embedding it across a network or privilege boundary violates the deployment assumption.
3. **Prompt/repository/provider → model output.** Remote providers receive exact prompt/history/tool schemas and return untrusted streaming text, reasoning, tool calls, usage, and provider state. Provider compatibility never grants capability support implicitly.
4. **Model tool call → approval/guards → side effect.** The agent logs a tool call, obtains an approval verdict, runs the monotonic `seam/pre_tool` waterfall, then dispatches through the tool registry. Shell execution, file mutation, web access, MCP calls, subagents, and future native tools must not bypass this single decision boundary.
5. **Tool/MCP → operating system.** `bash` executes arbitrary operator/model-selected shell text by design. The mandatory process-policy service routes bash, MCP and configured command credentials through one complete-argv transform; effective mode still defaults to explicit off, and file tools are not yet routed through a filesystem capability seam. “Arbitrary command execution” is reportable when it bypasses the declared approval/trust/sandbox boundary, not when an authorized tool call performs its documented job.
6. **Network → HTTP/SSE/provider parser.** Provider and web endpoints can return malicious status, headers, arbitrarily fragmented bytes, JSON/SSE payloads, redirects, and large or slow bodies. Transport framing is separate from protocol interpretation. Provider-specific error/state logic must not leak credentials or publish invented success.
7. **Credential providers → operation boundary.** Environment, configured command, and owner-only file providers resolve secrets by precedence. Descriptors expose metadata only. Secret material should exist only in zeroizing wrappers and the exact outbound auth operation; it must never enter prompt, session, UI events, catalog, config, diagnostics, or debug output.
8. **Memory → durable state.** Session events are append-only and publish only after write+flush. Attachment bytes commit before their metadata event and are addressed rather than inlined. Model-visible requests must be reconstructable from the durable log, including lossless provider continuation state. Config/settings/credential/catalog/attachment writers use version gates and atomic/no-clobber publication where implemented.
9. **Plugin/MCP/subagent lifecycle → process lifetime.** Plugin registration must be unique, dependency-complete, transactional, and effect-owned. Shutdown must unwind child processes, watchers, registrations, catalog flights, approvals, and subagents without orphaned authority.

### Security assumptions

- The OS, kernel, terminal emulator, current user account, and explicitly installed provider/MCP executables are trusted foundations. A fully compromised same-user account can already read most heycode data; heycode must still avoid unnecessarily copying secrets or widening exposure.
- The operator understands that `approval=auto` and `sandbox=off` grant a model broad same-user authority. Public-product onboarding must not present this combination as equivalent to a trusted-workspace, ask-approval, workspace-confined posture.
- HTTPS certificate validation and upstream cloud security are delegated to their maintained libraries/services. Custom HTTP endpoints are operator-controlled and can weaken confidentiality.
- Current sandbox claims are platform-scoped: macOS Seatbelt and Linux Landlock/bwrap have different guarantees; Windows has no current backend. An unavailable requested sandbox must fail loud.
- External distributable code product activation/install, delegated subscription runtimes and remaining platform/managed-policy Consumers are roadmap surfaces. Manifest-v1/cache/managed admission, abstract declarative/code activation and MCP stdio/HTTP/OAuth enforcement now exist with deterministic attack suites; concrete product registry/process activation and official live OAuth evidence remain explicit gaps.

## Attack Surface, Mitigations, and Attacker Stories

### Plugin composition, configuration, and migrations

`heycode-core` rejects duplicate plugin/service identities and unsatisfied injections; failed composition unwinds accumulated effects LIFO. The CLI owns the factory table and exact default inventory. Configuration versions fail on newer schemas, known home migrations are source-CAS guarded with byte-exact backups, and unknown plugin names fail loud.

Relevant attacker stories include a malicious project config activating an MCP command, weakening approvals/sandboxing, changing a provider endpoint, or omitting safety plugins; a symlink/concurrent edit redirecting a migration; and a plugin package claiming another contribution or escaping its install root. PL01 rejects incompatible/secret-bearing/non-portable/colliding metadata; PL02 now performs held descriptor-relative Unix package admission, immutable no-clobber refs and lease-safe cleanup. PL05 now closes first-install substitution for a *pinned* marketplace: PL02 alone cannot, because a never-seen id/version accepts whatever bytes arrive and the only digest in the system is the one the cache just computed from them. PL05 re-applies an operator-configured digest the cache does not own. **Still outside what this catches:** a marketplace that can rewrite both its catalog and the digest an operator pinned, since no publisher is authenticated — signatures are recorded, never verified. Remote fetch is unowned and non-Unix cache security remains fail-closed. Silent fallback after malformed config, dependency failure or collision is never acceptable.

### Prompt, skills, model output, and provider state

Repository text, skill Markdown, web/MCP results, and provider output can contain prompt injection. Prompt injection alone is not a vulnerability; it becomes security-relevant when it crosses a trust/approval/capability boundary, causes undisclosed secret inclusion, or triggers an unintended side effect. Model-callable tools are schema-registered, names are unique, requests resolve explicit model capabilities, and session v2 retains exact prompt/tool/provider state. C05 compares a detached durable projection with the live resolved call and binds success to the exact adapter instance.

Provider parsers enforce stream settlement, usage/finish order, tool-fragment identity, required DeepSeek reasoning state and Anthropic block/model/signature/tool/pause/cache-usage invariants. R02 applies separate sequence/correlation/payload/settlement validation to delegated runtime events. Malformed state must fail without a successful terminal event, and provider-controlled discriminators do not enter Anthropic/runtime diagnostics. Important classes are parser/resource exhaustion, state substitution across provider/model/protocol, tool-call argument confusion, malicious tool descriptions, and provider response content leaking into logs/errors.

Native compaction adds a destructive projection boundary. The provider may
return opaque state but cannot mutate the session: the Agent registry is the
single commit owner after validating count, size and exact
provider/model/protocol identity. A native checkpoint is applied only to that
route; neutral or incompatible projections retain the original prefix. Append,
open and independent projection reject a marker that names itself/future seqs,
preventing a crafted checkpoint from erasing later history. Cancellation owns
and settles the provider operation. Provider-specific adapters remain untrusted
until POA04/PAN04-style transport/continuation tests prove them.

POA04/PAN04 now satisfy that provider boundary and production activation.
Composition resolves the selected credential through the existing service and
passes it into a redacted route credential/provider; neither native checkpoint
Debug nor body-free faults expose it. The isolated activation proof uses a
unique temporary owner-only reference and performs no request. Detailed
OpenAI/Anthropic cache/edit metadata remains non-durable and therefore cannot
yet be relied on for audit/restart claims.

### Filesystem and shell tools

Read/write/edit/grep/glob/bash accept model-controlled paths or commands. E01 routes all file operations through one replaceable Provider with bounded/redacted specs/results, output-path validation and iterative glob work. Write refuses blind overwrite; edit requires a still-fresh observation stamp and publishes commit-consistent multiline metadata. Tool calls pass approval and the pre-tool waterfall; calls/results, including failure state, are durable. Bash uses an explicit scrubbed environment, bounded visible tail and whole-tree timeout/cancellation. Bash, MCP stdio and configured command credentials reach processkit only after the same effective sandbox service transforms their complete resolved argv.

Security-critical stories include path/symlink escape outside the intended workspace, TOCTOU after validation, command execution despite denial, secret recovery through an unsanitized environment name or readable credential file, sandbox escape, process-tree orphaning, and deceptive tool output. E05 now capability-binds canonical roots and rechecks root/parent/target/absence at atomic mutation; U01/K12 gate executable project authority. Seatbelt workspace roots are encoded as profile string literals, so quotes/backslashes remain data and controls/non-UTF-8 fail closed. E10 reports exact guarantees and the macOS matrix proves crafted-root, traversal, symlink, descendant and mutation enforcement natively. Seatbelt remains path- rather than inode-authorized: a pre-existing hard link inside the workspace can alias an outside inode. E11 hosted Landlock and E13 Windows confinement remain open. Arbitrary shell is intended, so severity still depends on bypass or a false confinement claim.

### Attachment storage and media admission

ATT01 stores user/provider-supplied bytes outside the workspace under an owner-only content-addressed capability. Relevant stories are MIME/extension confusion, image decompression bombs, oversized disk/memory use, content-address collisions, symlink/hard-link substitution, partial publication, cross-process races, metadata published before bytes, tampering after admission and Debug/error leakage of names or hashes. Admission ignores extensions, checks claimed MIME against content, reads image dimensions without full pixel decode, bounds bytes/dimensions/pixels and constructs validated safe metadata. Unix storage uses a schema marker, `0700` directories, `0600` single-link files, process plus `flock` serialization, fsynced temporary bytes and hard-link no-clobber publication. Reads recheck identity/length/hash/MIME/dimensions. The session event follows the byte commit; a failure may leave an unreachable immutable object but not a phantom durable reference. WEB03 adds validated redacted HTTP source provenance to the same event only after extraction succeeds. Windows/non-Unix local storage remains fail-closed until an audited owner-security backend exists.

ATT02 makes image selection separately durable. `user/attachments` must reference exact prior admissions and immediately precede its user message; split/orphaned pairs fail on open/projection/fork. Before that pair commits, Agent rereads and verifies every object, reconstructs the bounded four-format image payload, requires a strict adapter and exact `image_input=Supported` catalog evidence, and refuses Unsupported/Unknown/legacy paths. Responses/Chat/Anthropic serializers accept images only on user messages. This prevents a filename/MIME guess, stale metadata, route switch or unproven model from turning stored bytes into provider-visible content.

ATT03 applies the same law to PDFs/HTML without treating fallback as failure. Native PDF requires a strict adapter and exact `document_input=Supported`; Responses/Chat/Anthropic have separate pinned file shapes. Every other route uses the effect-owned bounded extractor, then commits its exact UTF-8 as a distinct owner-only object. `DocumentInputRoute` binds exact prior source and selected metadata as Native or Extracted; malformed, cross-object or non-text derivations fail session admission/projection. Local extraction emits no fabricated web provenance, observes cancellation/deadline and joins its worker. The remaining prompt-injection risk is the extracted/native document content itself; WEB05 must label untrusted retrieved content, and document-derived text is explicitly delimited but does not gain tool authority.

### Web and provider networking

The shared provider transport validates credential-free absolute URLs, hides header values from Debug, bounds buffered bodies/SSE events, handles cancellation, and leaves status/body semantics to the provider adapter. API-key validation maps body-free stable errors. `web_fetch` rejects literal and DNS-resolved private/loopback/link-local targets, pins admitted DNS answers, revalidates every redirect and caps rendered output. Search/fetch provider and domain selection is explicit or uniquely automatic. When a domain block rule exists, literal hosts fail closed as ambiguous translations and IDNs require a matching IDN allow rule; a broad ASCII parent cannot authorize an unlisted punycode homograph. Default no-block policy retains ordinary public literals and IDNs.

Relevant stories include SSRF to cloud metadata/local control planes, DNS rebinding, public-to-private redirects, credential-bearing URL/header leakage, decompression or unbounded-body denial of service, malicious content type/UTF-8/SSE framing, HTML/PDF parser panic/CPU/stack/decompression bombs, Markdown citation injection and retry duplication. WEB02/WEB04 pin per-hop authority and explicit domain/provider policy. WEB03 separates raw/output caps, uses lopdf's bounded load/per-page paths, caps pages, contains parser panic, supervises/joins one deadline-owned worker, rejects unsupported binary, escapes citation labels and publishes source metadata only after extraction. WEB05 marks successful Web tool output as typed untrusted external data, commits that on v2 `tool/result`, renders an explicit model warning and displays it in TUI replay. This does not neutralize adversarial instructions or replace approval/trust/sandbox enforcement; QSEC05 remains the behavioral eval after MCP content gains the same plane. P08 bounds/deadlines provider error bodies, removes reqwest URL text, gives HTTP status authority, disables replay for stateful calls and stops replay after normalized output; provider-specific idempotency/native-tool policies still require review as they activate.

Telemetry egress is absent by construction from the default provider, not merely disabled by a setting. Explicit `telemetry-otlp-http` lives in a separate HTTP/credential-owning crate and can replace local-off only through a profile. Its endpoint rejects userinfo/query/credential-shaped text; resource/attribute labels pass the shared credential screen; authentication settings name a header and credential reference/kind/scheme, never a value. The secret resolves per batch and raw HTTP/collector failure bodies have no return field. Requests, responses, retries and deadlines are bounded and cancellation owns the HTTP operation. Remaining risks are operator selection of an untrusted collector, metadata privacy even after secret screening, and implementation differences against a real collector; live evidence and data-governance policy remain release work.

### Credentials, settings, catalogs, and sessions

Credential inspection is separate from resolution. `CredentialSecret` has no display/serialization/clone surface and zeroizes on drop. Provider precedence prevents writes beneath a configured read-only source. File credentials reject symlinks/wrong types, use `0700`/`0600`, atomically replace, and preserve a conflict-checked legacy backup. Settings/catalog stores use schema gates, regular-file checks, atomic replacement, and last-good publication semantics.

Threats include secret values entering model/session/UI/error planes, permissions widened by migration or replacement, key rotation leaving stale validation, credential-provider fallback ambiguity, malicious cache metadata authorizing a capability, and tampered/truncated session logs changing model context. Catalog data is advisory and cannot override request resolution. Session readers reject unknown versions/kinds, sequence gaps, route/state mismatch, and prompt-hash drift. Session contents remain sensitive even without secrets and need owner-only retention/export policy; support bundles must be previewed and redacted.

QSEC02 treats credential-provider failure text as attacker-controlled boundary
data. The registry now drops it rather than trusting a documentation promise of
redaction; errors expose only validated provider/reference ids. A single
premise-checked environment canary crosses strict authentication and is absent
from prompt/body, UI/debug, request/session, process diagnostics and the
committed redacted support artifact. This proves heycode's planes, not secrecy at
the remote provider or inside an operator-selected credential backend.

### MCP, ACP, subagents, and child lifecycle

Configured MCP servers are executable/network capabilities and advertise untrusted tool names, schemas and results. Names are bounded before registry qualification, requests are cancellation-owned and Context shutdown settles children/connections. Definitions are private/redacted, publishers token-owned and generation swaps atomic. MCP12 commits admitted media through ATT01 and typed v2 provenance; raw media never enters Debug/UI/session JSON. MCP11/MCP13 bind each connection to the exact durable session, keep progress/logging human-only, cancel elicitation without late replies and pass a tool request with no server annotations through the ordinary Agent policy. Hook contributions become model-visible only after v2 append/flush/readback. Provider-specific bound generations and managed code authority remain PMM04/PZA05/PL09; no server content or profile fingerprint can authorize them.

Configured language servers are also executable children and their diagnostic
messages are prompt-injection data. E08 definitions bind exact argv/environment,
canonical workspace and language id, launch only through the sandboxed raw
subprocess service, bound Content-Length frames/documents/results, re-resolve
every returned file URI inside the workspace and reap descendants on effect
teardown. Model-facing results carry `UntrustedContentSource::Lsp`, distinct
from Web/MCP, before durable projection. Project definitions remain absent
until a K12-gated configuration owner exists. E06 spill ids are not bearer
capabilities: reads additionally require a host-created logical owner and verify
mode/link/identity/digest. Its audited backend is Unix-only; non-Unix refuses
owner-security claims.

ACP accepts local stdio JSON-RPC, creates composed worlds with client cwd, and can route ask-mode permissions back to the client. A malicious client with access to the channel can schedule prompts, supply up to 32-MiB image/resource bytes and answer approvals; this is expected only under the same-user assumption. X03 caps UTF-8 frames at 48 MiB, bounds/validates content blocks, admits media through ATT01, keeps one active prompt per session and uses the normalized runtime plane for output. `session/cancel` denies outstanding asks and cancels the exact runtime generation. EOF drains prompt/approval tasks, closes runtime and unwinds the retained Context. X02 app-server controls require workspace selections to be absolute, canonical, existing directories contained by the composed root; native roots remain fixed and delegated runtimes receive the selected path only after durable route commit. Subagents share provider/tools/approval authority, have a depth cap and distinct durable sessions. O05 background rows reserve token/id before spawn, retain the JoinHandle, propagate parent cancellation and publish settlement only after a durable inbox append. R05/R08 delegate permission decisions to the parent policy, refuse interactive questions and quiescently close the credential-owning runtime; neither path may outlive Context ownership or bypass parent policy.

The X04 app-server is a same-process local service today, not a network listener. Its client still crosses a closed 4-MiB JSON request/response boundary and every notification round-trips stable serde types, preventing Rust-call shortcuts from becoming the de facto SDK. It accepts only already-admitted attachment metadata; raw media remains ATT01/ACP/TUI authority. The native backend owns no task, filters replay by sequence and reads committed user routes before publication. Exposing this service over a socket in X06 requires same-user authentication/endpoint permissions; X04 is not remote-auth evidence.

X05's control contribution does not weaken that local-only trust assumption. It is a token-owned optional plugin generation and every method delegates to an existing owner service. Authorization catalogs never probe credential backends. Since 2026-09-05 the native keyring dependency and provider are removed entirely; bootstrap, setup, doctor and runtime cannot initialize an OS store. A correlated start is accepted only when its flow reference belongs to the effective provider; prompt notifications contain masked metadata only, responses contain no secret, and prompt-future drop revokes the answer id. The transient JSON answer necessarily exists inside the local request parser and zeroizing credential wrapper, so future socket/SDK hosts must prohibit request logging and preserve same-user endpoint protection.

Generic settings are wire-dark by default. Whole-namespace exposure is an explicit trusted owner attestation; unattested schema/default/base/user/project/resolved values never serialize and replacement is refused. Exposed writes use revision CAS and durable commit/watch publication before response. This is not S15 field-level secret-role verification: third-party/untrusted schema activation must remain unable to self-attest until that policy exists. MCP methods serialize only the registry's redacted snapshot, plugin methods expose exact public inventory, and model fallback never converts Unknown evidence into support/selectability. Control cancellation is joined in the request owner and Context shutdown removes the control generation before cancelling the base server.

X06 introduces no listener or new cross-user trust boundary. `heycode-sdk` transports receive raw frames, and an authorization answer necessarily carries a transient secret in one request; the contract forbids logging frames and maps all transport failures to body-free classes. Each frame is capped, response ids and closed events are validated, session notifications must match the opened identity, and sequence gaps fail before a success response. Rust serde supplies its recursion gate; TypeScript independently caps JSON traversal depth and UTF-8 bytes rather than trusting compile-time interfaces. The local adapter owns no task and drains notifications before response. A caller that starts a turn/cancel owns and awaits both futures. Shared fixtures reduce schema-confusion risk but are not authentication, fuzzing or hostile endpoint evidence. Any future stdio/socket/IDE transport must add same-user endpoint permissions, peer authentication where applicable, bounded framing and shutdown ownership under E08/X07.

R04 treats the official Codex process as a credential-owning delegated boundary, not a token source. heycode launches the exact identity/version, sends `account/read` only with `refreshToken:false`, parses a bounded closed response and discards email; it never opens `CODEX_HOME` auth files or accepts token fields. Debug/errors retain no raw account/model/provider bytes. Account and model calls each use a fresh connection and uncancelled quiescent close after every outcome, so cancellation cannot leave a token-refresh/model process orphan. A malicious or newer response cannot inflate memory through pages/cursors/models/efforts/modalities and cannot claim duplicate routes. Provider booleans and model fields map conservatively; absence or a namespace-tools false value cannot be promoted to generic Unsupported evidence. The live canary proves installed status/catalog compatibility, not token secrecy inside upstream Codex, login/logout behavior, a model turn, or primary-session containment.

### UI and terminal boundary

The TUI isolates secret input from transcript/EventBus/session state and renders only bullet placeholders. Approval dialogs carry structured action metadata. Model, tool, MCP, file, and web text remains untrusted display content; terminal-control injection, misleading permission summaries, hidden/truncated arguments, clipboard/link spoofing, and stale UI state are relevant classes. A rendered “allowed,” “sandboxed,” “connected,” or “saved” state must follow the durable/authoritative commit point.

### Out-of-scope or lower-relevance stories

- CSRF, browser XSS, internet-session fixation, and cross-tenant authorization are not primary classes while heycode has no browser-facing or multi-tenant server. They become in scope if a web/desktop remote control surface is added.
- Vulnerabilities wholly inside an upstream provider, MCP server, OS keychain, kernel, or explicitly installed executable are out of repository scope unless heycode exposes, misconfigures, trusts, or amplifies them unsafely.
- An attacker who already controls the operator's OS account is normally outside the privilege boundary. Persistence, cross-account access, secret duplication, or managed-policy bypass caused specifically by heycode remains in scope.
- Test/docs-only behavior is lower relevance unless it changes shipped artifacts, CI/release integrity, security guidance, or claims about a production control.

## Severity Calibration (Critical, High, Medium, Low)

### Critical

Critical impact requires a broadly reachable loss of the product's fundamental authority boundary, for example:

- A default or signed-update/plugin path allowing attacker-controlled repository or remote content to execute code before workspace trust/approval, with no meaningful operator action.
- A plugin/marketplace substitution that compromises all installs or exfiltrates credentials across users/releases.
- Remote unauthenticated control of an exposed heycode service leading to arbitrary same-user code execution or credential theft, if such a service is introduced.
- A claimed mandatory managed sandbox/policy that is universally bypassable to reach host credentials or arbitrary host execution.

The current local, single-user, stdio/terminal deployment reduces many network-service stories below Critical unless distribution-scale or zero-interaction reachability is proven.

### High

Examples include:

- A malicious cloned repository, model response, web page, or MCP server bypassing an enabled trust/approval guard to run shell commands, overwrite files outside the authorized workspace, or persist an executable configuration.
- SSRF reaching cloud metadata or local authenticated control planes and exposing usable credentials.
- Credential values leaking into model requests, session logs, tool output, diagnostics, or another provider route.
- Escape from a sandbox that the UI/policy explicitly represents as enforcing workspace or read-only confinement.
- Provider-state or durable-request confusion that causes a materially different tool-bearing request to be dispatched than the committed/approved one.

### Medium

Examples include:

- A malicious repository/session/provider frame causing a persistent crash, unbounded memory/disk/process use, or orphan child without crossing into code execution or secret access.
- Terminal rendering or diagnostics that convincingly spoof status/approval but still require a separate operator action for impact.
- Integrity loss limited to one local session/catalog/settings file with safe recovery and no credential or side-effect boundary crossed.
- A race or stale-state bug that produces an incorrect denial/selection but cannot silently authorize a destructive action.

### Low

Examples include:

- Minor metadata disclosure (model id, plugin list, non-sensitive path) without private source, session content, credentials, or useful escalation.
- Local malformed input producing a clear bounded error or one-shot process exit with no durable corruption.
- UI/reporting inconsistency that is not plausibly security-decisive and does not misrepresent approval, trust, credential, or sandbox state.

Severity always depends on realistic control of the input, default/profile reachability, required operator interaction, effective approval/sandbox posture, persistence, and concrete confidentiality/integrity/availability impact. Tests documenting a control are evidence of intent, not proof that an alleged attack is mitigated.

## 2026-08-31 boundary updates

- DNS-pinned web clients explicitly disable ambient proxies. Public URL values
  reject obvious local/metadata targets before provider selection; redirect and
  resolved-address checks remain independent controls.
- Provider readiness is caller-cancelled and joined before adapter/options/P10
  and C02/C05. A cloud profile or private catalog refresh cannot change the
  endpoint after durable verification or escape into a detached task.
- Hidden audio bytes remain in owner-only ATT01 storage. Stable session/UI/SDK
  planes expose metadata only and reject raw/base64 extension fields.
- Installed `[code]` packages require a separate managed authority generation
  binding provenance, session, grants and resources. The default product grants
  none; missing authority cannot downgrade to declarative activation.
- Sandbox roots reject lossy non-UTF-8 authority, Windows lexical paths reject
  ADS/device/control/trailing-dot-space forms, and workflow test-target names
  now address the real consolidated binaries. Native Windows/Landlock and the
  Seatbelt hard-link alias limitation remain explicit residual risks.
