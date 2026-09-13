# heycode release notes

## Unreleased — 0.1.0 development

This is the development changelog bundled with this build. It does not announce a published release.

- Session names support `/rename [title...]`, local title generation, and distinct names for rename operations. `/copy [number]` selects a completed answer, counting back from the latest.
- Context details retain request identity, configuration revisions, measured cache usage, and estimated contributor breakdowns across session restart. Estimated values remain labeled.
- Native file tools provide bounded paginated reads, batch reads, checked writes, and atomic multi-edit operations with explicit continuation and error details.
- Structured work, agent conversations, background processes, and teams have distinct identities and lifecycle controls.
- Model tools include plan-mode entry, consolidated LSP operations, and MCP resource/readiness inspection when their services are available.

Provider and platform capabilities vary. The current development validation records are in `docs/audits/`; these notes do not claim complete provider or Claude UI parity.
