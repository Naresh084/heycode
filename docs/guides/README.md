# heycode guides

These guides separate deterministic local behavior from evidence that needs a
provider, native runtime, platform backend, managed plugin authority, or a
fresh-machine release artifact. The project is not engineering; a command that
parses or passes offline is not evidence that a live integration works.

## Use heycode

- [Source-checkout setup](getting-started.md) — build, isolate product state,
  run the offline smoke, and choose workspace authority.
- [Provider and runtime setup](providers.md) — connect without copying secrets
  into config and select only catalog-evidenced models.
- [MCP servers](mcp.md) — configure stdio or Streamable HTTP, inspect health,
  and understand trust/auth boundaries.
- [Plugins](plugins.md) — built-in profiles, external manifest v1, lifecycle,
  managed admission, and current distribution limits.

- [Agent conversations and user questions](agent-conversations.md) — automatic
  delivery, navigation, readable failures, and structured answers.

## Understand or extend heycode

- [Architecture](architecture.md) — composition root, `Context`, service planes,
  and current dependency boundaries.
- [Plugin lifecycle](plugin-lifecycle.md) — activation transactions, effects,
  rollback, shutdown, and reload generations.
- [Session v2](durable-sessions.md) — JSONL envelopes, migration, projection,
  compaction, repair, and shared-prefix lineage.
- [Provider authoring](provider-authoring.md) — the direct inference-provider
  vertical from auth through composition evidence.

## Generated references

- [Commands](../reference/commands.md)
- [Configuration](../reference/configuration.md)
- [Plugins and manifest v1](../reference/plugins.md)
- [Session events](../reference/session-events.md)
- [Provider/model capability vocabulary](../reference/capabilities.md)
