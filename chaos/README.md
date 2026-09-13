# heycode deterministic chaos runner

`heycode-chaos` is an isolated, offline runner over public package surfaces. It
uses one explicit seed and emits only schema-versioned scenario names and
closed pass/skip/failure codes—never request bodies, credentials, paths,
provider output, or session content.

The runner covers Q13's external quality group:

- plugin apply failure and LIFO rollback, including idempotent disposal;
- whole-event and torn-write session boundaries, clean handled settlement,
  interrupted nested work, sequence-gap refusal, and read-only crash repair;
- quiescent subprocess-tree cancellation using an explicit release barrier;
- raw SSE and provider parsing across deterministic byte fragmentations;
- positive-premise provider error-body redaction and status-authoritative
  normalized failure classes.

Every wait has an outer deadline, every file is under `tempfile`, no network is
used, and the subprocess case uses a fixed explicit environment. Run the same
CI seed locally with:

```sh
cargo run --locked --manifest-path chaos/Cargo.toml -- --seed 12648430
```

On hosts without a POSIX shell, the subprocess-tree scenario reports
`skipped` rather than claiming evidence. A bounded deterministic pass proves
only the executed seed/scenarios; it is not long-duration chaos evidence.

