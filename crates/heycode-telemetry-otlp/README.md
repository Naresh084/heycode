# heycode-telemetry-otlp

Explicit opt-in OTLP/HTTP JSON provider for `heycode-telemetry`.

This crate owns the settings, credential-resolution and HTTP wire boundary that
the local-off telemetry crate deliberately cannot name. Selecting plugin
`telemetry-otlp-http` replaces `telemetry-local-off`; the default profile must
continue to select only local-off.

Configuration lives in the wire-exposed `telemetry-otlp` settings namespace.
Authentication contains only a credential reference and kind. The secret is
resolved for each batch, copied only into the validated request header, and is
never retained in configuration, payloads, failures or diagnostics.

The implementation targets OTLP/HTTP JSON metrics: HTTP `POST`, protobuf JSON
mapping, `Content-Type: application/json`, and a default metrics endpoint of
`http://localhost:4318/v1/metrics`. Protocol behavior follows the official
[OTLP specification](https://opentelemetry.io/docs/specs/otlp/) and
[OTLP exporter configuration specification](https://github.com/open-telemetry/opentelemetry-specification/blob/main/specification/protocol/exporter.md).

An opt-in named profile replaces, rather than layers over, local-off:

```toml
schema_version = 1

[[plugins]]
id = "telemetry-local-off"
enabled = false

[[plugins]]
id = "telemetry-otlp-http"
enabled = true
```

The settings document may then select a collector and a credential reference:

```toml
[settings.telemetry-otlp]
protocol = "http/json"
compression = "none"
endpoint = "https://collector.example/v1/metrics"

[settings.telemetry-otlp.resource]
service_name = "heycode"

[settings.telemetry-otlp.auth]
header = "authorization"
credential_reference = "telemetry/collector"
credential_kind = "api-key"
scheme = "bearer"
```

The credential value itself must be configured through the credentials service;
it is never valid in this namespace.
