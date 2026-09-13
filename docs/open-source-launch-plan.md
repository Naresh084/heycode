# HeyCode open-source launch

The public project is **HeyCode**, the command is `heycode`, and the GitHub owner is `Naresh084`. The development checkout stays named `dshx`. Application data has been moved to `~/.heycode`; the code does not fall back to the old home or old project configuration filenames.

## Publication boundary

Publish a fresh, curated source root rather than the private development history. Keep source, synthetic tests, build manifests, user documentation, and deliberately reviewed brand/product images. Temporary builds, captured private sessions, screenshots from earlier audits, caches, and copied PDFs remain local and ignored. Adding ignore rules does not remove historical Git objects, which is why the initial public repository uses a clean source snapshot.

The earlier local scan found no confirmed real credentials. The publication snapshot must be rescanned immediately before pushing. Review scanner findings individually and inspect the intentional images; do not equate a clean text scan with proof that every possible credential is absent.

## Identity and experience

Use HeyCode in command help, package metadata, crate names, SDK/editor identifiers, documentation, and release assets. Keep persisted cryptographic domain identifiers stable where changing them would invalidate retained data; these are internal format identifiers, not alternate homes or branding aliases.

The terminal companion uses a compact pixel cat with finite click animations and local greeting/acknowledgement reactions. It must preserve drafts, avoid consuming modal interactions, retain operational waiting/error cues, fit narrow layouts, and respect disabled animation. The brand artwork and actual product captures belong in `docs/brand/` and `docs/images/`.

The README should lead with purpose, installation, product screenshots, connection choices, and everyday commands. Explain subscription-backed delegated CLIs separately from inference API credentials and local endpoints. Put architecture and implementation details in contributor documentation.

## Distribution and releases

Use MIT licensing, a contributor guide, private security reporting, and user-facing release notes. Publish standalone macOS ARM64, macOS x86-64, and Linux x86-64 executables after native build/test gates pass. Do not advertise unsupported platform binaries, Apple notarization, marketplace publication, or live provider compatibility without evidence.

The installer downloads a single exact release, checks SHA-256, and installs without Cargo, Node, or a source checkout. Tag pushes matching the workspace version run the release pipeline. Build provenance accompanies the assets. Release publication follows successful native checks; a failed build must not publish a partial set.

Installer-managed copies check for stable updates in the background on launch. The update check is rate-limited, verifies the GitHub asset digest, preserves a previous binary, and replaces atomically. Running conversations keep their original executable until restart. Document the opt-out, failure status, explicit retry command, and rollback limitations in [release policy](releases.md).

## Acceptance before announcement

1. Run the exact source secret scan and verify that excluded artifacts are absent from the public commit.
2. Pass formatting, relevant Rust tests, SDK tests, and editor tests; investigate broader regression failures.
3. Inspect actual terminal captures from a clean demo workspace, including a real provider response and explicit approval, plus mascot interaction, narrow width, and reduced motion.
4. Push the curated source under `Naresh084/heycode`, observe hosted checks, then publish the version tag.
5. Download the actual public assets, test a fresh installation and automatic update behavior, and verify provenance independently.
6. Update release notes and README with observed support and limitations. Report what shipped and what was tested without substituting controlled tests for live evidence.
