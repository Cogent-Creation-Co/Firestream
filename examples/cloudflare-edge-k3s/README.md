# Firestream Cloudflare edge — local k3s / k3d

Odoo behind an nginx reverse proxy behind a Cloudflare Tunnel connector, in one
namespace on a laptop cluster.

```
Cloudflare edge ──▶ cloudflared Deployment ──▶ nginx Service :80 ──▶ odoo :8069  (HTTP)
   (outbound-only tunnel)                       (per-namespace proxy)  odoo :8072  (websocket)
```

This is the **first multi-chart example**. Every other example in this directory
deploys a single Helm release; this one composes three, and most of what it has
to teach is about how they agree with each other.

---

## What this example is for

Two things you cannot see from a single-chart example:

1. **Charts that must agree.** nginx's `upstreams` names Odoo's Service and its
   two ports; Odoo has to actually be running in the mode that binds them. That
   agreement is not enforced by any type — it is a property of the deployment,
   so `config.nix` is the single place the shared facts live.
2. **Ingress with no Ingress object.** There is no LoadBalancer, no
   `Ingress`, no cert-manager and no public port. The connector dials *out* to
   Cloudflare and traffic arrives back through that tunnel, which is why the
   cloudflared chart renders no Service at all.

## The one thing that cannot be real locally

`cloudflared` needs a **tunnel token** — a credential Cloudflare issues when a
tunnel is created, which both identifies the tunnel and authorises connectors to
register against it. Without a Cloudflare account there is no tunnel and no
token.

`scripts/deploy-local.sh` therefore writes a **syntactically valid fake token**
into the Secret. The connector starts, parses it, tries to reach the Cloudflare
edge, and never reports Ready.

**That is the expected outcome, and it is deliberate.** The alternative —
stubbing the connector out, or dropping it from the local example — would mean
the chart is never exercised at all. This way the pod spec, the Secret wiring,
the probes and the deploy ordering are all real, and the single thing that
genuinely requires a Cloudflare account fails honestly and visibly. Everything
*behind* the connector is fully testable; `make smoke` does exactly that.

To run it for real, put a real token in that Secret and it will go Ready — the
chart needs no other change.

## Quick start

```bash
# a throwaway cluster, if you don't already have one
k3d cluster create edge-demo
export K3D_CLUSTER=edge-demo

make deploy     # build + side-load images, create the token Secret, deploy all three
make smoke      # prove the routing works
make status
```

Expect `cloudflared` to sit at `0/1 Running`. Everything else reaches Ready.

## The three overrides

| File | What it decides |
|---|---|
| `firestream/odoo-overrides.nix` | `workers = 2` — the setting that makes 8072 exist at all |
| `firestream/nginx-overrides.nix` | `upstreams` — the hostname, the default backend, and the `/websocket` route |
| `firestream/cloudflared-overrides.nix` | the name of one Secret, and nothing else |

Each is a strict subset of that chart's own value surface. No chart templates
are forked and no image wiring is restated.

### Why `workers = 2` is the whole websocket story

Odoo has two server modes that expose **different sockets**:

| `workers` | mode | what binds 8072 |
|---|---|---|
| `0` (chart default) | threaded — one process on 8069, websockets served there too | **nothing** |
| `> 0` | prefork — HTTP workers plus a separate gevent worker | the gevent worker |

So a proxy that peels `/websocket` off to 8072 — the standard Odoo-behind-nginx
layout, and what `upstreams.<name>.routes` generates — is broken unless Odoo is
in prefork mode. The failure is nasty: pages load fine and only live-update
features break.

Setting `workers` is all it takes. The chart gates the container port, the
Service port and the NetworkPolicy ingress rule on that one value, so they
cannot drift apart.

The container has its own `config.odoo.workers` for the docker-compose loop,
where nothing splits ports and one process is cheaper. The chart's env wins over
the image's baked default, so this stays a per-*deployment* choice.

### Deploy order, and how to stop caring about it

`scripts/deploy-local.sh` deploys **odoo → nginx → cloudflared**.

Only the first edge is load-bearing, and only with the default configuration:
with `clusterDns` empty, nginx resolves every `proxy_pass` name **once at config
load** and refuses to start with `host not found in upstream` if the Service is
missing. Deploying Odoo first guarantees it exists.

Set `clusterDns` in `config.nix` to your cluster's kube-dns ClusterIP
(`10.43.0.10` on k3s/k3d) to switch to request-time resolution. nginx then boots
with nothing behind it and answers 502 until the backend appears — much better
behaviour for an edge proxy, and it makes the ordering a convenience rather than
a requirement. That also turns on `qualifyUpstreams`, which is not optional
alongside a resolver: nginx's resolver does not apply `/etc/resolv.conf` search
domains, so the bare `odoo` has to become
`odoo.<namespace>.svc.cluster.local`. The chart does that expansion when it
renders, which is the first point at which the namespace is known.

cloudflared genuinely does not care where it falls in the order — its readiness
depends only on the Cloudflare edge, and the edge resolves the tunnel's ingress
rules at request time. Last simply means the tunnel goes live once there is
something to reach.

### The Cloudflare → nginx hop, and why it is the one thing this example cannot test

Everything from nginx inwards is exercised locally. The hop *into* nginx is not:
`deploy-local.sh` writes a syntactically valid **fake** token, so the connector
never registers and never forwards a request. That hop is configured entirely on
the Cloudflare side — this chart has no `originRequest` surface and deliberately
never will, because remotely-managed tunnels keep their ingress rules in the
Cloudflare API (see `nix/options/tunnel.nix`). Two couplings therefore live only
here, in prose:

**1. The ingress rule.** The tunnel needs exactly one rule per hostname:

```
hostname: <cfg.hostname>            # e.g. odoo.example.test
service:  http://nginx.<namespace>.svc.cluster.local:80
```

plus an `http_status:404` catch-all last. Port **80** and the in-cluster
`nginx` Service DNS name — not Odoo's, not 8069. nginx is what does hostname
matching and what splits `/websocket` off to the gevent port; routing the tunnel
straight at Odoo skips both.

**2. Leave `originRequest` at its defaults. In particular, do NOT set
`httpHostHeader`.** nginx routes purely on `server_name <hostname>`, and the
chart's default server answers `404 no upstream configured for this host` to
anything that does not match. `httpHostHeader` rewrites the `Host` header on the
way out of the connector, so setting it to *anything* other than the public
hostname makes **every** request fall through to that default server and 404 —
with the tunnel healthy, nginx healthy and Odoo healthy, which is about the most
misleading failure mode available. The remaining defaults are fine and worth
stating so nobody "fixes" them: `connectTimeout` 30s (nginx accepts
immediately), `keepAliveTimeout` 90s (above nginx's 75s `keepalive_timeout`, so
nginx closes idle connections first, which is the correct direction), and
`noTLSVerify` irrelevant because the hop is plaintext HTTP inside the cluster.

## Going to production

The differences are all *outside* the charts:

- **The tunnel.** Create a `ZeroTrustTunnelCloudflared` with
  `config_src = "cloudflare"` (remotely-managed, which is why the chart ships no
  ConfigMap), a `ZeroTrustTunnelCloudflaredConfig` mapping each hostname to
  `http://nginx.<namespace>.svc.cluster.local:80` with an `http_status:404`
  catch-all last, and one proxied CNAME per hostname pointing at
  `<tunnel-id>.cfargotunnel.com`. Write the tunnel's token into the Secret this
  example fakes. Leave `origin_request` alone — see the hop section above, and
  above all do not set `http_host_header`.
- **Namespace ownership.** When the deploying layer creates the namespace —
  along with the ResourceQuota, LimitRange, NetworkPolicies and Workload
  Identity ServiceAccount that belong with it — set
  `_meta.createNamespace = false` on each chart so Helm cannot race that by
  conjuring a bare, unquota'd namespace.
- **NetworkPolicy egress.** A default-deny namespace must allow the connector
  out on **443 and 7844, TCP *and* UDP**. Miss the UDP rules and the tunnel
  silently falls back to a slower transport, or fails.
- **Replicas.** Leave cloudflared at the chart's default of 2. Cloudflare
  load-balances a tunnel across every connector registered against it, so two
  pods give a genuinely redundant edge. This example drops it to 1 only because
  a second not-Ready pod on a laptop teaches nothing.

## Files

```
config.nix                              your settings — namespace, hostname, credentials
flake.nix                               three charts.<app>.eval calls + the images to side-load
firestream/odoo-overrides.nix           the app
firestream/nginx-overrides.nix          the proxy and its upstreams
firestream/cloudflared-overrides.nix    the connector
scripts/deploy-local.sh                 build → side-load → Secret → deploy ×3
scripts/smoke.sh                        differential probes proving the routing
Makefile                                charts / render / deploy / smoke / status / destroy
```
