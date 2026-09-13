# Contributing to HeyCode

Thanks for helping improve HeyCode. Open an issue describing a reproducible problem or discuss a larger feature before investing in a broad change. For bugs, include your OS, terminal, `heycode --version`, expected behavior, and a minimal reproduction. Redact credentials, conversations, and private paths.

Use stable Rust and Cargo. From a checkout:

```sh
cargo build --locked -p heycode-cli
cargo run -p heycode-cli -- --fake --no-background
cargo fmt --all --check
cargo test --workspace --all-targets
```

The fake provider is a deterministic development aid, not evidence of live model quality. Use a separate absolute `HEYCODE_HOME` for testing so you don't alter personal sessions. Network/provider tests require deliberate configuration; never commit keys.

Keep pull requests focused, describe the resulting user behavior, and include the tests you ran. Changes to permissions, persistence, tool execution, or updating need regression coverage for failure cases. For terminal changes, include an actual terminal capture and check narrow layouts and disabled animations.

Read [architecture](docs/guides/architecture.md) for service boundaries and [AGENTS.md](AGENTS.md) for naming guidance. Contributions are licensed under the repository's MIT license. Preserve third-party notices and identify the origin and license of any added asset or dependency.
