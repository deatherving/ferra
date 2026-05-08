# ferra

This is a placeholder crate. The real Ferra is split into two crates:

- [`ferra-server`](https://crates.io/crates/ferra-server) — the HTTP + SSE
  configuration server (talks to Postgres, exposes the API)
- [`ferra-agent`](https://crates.io/crates/ferra-agent) — the sidecar that
  runs alongside your service container, holds an in-memory cache, and
  exposes a localhost HTTP API your service reads from

## Install what you actually need

```bash
cargo install ferra-server     # the server
cargo install ferra-agent      # the sidecar
```

If you `cargo install ferra`, you get this placeholder binary, which just
prints those install instructions and exits.

## Why this crate exists

To own the name on crates.io and prevent supply-chain confusion. Without
it, anyone could publish a malicious `ferra` crate, and users guessing
`cargo install ferra` (because "ferra is the project name") would get the
wrong thing.

## Source

https://github.com/deatherving/ferra

## License

MIT
