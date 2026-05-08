# ferra-agent

Sidecar agent for [Ferra](https://github.com/deatherving/ferra). Holds an
in-memory cache of every key under one or more configured prefixes, keeps it
fresh via SSE watch against `ferra-server`, and exposes a tiny localhost HTTP
API the service container hits for reads.

## Why use it

Without the agent, every service that talks to Ferra has to implement the
~500-line cache + watch + reconnect loop itself (per language). With the
agent, services do a 5-line localhost HTTP call:

```bash
curl http://localhost:9999/cfg/services/payment/timeout_ms
# 3000
```

One Rust binary, one set of tests, every language reaps the benefit.

## Install

```bash
cargo install ferra-agent
```

Or use the prebuilt container image once it's published.

## Run

```bash
ferra-agent \
  --server http://ferra-server.ferra.svc.cluster.local:8080 \
  --prefix services/payment/ \
  --prefix flags/global/ \
  --listen 127.0.0.1:9999
```

Or via env vars (more typical for K8s):

```bash
FERRA_SERVER=http://ferra-server.ferra.svc.cluster.local:8080 \
FERRA_PREFIX=services/payment/,flags/global/ \
FERRA_AGENT_LISTEN=127.0.0.1:9999 \
ferra-agent
```

## Local API

Consumed by the service container via `localhost`:

```http
GET /cfg/{key}                            # current value, ~1ms localhost RTT
GET /cfg/{key}?wait=30s&since=N           # long-poll, returns when key changes
GET /cfg?prefix=services/payment/         # list everything under a prefix
GET /healthz                              # always 200 (process alive)
GET /readyz                               # 200 when initial snapshots loaded
```

The `wait`/`since` parameters give you Consul-style blocking-query semantics:
`since` is the cursor you last saw (header `X-Ferra-Index`), `wait` is the max
hold time (capped at 5 minutes). Returns when the key's `event_id > since` or
the wait elapses.

`X-Ferra-Index` is on every response; use it as your next `since`.

### Reading a value

```bash
curl -i http://localhost:9999/cfg/services/payment/timeout_ms

# HTTP/1.1 200 OK
# X-Ferra-Index: 43
# Content-Type: application/json
#
# 3000
```

### Reacting to changes (long-poll)

```python
since = 0
while True:
    r = requests.get(
        f"http://localhost:9999/cfg/consumer/goroutines",
        params={"wait": "30s", "since": since},
    )
    pool.resize(int(r.json()))
    since = int(r.headers["X-Ferra-Index"])
```

The agent holds the connection until the value changes or 30s elapses, so the
service code is a normal blocking HTTP call — no SSE parsing.

## Prefix scoping

The agent only caches the prefixes you tell it to watch. A request for
`/cfg/services/auth/jwt` from a service whose agent watches only
`services/payment/` returns:

```json
{ "error": "key_not_in_watched_prefix", "message": "..." }
```

This is intentional — it forces explicit declaration of which config a service
needs, instead of letting agents accidentally fetch the entire instance.

## Resource footprint

Per Pod:

- Cache memory: ~200 bytes × number of keys under the watched prefixes.
  10–100 KB is typical; 1 MB is large.
- Background memory: ~10 MB (Rust runtime + reqwest + axum).
- CPU: idle.
- Network: one persistent SSE connection to `ferra-server`, plus a brief GET
  per `set` event.

## Configuration

Flags (env-var equivalents in parentheses):

| Flag | Env | Default | Purpose |
|---|---|---|---|
| `--server` | `FERRA_SERVER` | _required_ | URL of `ferra-server` |
| `--prefix` (repeatable) | `FERRA_PREFIX` (comma-separated) | _required_ | Prefix(es) to watch |
| `--listen` | `FERRA_AGENT_LISTEN` | `127.0.0.1:9999` | Local listen address |
| `--min-backoff` | `FERRA_MIN_BACKOFF` | `500ms` | Reconnect backoff floor |
| `--max-backoff` | `FERRA_MAX_BACKOFF` | `30s` | Reconnect backoff ceiling |

## Failure model

- **`ferra-server` unreachable on startup**: snapshot load fails, agent
  retries with exponential backoff. `/readyz` stays at 503 until a snapshot
  loads. `/cfg/{key}` returns 404 in the meantime.
- **`ferra-server` unreachable after startup**: cached values keep serving
  (last-known-good). The watch loop reconnects in the background. `/readyz`
  stays at 200 because the snapshot was loaded successfully — disconnected
  doesn't mean unhealthy.
- **`reload` event**: agent re-fetches the snapshot wholesale, replaces its
  cache, and resumes watching.

## Kubernetes example

See [`deploy/kubernetes/40-example-consumer-with-sidecar.yaml`](../deploy/kubernetes/40-example-consumer-with-sidecar.yaml)
in the main repo.

## License

MIT
