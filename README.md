# Ferra

A lightweight open-source configuration center built in Rust.

Ferra stores `key -> value` configuration in Postgres. Services read it via a
sidecar agent that holds an in-memory cache, kept fresh over Server-Sent
Events. Operators change values with a small HTTP API.

Ferra is **not** a distributed KV database. It does not replace etcd,
Consul, Redis, service discovery, service meshes, or feature-flag platforms.

## Components

```text
ferra/
  server/            # ferra-server: HTTP + SSE API, persists to Postgres
  agent/             # ferra-agent:  sidecar, in-memory cache + localhost HTTP for nearby services
  deploy/
    docker-compose/  # local dev (server + Postgres in one command)
    kubernetes/      # production manifests (server, NetworkPolicy, sidecar template)
  docs/
    api.md           # the HTTP + SSE wire protocol
```

## Architecture

```
                                writes
operator / CI ─────── HTTP ─────────────►  ferra-server  ──── SQL ────►  Postgres
                                                │
                                                │ SSE watch
                                                ▼
   service container ─── HTTP localhost ───►  ferra-agent  (sidecar, in-memory cache)
   (your app code)                              │
                                                │ keeps cache fresh, ~100ms after writes
                                                ▼
                                          last-known-good when ferra-server unreachable
```

Service code never speaks SSE; it does `curl http://localhost:9999/cfg/foo`.

## Security

There is no application-layer authentication. Anyone who can reach
`ferra-server` over the network can read and write every key. The trust
boundary is the network:

- **Kubernetes**: `ClusterIP` Service + `NetworkPolicy` allowlist.
- **Single VM**: firewall / security group on port 8080.
- **Public internet**: don't. Wrap in an authenticating reverse proxy first.

Don't put secrets (API keys, passwords) in Ferra. Use K8s Secrets / Vault /
AWS Secrets Manager for those. Ferra is for the values you actively *want*
to be live-tunable: timeouts, feature flags, business rules, allow/deny lists.

## Install

```bash
cargo install ferra-server     # binary: ferra-server
cargo install ferra-agent      # binary: ferra-agent
```

Or build container images:

```bash
docker build -t ghcr.io/deatherving/ferra:0.1.0       -f Dockerfile       .
docker build -t ghcr.io/deatherving/ferra-agent:0.1.0 -f Dockerfile.agent .
```

Multi-arch (ARM dev → amd64 deploy):

```bash
docker buildx build --platform linux/amd64,linux/arm64 \
  -t ghcr.io/deatherving/ferra:0.1.0 -f Dockerfile --push .
docker buildx build --platform linux/amd64,linux/arm64 \
  -t ghcr.io/deatherving/ferra-agent:0.1.0 -f Dockerfile.agent --push .
```

Replace the registry path with your own. The build context must be the repo
root for both images (the workspace `Cargo.toml` requires every member's
manifest to be present).

## Local quickstart

```bash
git clone https://github.com/deatherving/ferra
cd ferra/deploy/docker-compose
docker compose up --build
```

Talk to it with `curl`:

```bash
export FERRA=http://localhost:8080

curl -X PUT -H 'Content-Type: application/json' \
  -d '{"value":3000}' \
  $FERRA/v1/kv/services/payment/timeout_ms

curl $FERRA/v1/kv/services/payment/timeout_ms
curl "$FERRA/v1/kv?prefix=services/payment/"
curl --no-buffer "$FERRA/v1/watch?prefix=services/payment/&since=0"
```

## Deploy to Kubernetes

Full walkthrough: [`deploy/kubernetes/README.md`](deploy/kubernetes/README.md).

```bash
kubectl apply -f deploy/kubernetes/00-namespace.yaml
kubectl apply -f deploy/kubernetes/20-postgres.yaml      # eval only; use managed pg in prod
kubectl apply -f deploy/kubernetes/30-ferra-server.yaml
kubectl apply -f deploy/kubernetes/35-network-policy.yaml

kubectl rollout status -n ferra deployment/ferra-server

# label each consumer namespace so the NetworkPolicy lets it through
kubectl label namespace payment ferra-client=true
```

For each consumer service, copy
[`40-example-consumer-with-sidecar.yaml`](deploy/kubernetes/40-example-consumer-with-sidecar.yaml)
into its Deployment. Declare the prefixes it reads in the sidecar's env:

```yaml
- name: FERRA_PREFIX
  value: "services/payment/,flags/global/"
```

The service then reads config via `http://localhost:9999/cfg/{key}` — no SSE,
no reconnect, no cache logic in service code.

## HTTP API

```http
GET    /healthz                                # liveness
GET    /readyz                                 # readiness (db reachable?)

GET    /v1/kv/{key}
PUT    /v1/kv/{key}            { "value": <json> }
DELETE /v1/kv/{key}
GET    /v1/kv?prefix=...
GET    /v1/events?since=N&prefix=...
GET    /v1/watch?prefix=...&since=N            # SSE
```

Full contract — request/response shapes, error codes, watch event types,
and the implementation requirements for custom clients — is in
[`docs/api.md`](docs/api.md).

## Server configuration

| Variable                        | Default            | Purpose                              |
|---------------------------------|--------------------|--------------------------------------|
| `FERRA_DATABASE_URL`            | _required_         | Postgres connection string           |
| `FERRA_HTTP_ADDR`               | `0.0.0.0:8080`     | HTTP listen address                  |
| `FERRA_MAX_VALUE_BYTES`         | `262144` (256 KiB) | Max serialized value size            |
| `FERRA_WATCH_HEARTBEAT_SECONDS` | `30`               | SSE heartbeat interval               |

## Environments

One Ferra instance per runtime environment (prod / staging / dev), each with
its own Postgres database. There is no environment field in the data model
— mixing environments in one instance is not supported.

## License

MIT — see `LICENSE`.
