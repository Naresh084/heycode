# heycode-telemetry

`heycode-telemetry` owns the replaceable `telemetry` service.

- `telemetry-local-off` is the default provider. It contains no exporter, so it
  can count locally but cannot emit.
- `telemetry-otel` is a distinct lower provider for explicit composition. It
  requires a caller-supplied `OtlpTransport`; this crate supplies no network
  transport. Product plugin `telemetry-otlp-http` lives in the separate
  `heycode-telemetry-otlp` crate and must replace, never accompany, local-off in an
  explicitly selected profile.

The OTEL provider admits only closed attribute keys and screened `Label`
values. Prompts, response bodies, URLs, credential values, arbitrary resource
keys, endpoint data, and raw transport failures have no outbound payload field.
Events enter a bounded queue and are exported in batches by one owned worker.
Flush is an ordered barrier. Plugin shutdown closes admission, flushes within a
bounded grace period, cooperatively cancels a blocked transport if needed, and
joins the worker before returning.

Telemetry event schema v2 adds one positive aggregate `count`. Schema-v1 events
remain readable as one occurrence, zero is rejected, local counters add the
count and OTLP exports it as a delta sum. The closed names now include committed
provider request, compaction and cache observations; closed dimensions include
purpose, lineage, execution plane, compaction kind and cache activity. There is
still no field for a prompt, tool arguments/results, path, provider body or
arbitrary failure text.

Default Consumer `telemetry-metrics` is owned by `heycode-agent`. It injects the
session plus whichever telemetry Provider composition selected, seeds bounded
route/runtime/lineage correlation from existing JSONL without replaying old
metrics, and then listens on the session-owned post-commit event bus. Local,
provider-exact and provider-aggregate tool activity remain distinct; aggregate
counts are preserved.

The deterministic tests use injected in-memory transports and make no network
requests. Production exposes the separate settings-backed OTLP/HTTP factory for
opt-in profiles only. Local-off remains the built-in default and structurally
cannot emit.
