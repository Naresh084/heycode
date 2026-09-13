# Release support

Lane-owned Q14–Q16 support for `.github/workflows/release.yml`.

The release workflow uses GitHub's OIDC-backed artifact attestations. No
long-lived production signing key or repository secret is created or required.
It attests each native binary and the schema-v2 release manifest, retains the
Sigstore bundles beside them, verifies both, and runs `heycode release apply`
through the shipping `release-manager-gh` before a fresh-home smoke starts. A
release dispatch is accepted only from the exact `refs/tags/v<version>` ref.

`run-fresh-machine-smoke.sh` and `run-fresh-machine-smoke.ps1` accept explicit
`fake` or `real-openrouter` mode. Both require the exact absolute stable binary
path already published by the release manager, execute that path without a
copy, and create a new absolute `HEYCODE_HOME` and workspace. Fake mode emits
`deterministic_fake`. Real mode requires the workflow's OpenRouter secret,
selects `z-ai/glm-5.3-flash`, never prints the credential or response on
failure, and emits `real_provider` only after the fixed canary settles.

The native workflow likewise executes the downloaded, externally verified
candidate's own `release apply` command. It does not compile a second manager
from the checkout in the onboarding job. This makes the chain causal:
downloaded candidate → shipping verifier/manager → stable installed path →
fresh-home turn.

Shipping uses the dedicated Cargo `dist` profile rather than the ordinary
release profile. It retains panic unwinding, applies size optimization, fat
LTO, one codegen unit and symbol stripping. The build cache lives under the
runner's disposable `CARGO_TARGET_DIR`; only its `dist/heycode` output is staged
for measurement and attestation. Each native build uploads a content-free
`<platform>.size.json` containing exact bytes. The locally observed
macOS-aarch64 profile is capped at 20 MiB; unobserved Linux, Windows and macOS
x86_64 rows report `maximum_binary_bytes: null` until a real native build gives
them an honest baseline.

`run-local-release-transaction.sh` is the deterministic local complement. Give
it an already built real `heycode` executable and a new absolute evidence path. It
runs the opt-in crate test across fresh install, stable/preview policy,
enabled-plugin API update and rollback gates, update, directional rollback and
installed fake turns. Its exact JSON contains no artifact bytes, paths, prompt,
or model output and explicitly labels its signature proof
`deterministic_fixture_not_github`. It cannot satisfy Q14/Q15 or Q16.

Evidence vocabulary is enforced by `heycode_install::FreshMachineMatrix`:

- `workflow_definition` means a file exists and satisfies no observed cell.
- `cross_compiled` means target code compiled but did not run natively.
- `local_native` records a direct local observation for exactly one OS.
- `hosted_native` records a non-zero hosted workflow run.

Q16 is complete only when macOS, Linux, and Windows each have native
attestation, fresh-install, first-ready, and **real-provider turn** observations.
This repository currently has only the workflow definition; no hosted run is
claimed by checking in these files.

The product verifier/manager/CLI exists, but this repository still has only the
workflow definition. Q14/Q15 require a real heycode manifest and artifact bundle
to pass `heycode release apply` and rollback evidence; Q16 requires the uploaded
real-provider evidence from all three native runners. No checkmark follows from
the YAML alone.
