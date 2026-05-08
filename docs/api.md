# Ferra HTTP + SSE Protocol

This is the wire contract between the Ferra server and any client (the
`ferra-agent` sidecar, an operator using `curl`, or a service that embeds its
own watch loop). It is intentionally small.

For most service consumers, you should not implement this protocol yourself
— run [`ferra-agent`](../agent/) as a sidecar and read from its localhost
HTTP API. The implementation requirements in §8 are subtle; the agent
already gets them right.

> **No authentication.** Every endpoint accepts any caller that can reach it
> over the network. The trust boundary is the network — see the project
> README for the security model.

---

## 1. Endpoints at a glance

```
GET    /healthz                                # 200 ok always
GET    /readyz                                 # 200 ok / 503 if db is down

GET    /v1/kv/{key}                            # get one key
PUT    /v1/kv/{key}     { "value": <json> }    # set one key
DELETE /v1/kv/{key}                            # delete one key
GET    /v1/kv?prefix=...                       # list (and snapshot) by prefix
GET    /v1/events?since=N&prefix=...&limit=K   # paginated event log
GET    /v1/watch?prefix=...&since=N            # SSE watch (long-lived)
```

No headers required. JSON request bodies use `Content-Type: application/json`.

---

## 2. Keys

- A key is a string of 1–1024 bytes, no NUL.
- `/` is convention for directory-style organization. The server stores keys
  as-is; it does not parse path segments.
- When a key appears in a URL path (`GET /v1/kv/{key}`), percent-encode each
  segment but leave `/` as the segment separator. Example: a key
  `services/payment/timeout ms` becomes `/v1/kv/services/payment/timeout%20ms`.
- When a key or prefix appears as a query parameter (`?prefix=...`),
  percent-encode the whole value including `/`.

---

## 3. Values

- Values are arbitrary JSON: numbers, strings, booleans, null, arrays,
  objects. They are stored as `jsonb` in Postgres.
- Default max serialized size: 256 KiB (`FERRA_MAX_VALUE_BYTES`). Servers
  may be configured lower; over-limit writes return `413 payload_too_large`.
- A client SHOULD store values as opaque JSON in its cache and decode lazily
  in typed getters. Re-encoding on every `get` is wasted work.

---

## 4. Errors

All errors are JSON:

```json
{ "error": "bad_request", "message": "key too long (max 1024 chars)" }
```

| HTTP | `error`              | When                                              |
|------|----------------------|---------------------------------------------------|
| 400  | `bad_request`        | empty key, key > 1024 bytes, NUL in key, etc.     |
| 404  | `not_found`          | `GET`/`DELETE` on a key that does not exist       |
| 413  | `payload_too_large`  | `value` over `FERRA_MAX_VALUE_BYTES`              |
| 500  | `internal_error`     | DB or serialization failure                       |
| 503  | _(no body)_          | `/readyz` only, when DB is unreachable            |

Treat 4xx as "stop, the request is malformed" and 5xx as "transient, retry
with backoff."

---

## 5. KV operations

### 5.1 Get one key

```http
GET /v1/kv/services/payment/timeout_ms
```

```json
{
  "key": "services/payment/timeout_ms",
  "value": 3000,
  "event_id": 43,
  "updated_at": "2026-04-29T12:00:00Z"
}
```

### 5.2 Set one key

```http
PUT /v1/kv/services/payment/timeout_ms
Content-Type: application/json

{ "value": 3000 }
```

```json
{
  "key": "services/payment/timeout_ms",
  "event_id": 43,
  "operation": "set"
}
```

`event_id` is monotonically increasing across the whole instance — it comes
from the `kv_events` Postgres sequence.

### 5.3 Delete one key

```http
DELETE /v1/kv/services/payment/timeout_ms
```

```json
{
  "key": "services/payment/timeout_ms",
  "event_id": 44,
  "operation": "delete"
}
```

Deleting a non-existent key returns `404`, not `204`.

### 5.4 List / snapshot by prefix

```http
GET /v1/kv?prefix=services/payment/
```

```json
{
  "prefix": "services/payment/",
  "latest_event_id": 43,
  "items": [
    { "key": "services/payment/timeout_ms", "value": 3000, "event_id": 43 }
  ]
}
```

This is the snapshot endpoint a client uses on startup and after a `reload`.

> **`latest_event_id` is global, not prefix-scoped.** It is `MAX(id) FROM
> kv_events` across the whole instance, not just events under `prefix`. This is
> intentional: clients use it as the `since=` cursor for `/v1/watch`, and the
> watch must be ordered against the same global event log. Do not assume
> `latest_event_id` corresponds to a key in `items`.

An empty `prefix` matches all keys.

---

## 6. Event log

```http
GET /v1/events?since=42&prefix=services/payment/&limit=1000
```

```json
{
  "from_event_id": 42,
  "to_event_id": 44,
  "events": [
    { "event_id": 43, "key": "services/payment/timeout_ms", "operation": "set" },
    { "event_id": 44, "key": "services/payment/feature_x",  "operation": "delete" }
  ]
}
```

- `since` is **exclusive** (you get events with `event_id > since`).
- `prefix` is optional.
- `limit` defaults to 1000, clamped to `[1, 5000]`.
- `to_event_id` equals the last returned event's id, or `since` if the page is
  empty.

> **Events do not carry `value`.** They tell you that key K was set or
> deleted. To learn the new value of a `set` you must `GET /v1/kv/{key}`. This
> keeps the events table small and avoids storing every historical revision.

A client typically does not need this endpoint — `/v1/watch` covers both
catch-up and live updates. It exists for ad-hoc tooling and tests.

---

## 7. Watch (SSE)

```http
GET /v1/watch?prefix=services/payment/&since=42
Accept: text/event-stream
```

The server replies `200 OK` with `Content-Type: text/event-stream` and then
streams events forever (until the client disconnects, the server is shut down,
or the subscriber lags too far behind the in-memory ring buffer).

Three event types are emitted:

### 7.1 `kv_changed`

Sent for every set / delete that matches `prefix` and has `event_id > since`.

```
event: kv_changed
id: 43
data: {"event_id":43,"key":"services/payment/timeout_ms","operation":"set"}
```

`operation` is `"set"` or `"delete"`. As with `/v1/events`, the `value` is not
included; a client that needs the new value must `GET /v1/kv/{key}`.

### 7.2 `heartbeat`

Sent every `FERRA_WATCH_HEARTBEAT_SECONDS` (default 30s) so idle clients and
load balancers don't close the socket.

```
event: heartbeat
data: {}
```

A client SHOULD use heartbeats to detect a dead connection: if no bytes arrive
for ~2× the heartbeat interval, treat it as a transport error and reconnect.

### 7.3 `reload`

Sent when the server's in-memory event ring buffer has rotated past the
client's `since`, i.e. the client lagged so far it cannot be caught up
incrementally.

```
event: reload
data: {"reason":"lagged"}
```

After `reload` the server closes the stream. The client MUST drop its cache,
re-fetch a full snapshot via `GET /v1/kv?prefix=...`, and reconnect with
`since=<snapshot.latest_event_id>`.

### 7.4 `error` (rare)

A best-effort signal that catch-up failed. Treat it as a transport error:
disconnect, back off, reconnect.

```
event: error
data: {"message":"catch-up failed"}
```

---

## 8. Implementation requirements (the part LLMs get wrong)

These apply to **service-embedded clients** (anything that maintains a
cache and a watch). One-shot `curl` users can ignore them.

If you write a client by hand or generate one, verify each item below. These
are bugs we have seen in the wild.

1. **Reads come from the in-memory cache, not from HTTP.** This is the
   architectural premise. If your `getInt(key)` does an HTTP call to
   `ferra-server` per request, you've defeated the purpose — every config
   read pays a network round-trip and every Ferra hiccup cascades into
   your service.

2. **Re-fetch on `set`.** Watch payloads carry `key` + `operation` only. On
   `set`, `GET /v1/kv/{key}` and update your cache with the returned `value`.
   On `delete`, just remove the key from your cache — no GET needed.

3. **Tolerate 404 on the follow-up GET.** Between receiving a `set` event and
   issuing the GET, another writer may have deleted the key. A 404 is not an
   error; ignore it and let the next event catch you up.

4. **Use the global `latest_event_id` as your `since` cursor.** The snapshot's
   `latest_event_id` is global, not prefix-scoped. Use it verbatim. Don't try
   to derive it from the items in the snapshot.

5. **Advance `since` _after_ applying.** Update your in-memory `latest_event_id`
   only after the cache mutation succeeds. If you advance it first and then
   crash mid-apply, your next reconnect will skip the event.

6. **Heartbeat-driven socket timeout.** Set the read timeout on the watch
   connection to ~2× the server heartbeat interval (default 30s → use 60–90s).
   Without this you will silently die on idle disconnects from NATs and load
   balancers.

7. **`reload` means drop the cache.** When you receive `reload`, do NOT keep
   your existing cache and continue. Issue a fresh `GET /v1/kv?prefix=...`,
   replace the cache wholesale, set `since` to the new `latest_event_id`, then
   reconnect. Failing this, you'll happily run on stale data.

8. **Reconnect with backoff.** On any transport error (TCP reset, idle
   timeout, 5xx), reconnect with exponential backoff (e.g. 500ms → 30s).
   Reset the backoff to the floor on a successful reconnect (any byte
   received, including a heartbeat).

9. **Last-known-good while disconnected.** Reads against the cache MUST NOT
   block on or fail because the watch is disconnected. The whole point of the
   cache is that services keep working while Ferra is briefly unreachable.
   If your service's `getInt` throws or hangs because the watch loop is in
   reconnect backoff, you've defeated the purpose.

10. **Empty prefix means everything.** A client with `prefix=""` will receive
    a snapshot and watch covering every key in the instance. Usually you want
    to scope to a service.

11. **No SSE retry hint.** Ferra does not emit `retry:` lines. Do not parse
    them; if your SSE library auto-reconnects on a `retry:` value, that's
    fine, but don't depend on it.

12. **URL-encode the key path correctly.** `/` separates path segments,
    everything else is percent-encoded. A naive `URLEncoder.encode(key)` that
    encodes `/` as `%2F` will break `GET /v1/kv/{key}`. See §2.

---

## 9. Health endpoints

`GET /healthz` — always returns `200 {"status":"ok"}`. Use for liveness.

`GET /readyz` — `200 {"status":"ok"}` when the database round-trips, else
`503 {"status":"unavailable"}`. Use for readiness probes.

---

## 10. Versioning

The path prefix `/v1/` is the version. Any breaking change to request or
response shape will live under a new prefix (`/v2/`). Additive fields on
existing endpoints are not breaking; clients MUST ignore unknown fields.
