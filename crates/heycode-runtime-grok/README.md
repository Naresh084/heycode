# heycode-runtime-grok

Official Grok Build subscription integration through its ACP process. Plugin `runtime-grok` contributes `agent_runtime:grok`; it does not read vendor tokens or register a heycode credential provider.

The reviewed release is 1.0.13 with exact output `grok 1.0.13 (5e9a58528b76)`. Discovery uses an effect-owned temporary workspace. Actual sessions use their requested working directory. The runtime checks official cached-token authentication, discovers models and selects them through the legacy ACP model contract when returned by this release. Missing installation remains an unavailable runtime row.

The explicitly enabled installed canary passed account discovery, model selection and a tool-free subscription turn on 2026-09-05:

```sh
HEYCODE_GROK_E2E=1 cargo test -p heycode-runtime-grok --test installed_canary -- --nocapture
```

Install and sign in using the official Grok CLI. heycode never copies the vendor's token into its own store. This optional default plugin does not force itself into explicit custom profiles.

Runtime descriptors may carry bounded provider-owned connection recovery instructions. The subscription wizard displays them after account or installation checks fail and keeps Enter available for retry; sign-in is performed through the official app.
