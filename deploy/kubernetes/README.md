# Ferra on Kubernetes

End-to-end deployment example: server + Postgres + sidecar consumer template.

## Files

| File | What it does |
|---|---|
| `00-namespace.yaml` | Creates the `ferra` namespace |
| `20-postgres.yaml` | Single-replica Postgres, **for evaluation only** — use managed Postgres in prod |
| `30-ferra-server.yaml` | The Ferra server Deployment + ClusterIP Service |
| `40-example-consumer-with-sidecar.yaml` | Template for a consumer Pod with `ferra-agent` sidecar |

## Deploy

```bash
# Build + push images (replace registry as needed)
docker build -t ghcr.io/deatherving/ferra:0.1.1       -f Dockerfile       .
docker build -t ghcr.io/deatherving/ferra-agent:0.0.1 -f Dockerfile.agent .
docker push ghcr.io/deatherving/ferra:0.1.1
docker push ghcr.io/deatherving/ferra-agent:0.0.1

# On macOS (ARM) targeting EKS amd64 nodes, use buildx:
# docker buildx build --platform linux/amd64 \
#   -t ghcr.io/deatherving/ferra:0.1.1 -f Dockerfile --push .

# Apply
kubectl apply -f deploy/kubernetes/00-namespace.yaml
kubectl apply -f deploy/kubernetes/20-postgres.yaml         # eval only
kubectl apply -f deploy/kubernetes/30-ferra-server.yaml

kubectl rollout status -n ferra deployment/ferra-server
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
  `FERRA_DATABASE_URL` (or the discrete `FERRA_DATABASE_HOST/NAME/USER/...`
  fields) in `30-ferra-server.yaml`'s ConfigMap at RDS / Cloud SQL /
  Aiven / etc. Move passwords into a `Secret`, never a `ConfigMap`.
- **For AWS RDS / Aurora, prefer IAM authentication.** Set
  `FERRA_DATABASE_IAM_AUTH_ENABLED=true`, `FERRA_DATABASE_HOST`,
  `FERRA_DATABASE_NAME`, `FERRA_DATABASE_USER` (granted the `rds_iam`
  role), and `FERRA_DATABASE_AWS_REGION`. SSL mode defaults to
  `verify-full` automatically in IAM mode — mount the AWS RDS CA bundle
  and point `FERRA_DATABASE_SSL_ROOT_CERT` at it (see "AWS RDS CA bundle"
  below). Bind the pod's ServiceAccount to an IAM role via IRSA; the
  role needs `rds-db:connect` permission for the user. The server mints
  a 15-min auth token and refreshes it every 14 minutes via
  `PgPool::set_connect_options`, so the SQLx pool always has a fresh
  token for new physical connections. No password lives in K8s.
- **Pin image tags.** Use `:0.1.1`, not `:latest`.
- **Don't scale `ferra-server` past 1 replica.** The SSE fan-out is
  per-process (a `tokio::sync::broadcast` channel), so writes to one
  replica don't propagate live to subscribers on another. Watchers would
  only catch up via the `kv_events` table on reconnect, with delay or
  potential gaps. Keep `replicas: 1` until cross-instance fan-out lands.
- **No NetworkPolicy is shipped.** The repo previously included a
  template; it was removed because not every cluster has a CNI that
  enforces NetworkPolicy, and shipping one as a default created false
  confidence. If your cluster supports it, restrict ingress to
  `ferra-server`'s pod selector to the namespaces / pods that actually
  consume Ferra — but write it for your environment, not against a
  generic template.

## AWS RDS CA bundle

For `verify-full` against an RDS or Aurora endpoint, sqlx-postgres needs
the AWS-issued CA chain. It is NOT in the system trust store, so the
server has to be told where to find it.

```bash
# Download once and bake into your image, or mount as a ConfigMap/Secret.
curl -o global-bundle.pem \
  https://truststore.pki.rds.amazonaws.com/global/global-bundle.pem
```

In K8s, the simplest pattern is a ConfigMap mounted into the
`ferra-server` Pod:

```yaml
apiVersion: v1
kind: ConfigMap
metadata:
  name: rds-ca-bundle
  namespace: ferra
binaryData:
  global-bundle.pem: |
    <base64-encoded contents of global-bundle.pem>
---
# In the ferra-server Deployment:
spec:
  template:
    spec:
      containers:
        - name: ferra-server
          env:
            - name: FERRA_DATABASE_SSL_ROOT_CERT
              value: /etc/rds-ca/global-bundle.pem
          volumeMounts:
            - name: rds-ca
              mountPath: /etc/rds-ca
              readOnly: true
      volumes:
        - name: rds-ca
          configMap:
            name: rds-ca-bundle
```

If `FERRA_DATABASE_SSL_MODE=verify-ca` / `verify-full` is set but
`FERRA_DATABASE_SSL_ROOT_CERT` is not, the server logs a warning at
startup and falls back to the system trust store. For RDS / Aurora that
fallback typically fails — set the env var.

## IRSA setup for RDS IAM auth (EKS specific)

When using IAM mode on EKS, the `ferra-server` pod needs an AWS-side
identity that's allowed to call `rds-db:connect`. The standard pattern
is IRSA — IAM Roles for Service Accounts.

Sketch (one-time, per cluster):

```bash
# 1. Create an IAM policy that grants rds-db:connect for the IAM DB user
cat > /tmp/ferra-rds-iam.json <<'POLICY'
{
  "Version": "2012-10-17",
  "Statement": [{
    "Effect": "Allow",
    "Action": "rds-db:connect",
    "Resource": "arn:aws:rds-db:us-west-2:123456789012:dbuser:cluster-XXXXX/ferra_iam"
  }]
}
POLICY
aws iam create-policy --policy-name ferra-rds-iam --policy-document file:///tmp/ferra-rds-iam.json

# 2. Create the IAM role with a trust policy for the cluster's OIDC provider,
#    attached to the policy above. (eksctl makes this one command:)
eksctl create iamserviceaccount \
  --cluster my-cluster \
  --namespace ferra \
  --name ferra-server \
  --attach-policy-arn arn:aws:iam::123456789012:policy/ferra-rds-iam \
  --approve
```

Then reference the ServiceAccount in `30-ferra-server.yaml`'s pod spec:

```yaml
spec:
  template:
    spec:
      serviceAccountName: ferra-server      # ← created by eksctl above
      containers:
        - name: ferra-server
          ...
```

Inside the DB, grant the role to the IAM user (one-time, run as superuser):

```sql
CREATE USER ferra_iam;
GRANT rds_iam TO ferra_iam;
GRANT ALL ON DATABASE ferra TO ferra_iam;
```

That's the only one-time setup. Token refresh is fully automatic on
the server side after that.
