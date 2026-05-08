# Ferra on Kubernetes

End-to-end deployment example: server + Postgres + sidecar consumer template
+ NetworkPolicy lockdown.

## Files

| File | What it does |
|---|---|
| `00-namespace.yaml` | Creates the `ferra` namespace |
| `20-postgres.yaml` | Single-replica Postgres, **for evaluation only** — use managed Postgres in prod |
| `30-ferra-server.yaml` | The Ferra server Deployment + ClusterIP Service |
| `35-network-policy.yaml` | Restricts ingress to namespaces labeled `ferra-client=true` |
| `40-example-consumer-with-sidecar.yaml` | Template for a consumer Pod with `ferra-agent` sidecar |

## Deploy

```bash
# Build + push images (replace registry as needed)
docker build -t ghcr.io/deatherving/ferra:0.1.0       -f Dockerfile       .
docker build -t ghcr.io/deatherving/ferra-agent:0.1.0 -f Dockerfile.agent .
docker push ghcr.io/deatherving/ferra:0.1.0
docker push ghcr.io/deatherving/ferra-agent:0.1.0

# Apply
kubectl apply -f deploy/kubernetes/00-namespace.yaml
kubectl apply -f deploy/kubernetes/20-postgres.yaml         # eval only
kubectl apply -f deploy/kubernetes/30-ferra-server.yaml
kubectl apply -f deploy/kubernetes/35-network-policy.yaml

kubectl rollout status -n ferra deployment/ferra-server

# For each consumer namespace, allow it through the NetworkPolicy:
kubectl label namespace payment   ferra-client=true
kubectl label namespace matching  ferra-client=true
```

## Smoke test from your laptop

```bash
kubectl port-forward -n ferra svc/ferra-server 8080:8080 &

curl -X PUT -H 'Content-Type: application/json' \
  -d '{"value":3000}' \
  http://localhost:8080/v1/kv/services/payment/timeout_ms

curl http://localhost:8080/v1/kv/services/payment/timeout_ms
# {"key":"services/payment/timeout_ms","value":3000,"event_id":1,...}
```

## Deploy a consumer with sidecar

Copy `40-example-consumer-with-sidecar.yaml` into your service's repo and
edit:

- `payment-service` → your service name + image
- `services/payment/` → the prefix(es) your service reads
- agent image tag → your built version

The template uses **Kubernetes 1.29+ native sidecars** (`initContainers`
with `restartPolicy: Always`). For earlier Kubernetes, move the agent block
to the regular `containers` list and rely on the consumer's `startupProbe`
hitting the agent's `/readyz` to gate traffic until snapshots load.

Verify after applying:

```bash
# Agent's snapshots loaded?
kubectl exec -n payment deploy/payment-service -c ferra-agent -- \
  wget -qO- http://127.0.0.1:9999/readyz
# {"status":"ok"}

# Service can read config via the agent?
kubectl exec -n payment deploy/payment-service -c payment-service -- \
  wget -qO- http://localhost:9999/cfg/services/payment/timeout_ms
# 3000
```

## Production notes

- **Use managed Postgres.** Skip `20-postgres.yaml` and point
  `FERRA_DATABASE_URL` in `30-ferra-server.yaml`'s ConfigMap at RDS / Cloud
  SQL / Aiven / etc.
- **Apply `35-network-policy.yaml`.** Without it, any pod in any namespace
  can reach `ferra-server` and read or rewrite your config. Requires a CNI
  that enforces NetworkPolicy (Calico, Cilium, or AWS VPC CNI with
  `enableNetworkPolicy: true`).
- **Pin image tags.** Use `:0.1.0`, not `:latest`.
- **Don't scale `ferra-server` past 1 replica.** The SSE fan-out is
  per-process (a `tokio::sync::broadcast` channel), so writes to one
  replica don't propagate live to subscribers on another. Watchers would
  only catch up via the `kv_events` table on reconnect, with delay or
  potential gaps. Keep `replicas: 1` until cross-instance fan-out lands.
