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

Three Postgres connection modes are supported. Pick one — they're mutually
exclusive.

### 1. URL mode (simplest, backwards-compatible)

```bash
export FERRA_DATABASE_URL=postgres://user:pass@host:5432/ferra
ferra-server
```

### 2. Discrete-fields mode (no URL, explicit fields)

```bash
export FERRA_DATABASE_HOST=db.example.com
export FERRA_DATABASE_PORT=5432              # optional, default 5432
export FERRA_DATABASE_NAME=ferra
export FERRA_DATABASE_USER=ferra_user
export FERRA_DATABASE_PASSWORD=s3cret
export FERRA_DATABASE_SSL_MODE=require       # optional, default prefer
ferra-server
```

### 3. AWS RDS / Aurora IAM authentication mode

For RDS or Aurora PostgreSQL instances with IAM database authentication
enabled. The server mints a 15-minute auth token via the AWS SDK and
refreshes it in the background (default every 14 minutes) so the SQLx pool
always has a valid token when it needs to open new physical connections.

```bash
export FERRA_DATABASE_IAM_AUTH_ENABLED=true
export FERRA_DATABASE_HOST=ferra-prod.cluster-xyz.us-west-2.rds.amazonaws.com
export FERRA_DATABASE_NAME=ferra
export FERRA_DATABASE_USER=ferra_iam         # DB user with rds_iam role
export FERRA_DATABASE_SSL_MODE=require       # RDS IAM requires TLS
export FERRA_DATABASE_AWS_REGION=us-west-2
# Optional: tighten or loosen the refresh cadence (default 840s = 14min,
# must be < 900s since IAM tokens expire at 15 minutes):
# export FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS=600
ferra-server
```

AWS credentials are picked up automatically by the SDK from the usual
chain: IRSA / EC2 instance profile / ECS task role / `AWS_*` env vars /
`~/.aws/credentials`. The DB user needs the `rds_iam` PostgreSQL role
granted in the database.

How the refresh handles the 15-minute token TTL: at each tick the server
regenerates the token, builds new `PgConnectOptions`, and calls
`PgPool::set_connect_options(new_opts)`. That call affects only **future**
physical connections — existing in-flight connections keep using their
original token until they're rotated by `max_lifetime` (10 min in IAM
mode) or returned to the pool naturally. New connections (pool growth,
reconnect after idle, etc.) always use the freshest token. Refresh
failures are logged and retried on the next tick; the pool is never
closed.

## Server configuration

| Variable | Default | Purpose |
|---|---|---|
| `FERRA_DATABASE_URL` | _(none)_ | Postgres connection URL. Takes precedence over discrete fields; not usable with IAM. |
| `FERRA_DATABASE_HOST` | _(none)_ | Required in discrete/IAM modes. |
| `FERRA_DATABASE_PORT` | `5432` | |
| `FERRA_DATABASE_NAME` | _(none)_ | Required in discrete/IAM modes. |
| `FERRA_DATABASE_USER` | _(none)_ | Required in discrete/IAM modes. |
| `FERRA_DATABASE_PASSWORD` | _(none)_ | Required in discrete (non-IAM) mode. |
| `FERRA_DATABASE_SSL_MODE` | `prefer` | `disable` / `allow` / `prefer` / `require` / `verify-ca` / `verify-full`. IAM mode forbids `disable`. |
| `FERRA_DATABASE_IAM_AUTH_ENABLED` | `false` | Set to `true` to enable RDS IAM auth. |
| `FERRA_DATABASE_AWS_REGION` | _(none)_ | Required when IAM auth is enabled. |
| `FERRA_DATABASE_IAM_TOKEN_REFRESH_INTERVAL_SECS` | `840` (14 min) | Must be `< 900` (token TTL is 15 minutes). |
| `FERRA_HTTP_ADDR` | `0.0.0.0:8080` | HTTP listen address. |
| `FERRA_MAX_VALUE_BYTES` | `262144` (256 KiB) | Max serialized value size. |
| `FERRA_WATCH_HEARTBEAT_SECONDS` | `30` | SSE heartbeat interval. |

Postgres migrations run automatically at startup.

## HTTP endpoints

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

There is no application-layer authentication. The server trusts whoever
can reach it on the network — deploy with a `ClusterIP` Service and a
`NetworkPolicy`, or behind a firewall.

Full protocol contract:
[`docs/api.md`](https://github.com/deatherving/ferra/blob/main/docs/api.md).

## License

MIT
