# Releases and automatic updates

HeyCode uses `major.minor.patch` versions. `heycode --version` reports the version compiled into the executable. Before 1.0, minor versions may change configuration or extension contracts; patch versions are intended for compatible fixes. Release notes must identify migrations and breaking changes.

## Publication

A maintainer updates the workspace/package versions, documents changes, runs the required tests, and pushes an immutable `v<version>` tag matching `Cargo.toml`. The release workflow builds and tests on each supported native runner, runs an isolated executable smoke check, and records GitHub build provenance. It publishes the release only after every matrix build succeeds. A failed build leaves the release unpublished. Released tags and binary assets must not be replaced: publish a new patch version instead.

Artifacts are standalone binaries for macOS ARM64, macOS x86-64, and Linux x86-64, with a `SHA256SUMS` file and the installer. Platform availability is defined by assets that actually appear on the release, not by source-level cross-compilation claims. Provider-backed testing is separate from deterministic/fake-provider smoke evidence.

## Automatic updates

The installer creates `.heycode-install` beside the binary. An interactive launch checks that marker and starts a background update check that runs at most once an hour while the process stays open. There is no background daemon when HeyCode is closed. A new stable release will be picked up on the next eligible background check; pushing an ordinary source commit does not update users.

The updater uses the fixed `Naresh084/heycode` GitHub HTTPS API and selects the exact asset for the running platform. It rejects prereleases, downgrades, missing digests, oversized downloads, and unexpected download origins. It verifies GitHub's asset SHA-256 digest before writing. This authenticates the download through GitHub HTTPS; it is not an independent Sigstore verification. Build attestations are separately inspectable with GitHub CLI:

```sh
gh attestation verify ./heycode-macos-aarch64 --repo Naresh084/heycode
```

The updater stages on the installation filesystem, preserves permissions, and atomically replaces the executable on supported Unix platforms. A lock prevents concurrent replacements. `heycode.previous` retains the prior binary. Existing processes continue running their original executable. Installation needs write access to its directory; it never requests administrator access. Failures are recorded in `~/.heycode/update-status.txt` and retried on a later eligible launch.

Set `HEYCODE_AUTO_UPDATE=0` to disable automatic checks. `heycode update --check` checks immediately, and `heycode update` attempts an immediate installation. These commands do not read provider credentials. For an interrupted update with a leftover `.heycode-update.lock`, first ensure no update is running before removing that lock.

## Install a specific version or roll back

```sh
curl -fsSL https://raw.githubusercontent.com/Naresh084/heycode/main/install.sh -o /tmp/heycode-install.sh
HEYCODE_VERSION=0.1.0 sh /tmp/heycode-install.sh
```

To roll back locally, stop HeyCode, copy `heycode.previous` over `heycode` in your installation directory, and set `HEYCODE_AUTO_UPDATE=0` until you want to upgrade again. Binary rollback does not reverse data/schema migrations: read the release notes and retain a private backup of `~/.heycode` before crossing a breaking version.

## Maintainer checklist

- Confirm version consistency and provide user-facing release notes.
- Run formatting and relevant tests; investigate failures before tagging.
- Scan the exact publication tree and inspect intentional screenshots for secrets.
- Confirm the tag workflow passes on each advertised platform.
- Download public assets into a fresh home; check version, startup, installer, and update behavior.
- Report deterministic tests, real-provider checks, and unsupported cases separately.
