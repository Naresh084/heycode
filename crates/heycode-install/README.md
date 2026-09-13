# heycode-install

Q14→Q15→Q16 release boundary and effect-owned product manager. It authenticates
release manifests and artifacts, performs recoverable fresh/update/directional-
rollback transitions, enforces stable/preview/pinned and enabled-plugin API
policy, and evaluates fresh-machine evidence without promoting a workflow
definition into a pass.

This crate performs no artifact retrieval and owns no production signing key.
Plugin `release-manager-gh` injects the composed subprocess service and invokes
the official [GitHub CLI attestation verifier](https://cli.github.com/manual/gh_attestation_verify)
against the downloaded offline bundle. It
clears inherited environment and supplies only isolated HOME/cache/config plus
exact repository, signer workflow, OIDC issuer,
and `refs/tags/...` source identity; subject/bundle files live only in one
owner-only temporary directory. The release workflow uses GitHub OIDC/Sigstore
attestations, so no long-lived release secret is required.
On each native runner the externally verified downloaded `heycode` candidate
invokes its own shipping `release apply` command. The onboarding scripts then
execute the exact stable path that command published; neither step rebuilds a
checkout-owned manager or recopies the installed binary.

The release workflow builds the full-capability candidate with Cargo profile
`dist`. That profile keeps unwind-based plugin containment while using size
optimization, fat LTO, one codegen unit and symbol stripping. The staged bytes
are measured before attestation, and the resulting content-free size document
travels beside the binary without entering the signed release manifest.

## Authenticated release flow

1. `ReleaseManifest::parse_attested` bounds the manifest and detached bundle,
   invokes the configured signer verifier **before parsing remote JSON**, then
   admits the complete schema-v2 generation.
2. `AttestedReleaseManifest::verify_artifact` checks artifact size and checksum,
   checks the exact bundle digest pinned by the signed manifest, verifies the
   artifact signature, and returns a non-cloneable `VerifiedReleaseArtifact`
   owning those exact bytes.
3. Fresh install accepts that proof directly. Replacing an existing version
   additionally requires `ReleasePolicy` approval bound to the durable current
   version. Bypassing or replaying an approval is refused.
4. Rollback rereads and verifies the retained binary, artifact bundle, manifest,
   and manifest bundle before publication. It then applies the B03 config gate
   and the enabled-plugin API gate.

The verifier receives exact subject/bundle bytes plus locally configured
GitHub repository, workflow, and OIDC issuer. It returns only
`invalid|unavailable|unsupported`; verifier output never enters errors.
Deterministic signing material exists only under integration-test support.

## Durable layout and transition recovery

```text
<root>/.heycode-install
<root>/versions/<v>/heycode
<root>/versions/<v>/release.json
<root>/versions/<v>/artifact.sigstore.json
<root>/versions/<v>/release-manifest.json
<root>/versions/<v>/release-manifest.sigstore.json
<root>/bin/heycode
<root>/bin/.transition                 interrupted transition only
```

Retained versions are derived from complete disk entries, never a potentially
stale retention list. Writes use unique create-new siblings, file sync, mode
before publication, and atomic rename. Before replacing `bin/heycode`, the crate
commits a bounded transition journal. Reopening compares the exact current
binary digest and deterministically completes the new record or clears an
unpublished transition; contradictory state fails loud.

Rollback is directional. The target becomes current and `previous` clears;
the escaped version remains retained but is not automatically armed as the
next rollback target. A newer binary may already have performed a one-way
configuration migration, so rollback is refusal plus restore, never PL06's
symmetric plugin-version swap.

## Release channels and plugin API

- `Stable` accepts only a semantically newer non-prerelease.
- `Preview` accepts a semantically newer stable or prerelease.
- `Pinned(v)` accepts only exact `v` and may intentionally restore an older
  pin; the same current version is a no-op.

Every authenticated manifest declares the candidate external plugin host API.
`PluginCompatibilitySet` is a sorted, duplicate-free snapshot of enabled PL01
plugin ranges. Update and rollback both refuse the first incompatible plugin;
they never silently disable it.

## Fresh-machine evidence

`FreshMachineMatrix` evaluates macOS, Linux, and Windows independently. Sources
remain distinct:

- `WorkflowDefinition` and `CrossCompiled` satisfy no native observation.
- `LocalNative` applies only to the locally observed OS.
- `HostedNative { run_id }` requires a non-zero actual run.

The deterministic matrix requires attestation verification, fresh install,
first-ready, and any completed native turn on all three OSes. Q16 is stricter:
the turn must be `RealProvider` on all three. The checked-in release workflow
has never run. It now produces separate deterministic and real-provider
content-free documents and finishes with `heycode release evidence`; strict schema
v1 parsing rejects unknown platforms/checks, duplicate/incomplete facts, future
schemas and zero hosted run ids. Q16 remains externally incomplete until that
matrix job actually passes.

## Product surface and remaining evidence

`heycode release apply` accepts four explicit absolute local files (manifest,
manifest bundle, platform artifact, artifact bundle), explicit install root,
repository/workflow/source-tag trust and stable/preview/pinned policy. The
minimal management world snapshots every enabled PL01 manifest's inclusive API
range before verification. `heycode release rollback` re-snapshots those plugins,
uses the current config-version classification and re-verifies retained proofs.
Manager composition is mutation-free: an absent root remains absent until the
manifest, policy and artifact proof all pass. Update approvals are bound to the
observed current version and checked again at commit. Held manager handles
become terminal on Context shutdown.

Q14/Q15 still need one actual heycode release workflow run whose real GitHub
attestations pass this product command; a fixture verifier is not that evidence.
Q16 separately needs fresh-machine real-provider turns on macOS, Linux and
Windows. Workflow YAML and deterministic fake turns cannot close either gap.

For a credential-free local packaging check, the opt-in
`local-release-evidence` test accepts an explicitly built real `heycode`
executable and runs fresh install, stable-channel refusal, enabled-plugin API
refusal, update, rollback refusal, directional rollback, and installed fake
turns. It writes a strict content-withheld JSON record. That test deliberately
uses the deterministic integration-test verifier and records
`deterministic_fixture_not_github`; it is local transaction evidence, never a
GitHub attestation, hosted platform run, or real-provider claim.

## Focused verification

```sh
cargo fmt -p heycode-install -- --check
cargo clippy -p heycode-install --all-targets -- -D warnings
cargo test -p heycode-install

# After building the current heycode artifact, write local content-free evidence.
evidence_root=$(mktemp -d)
bash release-support/run-local-release-transaction.sh \
  "$PWD/target/debug/heycode" "$evidence_root/local-release-evidence.json"
```
