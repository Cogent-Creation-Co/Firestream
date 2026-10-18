# Firestream Cloudflare edge — GKE Autopilot

Odoo behind an nginx reverse proxy behind a **real** Cloudflare Tunnel, on a GKE
Autopilot cluster.

```
Cloudflare edge ──▶ cloudflared Deployment ──▶ nginx Service :80 ──▶ odoo :8069  (HTTP)
   (outbound-only tunnel)                       (per-namespace proxy)  odoo :8072  (websocket)
```

The production shape of [`../cloudflare-edge-k3s`](../cloudflare-edge-k3s) — the
same three charts through the same `charts.<app>.eval` seams. What changes is
everything *around* them: a real tunnel, real DNS, and a Pulumi stack that owns
the namespace.

---

## The headline: there is no Ingress

No `Ingress` object, no LoadBalancer, no managed certificate, no public port,
and no inbound firewall rule. The only path into the namespace is a tunnel the
connector dials **out** to Cloudflare — which is why the cloudflared chart
renders no Service at all, and why the cluster needs no externally reachable
address.

TLS terminates at the Cloudflare edge. The request reaches nginx over plain
HTTP, so `$scheme` inside nginx is always `http`; the chart forwards the edge's
`X-Forwarded-Proto` rather than `$scheme`. That matters because the Odoo image
bakes `proxy_mode = True` and therefore *trusts* that header — get it wrong and
every absolute URL Odoo generates (password-reset links, portal links, outbound
mail) points at `http://`.

## Setup

```bash
# 0. Auth
gcloud auth login && gcloud auth application-default login

# 1. Configure the stack — see pulumi/Pulumi.dev.example.yaml for every key.
#    The Cloudflare API token needs `Account:Cloudflare Tunnel:Edit` and
#    `Zone:DNS:Edit`.
cd pulumi && pulumi stack init dev
pulumi config set gcp:project my-gcp-project
pulumi config set hostname    odoo.example.com
pulumi config set cloudflareAccountId <id>
pulumi config set cloudflareZoneId    <id>
pulumi config set --secret cloudflare:apiToken <token>
pulumi config set --secret odooPassword       "$(openssl rand -base64 24)"
pulumi config set --secret dbPassword         "$(openssl rand -base64 24)"
pulumi config set --secret dbPostgresPassword "$(openssl rand -base64 24)"
cd ..

# 2. Create the infrastructure (cluster, registry, secrets, tunnel, DNS, namespace)
make pulumi-up

# 3. Copy the stack outputs into config.nix, and read the live cluster DNS:
kubectl -n kube-system get svc kube-dns -o jsonpath='{.spec.clusterIP}'

# 4. Build + push images, sync secrets, deploy all three charts
make deploy
```

Then `https://odoo.example.com`.

## Four values must agree across the two sides

`config.nix` and the Pulumi stack name the same objects from opposite
directions, and nothing checks that they match:

| `config.nix` | Pulumi config | What breaks if they differ |
|---|---|---|
| `namespace` | `namespace` | charts deploy into a namespace with no token Secret |
| `hostname` | `hostname` | the tunnel resolves; nginx answers 404 to everything |
| `tunnelSecretName` | `tunnelSecretName` | connector stuck in `CreateContainerConfigError` |
| `clusterDns` | *(read from the cluster)* | nginx cannot resolve `odoo`; every request 502s |

## What Pulumi owns, and why it reaches into Kubernetes

The other `*-gke` examples create their Kubernetes Secrets with a shell script
that reads values back out of Secret Manager. This one does that too — for
Odoo's passwords — but the **tunnel token** is different: Cloudflare mints it
when the tunnel is created, so it exists only as an output of a Pulumi resource.
Round-tripping it through a shell script would mean copying a live credential
through the environment for no benefit. Declaring the Secret in the stack keeps
it inside Pulumi's encrypted state and makes the tunnel and its token a single
unit, created, rotated and destroyed together.

Because the stack creates the Namespace, it owns namespace lifecycle — so all
three chart overrides set `_meta.createNamespace = false`. Helm conjuring a
bare, unquota'd namespace would race the quotas and policies that belong with
it.

## Things that are easy to get wrong

**`workers = 2` on Odoo is load-bearing.** Odoo binds its gevent port (8072)
only in prefork mode. With the chart default of `0` it runs threaded, nothing
listens on 8072, and nginx's `/websocket` route is a 502 — pages load fine and
only live-update features break. One setting covers it: the chart gates the
container port, the Service port and the NetworkPolicy ingress rule on the same
value.

**NetworkPolicy egress needs UDP.** In a default-deny namespace the connector
must be allowed out on **443 and 7844, TCP *and* UDP**. Miss the UDP rules and
the tunnel silently falls back to a slower transport, or fails outright.

**The catch-all ingress rule must be last and must be explicit.** cloudflared
requires the final rule to have no hostname; `pulumi/__main__.py` makes it
`http_status:404` so an unmatched Host cannot fall through into the first rule
and reach Odoo.

**Hostname depth.** Cloudflare Universal SSL covers one label
(`odoo.example.com`), not two (`odoo.staging.example.com`). A deeper hostname
needs an Advanced Certificate.

**`clusterDns` has no correct default.** GKE auto-allocates the Services CIDR —
recent Autopilot clusters land near `34.118.224.10`, older ones at
`10.96.0.10`. Read the live value rather than guessing.

## Files

```
config.nix                              your settings — must agree with the Pulumi stack
flake.nix                               three charts.<app>.eval calls + the images to push
firestream/odoo-overrides.nix           the app
firestream/nginx-overrides.nix          the proxy and its upstreams
firestream/cloudflared-overrides.nix    the connector — one Secret name
pulumi/__main__.py                      cluster, registry, secrets, tunnel, DNS, namespace
scripts/deploy-local.sh                 build → push → sync secrets → deploy ×3
scripts/sync-secrets.sh                 Secret Manager → Kubernetes (NOT the tunnel token)
cloudbuild.yaml                         the same flow as a Cloud Build pipeline
```
