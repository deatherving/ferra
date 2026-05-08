# ferra-server

HTTP + SSE configuration server for
[Ferra](https://github.com/deatherving/ferra), a lightweight Postgres-backed
configuration center.

## Install

```bash
cargo install ferra-server
```

Or build the container image from the
[`Dockerfile`](https://github.com/deatherving/ferra/blob/main/Dockerfile)
in the main repo.

## Run

```bash
export FERRA_DATABASE_URL=postgres://user:pass@host:5432/ferra
ferra-server
```

Postgres migrations run automatically at startup. The server exposes:

```http
GET    /healthz                                # liveness, always 200
GET    /readyz                                 # readiness, 200 / 503

GET    /v1/kv/{key}
PUT    /v1/kv/{key}     { "value": <json> }
DELETE /v1/kv/{key}
GET    /v1/kv?prefix=...
GET    /v1/events?since=N&prefix=...
GET    /v1/watch?prefix=...&since=N            # SSE
```

There is no application-layer authentication. The server trusts whoever can
reach it on the network — deploy with a `ClusterIP` Service and a
`NetworkPolicy`, or behind a firewall.

Full protocol contract:
[`docs/api.md`](https://github.com/deatherving/ferra/blob/main/docs/api.md).

## License

MIT
