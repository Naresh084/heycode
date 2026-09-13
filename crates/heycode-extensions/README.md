# heycode-extensions

Validated extension manifests, the immutable local install cache, and the
marketplace pin/provenance boundary. This crate owns package admission,
storage, origin establishment, and the host-neutral declarative activation
bridge, plus the pure managed marketplace/plugin policy boundary. Workspace
trust, remote dependency acquisition, network fetch, cryptographic signature
verification, and root settings/profile composition remain separate owners.

## Manifest boundary

Manifest v1 validates plugin identity/version/API/platform compatibility,
portable declared paths, contribution namespaces, dependency relationships,
permission/auth references and pinned source metadata. Unknown fields,
duplicates, Windows device aliases, unsafe URLs and undeclared override
authority fail before any contribution publishes. Ordinary projections are
secret-free and deterministic.

An optional strict `[code]` table now owns the installed artifact choice. It
requires exactly one `native_process` or `wasi_component_v1` runtime and one
portable package-relative entrypoint. Declarative-only manifests retain no code
metadata. Product activation uses only this typed declaration; a caller cannot
silently substitute another artifact or runtime after package admission.

## Install cache

- A schema-v1 owner-only cache marker controls compatibility.
- Source traversal is held, descriptor-relative and no-follow through
  `cap-std`; symlinks, hardlinks, special files, traversal, portable-name
  collisions and source/root swaps are rejected.
- Package size, entry count, file size and depth have explicit admission caps.
- A canonical SHA-256 tree hash names immutable content objects. Exact plugin
  id and SemVer map through case-insensitive-safe hashed reference keys while
  the verified reference retains the original version text.
- Publication uses staged revalidation and create-if-absent hard-link commits;
  an existing id/version reference is never replaced.
- OS-backed install/object/staging leases serialize processes and protect live
  work from cleanup. Same-process locks close advisory-lock aliasing gaps.
- Resolution rehashes and revalidates the referenced object. Failed installs
  retain the last good version and publish no partial generation.

Unix currently supplies the audited owner/mode and file-lock implementation.
Other platforms fail closed with `UnsupportedSecurity` until an equivalent
owner-only DACL and lock backend is verified. Cross-platform release gates
must not reinterpret that failure as install support.

## Marketplace pin, provenance and substitution

- A `MarketplaceSource` is pinned to one exact `CatalogDigest`. Catalog bytes
  are size-bounded, then hashed and compared against that pin, and only then
  decoded — an unpinned or oversized document is never parsed.
- A catalog is admitted as a whole generation. One malformed row rejects the
  document, a row may not carry an id outside the catalog's own marketplace
  namespace, a remote catalog may not vend a local package source, and the
  same id/version may not be offered twice.
- Each row pins the canonical package **tree** digest — the address this crate
  computes and can therefore verify. The manifest's `source.checksum`
  addresses an upstream artifact that nothing here fetches, so it is carried
  but never checked.
- `MarketplaceCatalog::admit` verifies installed bytes against the pin the
  caller **requested**, not the identity that arrived. A package that installs
  as another offered version fails rather than resolving to it.
- `PackageProvenance` records the established origin plus the verified digest,
  and `reverify` re-applies the same comparison later. The cache proves an
  object matches its own committed reference; that is self-consistency, and
  whoever wrote the cache wrote both halves.
- `PackageOrigin::Unknown` is terminal. A package's own `[source]` block is a
  claim about itself and never establishes an origin; the claim is retained
  separately so a surface can show it beside the fact.
- `SignatureState` has `Absent` and `Present` and deliberately no `Valid`.
  This crate performs no cryptographic verification and holds no publisher
  keys, so a recorded signature is evidence of a declaration only.
- Failures carry a row index plus compile-time field paths, never bytes read
  out of the fetched document.

## Managed marketplace/plugin policy

- A managed policy contains exact id/version rules copied from one pinned PL05
  marketplace source and catalog row. No rule is an explicit deny; there is no
  implicit wildcard, neighbouring version, source mirror or registration-order
  fallback.
- Every evaluation reports eight independent decisions: source, update channel,
  marketplace publisher namespace, version, catalog/package digest, signature,
  platform and capability. Only eight affirmative `Allowed` verdicts authorize
  an operation. `Unknown` never becomes allowed.
- Source admission pins the marketplace kind/locator/catalog digest and the
  package source kind/locator. Publisher here means the administrator-approved
  marketplace namespace; it is not cryptographic publisher authentication.
- Capabilities are an explicit ceiling over both requested permissions and
  contribution kinds. Empty allowlists deny all such requests, and duplicate
  administrator rows fail rather than being silently normalized.
- Signature and checksum limitations remain honest. Policy may explicitly
  accept digest-only or a catalog-pinned signature declaration. Requiring a
  verified publisher signature, or the manifest's upstream artifact checksum,
  produces `Unknown` because PL05 owns neither a key verifier nor a fetch
  receipt. Those requirements therefore deny install and activation today.
- `install_managed_marketplace` reads and freezes the exact source tree first,
  evaluates policy and PL05 provenance, then gives those same bytes to PL02's
  unchanged no-clobber commit. A refusal creates no cache object or reference.
- `prepare_managed_declarative` resolves and rehashes the committed object,
  reapplies PL05 plus the current policy, and freezes contribution documents
  from that exact in-memory tree. Only its opaque result can enter
  `managed_declarative_activation_plugin`; a refusal invokes no host callback.
- `ManagedPluginAdmissionGeneration` binds source, exact catalog digest, host
  target and every policy rule behind one deterministic SHA-256 fingerprint.
  Lifecycle and installed-code activation clone this same generation and both
  call `prepare_managed_declarative`; a profile fingerprint from an older
  catalog, host or capability ceiling cannot authorize current code.
- Policy/configuration/operation failures are closed and body-free. They never
  retain or render catalog/package documents, locators, signature bytes or host
  paths.

Product lifecycle now requires an explicit `PluginLifecycleAdmission` for every
install/enable/update/rollback. The composition root supplies
`RequireManagedPluginPolicy`, so missing authority denies before cache/state
mutation while disable/remove remain usable. `ManagedLifecycleAdmission` binds
an exact cache/source/catalog/host/policy generation and rechecks the cached
version through `prepare_managed_declarative`. A future administrator discovery
surface supplies that generation; it cannot make the default permissive.

## Dependency, conflict and platform resolution

- `PluginGraphResolver` consumes a complete candidate manifest generation and
  an explicit current `PlatformTarget`. It never infers platform support from
  the build host, a catalog source, or validation that may have happened for a
  different target.
- One plugin id selects one exact manifest/version. Required dependencies must
  be present; optional dependencies may be absent, but when present they must
  satisfy the same inclusive-minimum/exclusive-maximum SemVer range. SemVer
  build metadata does not alter precedence.
- A selected conflict is symmetric even when only one manifest declares it.
  Missing dependencies, incompatible versions, conflicts, duplicate ids,
  unsupported current platforms and directed dependency cycles have distinct
  closed diagnostics containing only validated ids, versions and platforms.
- Successful graphs are dependency-first. Every simultaneously ready row is
  ordered by plugin id, so caller order and manifest dependency-list order do
  not affect activation or lifecycle publication.
- The opaque `ResolvedPluginGraph` gates current mutation boundaries:
  `install_resolved_directory` compares frozen source metadata before PL02
  publication; `install_resolved_managed_marketplace` joins the same check to
  PL08; `PluginLifecycle::apply_resolved_graph` preflights every cache/admission
  row before at most one Settings write; and resolved activation constructors
  verify the complete package set before any host callback.
- Lifecycle reconciliation enables/installs/updates/rolls back rows in resolved
  order and disables installed rows outside the generation without removing
  retained versions. Managed admission is still rechecked for every
  authority-increasing row.

The resolver selects among supplied manifests; it does not discover, download
or choose dependency versions. A root marketplace/fetch owner must assemble the
candidate set, construct the graph, and route product cache/lifecycle/activation
through the combined resolved+managed APIs. No new service or empty plugin is
published while that root Consumer remains absent.

## Declarative activation bridge

- `DeclarativePackage::load` opens an installed package component-by-component
  without following symlinks, then freezes bounded UTF-8 primary documents for
  skills, commands, agent presets, hooks, themes and provider declarations.
- `DeclarativeContributionHost` has six required activation methods. A concrete
  host cannot compile while silently omitting one PL03 registry, and it declares
  every service and exact inventory family the aggregate bridge plugin uses.
- The aggregate `declarative-extensions` plugin dispatches packages in manifest
  order inside core's verified K09 transaction. Successful host registrations
  must attach their exact disposer to the supplied `Context`; a later refusal
  therefore removes the already-activated prefix.
- Active package ids and `(kind, public name)` claims are collision-checked
  before composition. Documents and host failures use bounded, body-free
  diagnostics; document bodies are absent from `Debug`.
- Bundled MCP declarations are frozen separately with the verified installed
  root and handed to PL04's transport-aware product host rather than being
  mislabeled as one of PL03's six kinds.

`heycode-extension-host` supplies the concrete product adapters and the default
`product-extensions` factory. Enabled lifecycle rows are resolved and rehashed
from the PL02 cache before strict documents reach skills, commands, subagent
presets, hooks, themes, providers or bundled MCP.

## Out-of-process code-plugin protocol (PL09)

- A native code artifact is a regular owner-only executable inside the exact
  reverified PL02 object. The package-tree digest and executable-byte digest
  are independent identities, both carried through the strict correlated
  initialize/ready handshake.
- Capability grants are constructed for one package/version/tree/executable
  generation. Empty is the default; duplicates and permissions the manifest
  did not request fail before launch. Process annotations cannot add a grant or
  contribution.
- JSON request/response frames have fixed byte, depth and node ceilings, exact
  session/request correlation and closed body-free failures. A pre-cancelled
  call sends nothing. Cancellation after admission, transport/protocol drift
  or process exit retires the complete generation before any response escapes;
  a closed per-operation denial leaves a healthy process active.
- Host registration is two phase. Product proxies register behind a closed
  gate, the runtime commits the complete generation once it owns its disposer,
  and crash/cancel/refusal closes the gate before LIFO token withdrawal.
- Each code contribution also freezes its bounded immutable PL03 document from
  the same reverified package generation. Concrete registries therefore never
  reopen an ambient path after the executable handshake.
- Native libraries are not loaded into the heycode address space. There is no
  `dlopen`/dylib ABI or shortcut around the out-of-process boundary.

The protocol now has a real execution-owner adapter above it.
`heycode-exec::SubprocessService::spawn_exact_interactive_raw` binds the frozen
bytes, explicit empty environment, authority ceiling, raw framing and process
tree; `heycode-extension-host::HeycodeExecCodePluginLauncher` implements this trait.
Native launch remains fail-closed when the selected OS sandbox would expose an
ungranted authority. `heycode-extension-host` now supplies the six concrete
product registry adapters and a combined installed `product-extensions`
factory. The factory joins exact enabled lifecycle state to an explicit
`InstalledCodePluginAuthority`: established provenance, one unique host session,
an exact grant set and runtime-specific resources. The production managed
provider verifies the profile's PL08 fingerprint, re-admits current bytes,
compares version/tree digest/runtime/entrypoint/grants, object-binds WASI
preopens during apply and mints a new session for each activation. Missing,
extra or drifted rows fail before any registry publication, and the
declarative-only factory refuses to downgrade code into inert metadata. Root
still owns trusted profile discovery and supplies the same PL08 generation to
lifecycle plus code activation; these crates never infer a grant, preopen,
endpoint or session.

## WASI Component Model / WIT v1 (PL10 lower boundary)

The checked-in [`heycode-code-plugin-v1.wit`](wit/heycode-code-plugin-v1.wit) defines
one typed exported plugin interface and four exact worlds: pure, filesystem,
network and filesystem+network. The design follows the official Component
Model [WIT world contract](https://github.com/WebAssembly/component-model/blob/main/design/mvp/WIT.md),
[layer-1 binary preamble](https://github.com/WebAssembly/component-model/blob/main/design/mvp/Binary.md)
and [Canonical ABI](https://github.com/WebAssembly/component-model/blob/main/design/mvp/CanonicalABI.md),
with the current [WASI 0.3.1](https://wasi.dev/releases/wasi-p3)
filesystem/socket `imports` worlds pinned as part of this ABI revision.

- Loading freezes an owner-only non-executable component file, requires the
  Component Model layer preamble and binds both package-tree and component-byte
  SHA-256 identities. Full decoding/typechecking remains engine-owned.
- `deny_all` selects the pure world and inherits no environment, arguments,
  stdio, preopens or network. Filesystem grants require unique absolute host
  roots mapped to unique portable guest paths with explicit read/write modes.
  Network grants require exact host+port endpoints; unrestricted inheritance is
  unrepresentable.
- WIT v1 deliberately has no isolated process-spawn, credential-use or MCP
  host interface, so those runtime grants fail closed. Hook registration and
  override remain activation-time authority and add no WASI import.
- A `WasiComponentEngine` must fully validate the component, Canonical-ABI
  typecheck the selected world and return the exact WIT digest/import/export
  report before the ordinary PL09 handshake may begin. Mismatch or cancellation
  shuts the instance down.

`heycode-extension-host::WasmtimeWasiComponentEngine` is now the concrete engine
adapter, pinned to Wasmtime 48.0.1/WASIp3. It fully decodes the Component,
checks the exported interface and both function shapes against this WIT,
accepts only selected-world WASI imports, instantiates with bounded
memory/tables/instances/fuel and executes typed `initialize`/`invoke` calls.
The default `WasiCtx` inherits no environment, arguments, stdio, preopens or
network. Read-only/read-write preopens are minted exactly. Outbound TCP is
checked against pre-resolved IP+port rows; hostnames are refused because
Wasmtime exposes only a global DNS switch, and write-only preopens are refused
because its filesystem host API cannot enforce write-without-read. Refusal is
preferable to widening either grant.

The real engine and ABI/isolation suite now feeds the combined installed product
factory. Root already owns the pinned Wasmtime workspace dependencies; it still
must supply the current managed package/policy authority generation to that
factory. A Component may import only the interfaces it uses, so the engine
reports the actual import set and admission requires it to be a subset of the
selected world's pinned 0.3.1 contract; a pure world still requires the empty
set exactly. Exports, function shapes, WIT digest and selected world remain
exact.

## Curated inspector projection (PL11 lower boundary)

`CuratedPluginInspector` joins exact lifecycle state with a value-minimized
package view. Its report can contain only validated id/version, enabled and
rollback booleans, closed execution/generation enums, contribution-kind counts
and closed requested/granted permission names. Descriptions, contribution
names/bodies, paths, locators, digests, process/session ids, errors, settings and
credential references have no field. Missing, extra, duplicate or version-
drifted inputs fail the whole bounded report.

This crate does not register a model tool. QSEC01 review and a root product
Consumer must approve and mount the projection through the ordinary Tool/Agent
durable result path before PL11 can claim model-facing reachability.

## Verification

```sh
cargo fmt -p heycode-extensions -- --check
cargo test -p heycode-extensions
cargo clippy -p heycode-extensions --all-targets -- -D warnings
```

The suite includes concurrent identical/conflicting publishers, cleanup races,
ambient ancestor replacement, no-clobber publication, case-distinct SemVer,
tamper recovery and capability-bounded deletion; marketplace pin, provenance
and substitution cases; eight-axis managed-policy denial with zero cache/host
mutation; deterministic PL07 graph/order/error and pre-mutation cache/lifecycle/
activation cases; generated register/dispose/reload lifecycle properties; and
six-kind declarative activation/rollback checks in a real core composition;
PL09 two-phase proxy-generation/crash/cancellation/refusal cases plus real
exact-image/raw-framing/tree-reap coverage; PL10 component identity/WIT
ABI/default-deny/scoped-capability cases plus real pure and scoped Component
execution; and PL11 structural redaction/fail-loud join cases.
