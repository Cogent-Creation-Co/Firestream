"""Pulumi stack for the Firestream Cloudflare-edge GKE example.

Provisions everything the deployment needs that is not a Helm chart:

  * the required Google service APIs
  * a GKE Autopilot cluster (VPC-native, Workload Identity on by default)
  * an Artifact Registry Docker repository for the firestream-* images
  * Secret Manager secrets holding the Odoo admin + Postgres passwords
  * a deployer service account with the IAM Cloud Build needs
  * THE CLOUDFLARE TUNNEL: the tunnel itself, its remotely-managed ingress
    rules, one proxied DNS record per hostname, and the in-cluster Secret
    carrying its token
  * the target namespace, so Helm never has to create it

WHY THIS STACK TOUCHES KUBERNETES AND ../../odoo-gke's DOES NOT.

The other GKE examples materialise their Kubernetes Secrets with a shell script
(`kubectl create secret ... | kubectl apply`) reading values back out of Secret
Manager. That works because a human chose those values in the first place.

A tunnel token is different: Cloudflare mints it when the tunnel is created, so
it exists only as an output of a resource in this stack. Round-tripping it
through a shell script would mean copying a live credential through the
environment for no benefit. Declaring the Secret here keeps the token inside
Pulumi's encrypted state and makes the tunnel and its token a single unit that
is created, rotated and destroyed together.

Once this stack owns the namespace, it owns namespace lifecycle -- which is why
the chart overrides set `_meta.createNamespace = false`. See ../README.md.
"""

import pulumi
import pulumi_cloudflare as cloudflare
import pulumi_gcp as gcp
import pulumi_kubernetes as k8s
import pulumi_random as random

cfg = pulumi.Config()
gcp_cfg = pulumi.Config("gcp")

project = gcp_cfg.require("project")
region = gcp_cfg.get("region") or "us-central1"
cluster_name = cfg.get("clusterName") or "firestream-edge"
ar_repo_id = cfg.get("arRepo") or "firestream"
namespace_name = cfg.get("namespace") or "edge"

# The public hostname, which must match `hostname` in ../config.nix -- nginx
# matches it as `server_name`, and Cloudflare routes it to the tunnel. A
# mismatch produces a tunnel that resolves and an nginx that answers 404.
hostname = cfg.require("hostname")

cf_account_id = cfg.require("cloudflareAccountId")
cf_zone_id = cfg.require("cloudflareZoneId")

# Kubernetes Secret the cloudflared chart reads its token from. Must match
# `tunnelSecretName` in ../config.nix.
tunnel_secret_name = cfg.get("tunnelSecretName") or "cloudflared-tunnel-token"

# --- 1. Service APIs --------------------------------------------------------
service_apis = [
    "container.googleapis.com",
    "artifactregistry.googleapis.com",
    "secretmanager.googleapis.com",
    "cloudbuild.googleapis.com",
]
services = {
    api: gcp.projects.Service(
        api.split(".")[0],
        project=project,
        service=api,
        disable_on_destroy=False,
    )
    for api in service_apis
}

# --- 2. GKE Autopilot cluster ----------------------------------------------
# Autopilot gives VPC-native networking, Workload Identity and per-pod billing
# with no node pools to size. It also mutates every pod to carry resource
# requests, which is why the charts here set them explicitly rather than
# leaving them to whatever Autopilot's minimum happens to be.
cluster = gcp.container.Cluster(
    "cluster",
    name=cluster_name,
    project=project,
    location=region,
    enable_autopilot=True,
    deletion_protection=False,
    opts=pulumi.ResourceOptions(depends_on=[services["container.googleapis.com"]]),
)

# --- 3. Artifact Registry ---------------------------------------------------
ar_repo = gcp.artifactregistry.Repository(
    "images",
    project=project,
    location=region,
    repository_id=ar_repo_id,
    format="DOCKER",
    description="Firestream container images for the Cloudflare-edge example",
    opts=pulumi.ResourceOptions(
        depends_on=[services["artifactregistry.googleapis.com"]]
    ),
)

# --- 4. Odoo credentials in Secret Manager ---------------------------------
# Values come from stack config (`pulumi config set --secret`) so they never
# live in source. scripts/sync-secrets.sh reads them back at deploy time and
# materialises the Kubernetes Secrets the Odoo chart references by name.
secret_ids = {}
for name, config_key in [
    ("odoo-password", "odooPassword"),
    ("odoo-db-password", "dbPassword"),
    ("odoo-db-postgres-password", "dbPostgresPassword"),
]:
    secret = gcp.secretmanager.Secret(
        name,
        project=project,
        secret_id=name,
        replication=gcp.secretmanager.SecretReplicationArgs(
            auto=gcp.secretmanager.SecretReplicationAutoArgs()
        ),
        opts=pulumi.ResourceOptions(
            depends_on=[services["secretmanager.googleapis.com"]]
        ),
    )
    gcp.secretmanager.SecretVersion(
        f"{name}-version",
        secret=secret.id,
        secret_data=cfg.require_secret(config_key),
    )
    secret_ids[name] = secret.secret_id

# --- 5. Deployer service account -------------------------------------------
deployer = gcp.serviceaccount.Account(
    "deployer",
    project=project,
    account_id="firestream-edge-deployer",
    display_name="Firestream Cloudflare-edge deployer",
)
for role in [
    "roles/container.developer",
    "roles/artifactregistry.writer",
    "roles/secretmanager.secretAccessor",
]:
    gcp.projects.IAMMember(
        f"deployer-{role.split('/')[-1]}",
        project=project,
        role=role,
        member=deployer.email.apply(lambda e: f"serviceAccount:{e}"),
    )

# --- 6. The Cloudflare tunnel ----------------------------------------------
# `config_src="cloudflare"` makes this a REMOTELY-MANAGED tunnel: its ingress
# rules live in Cloudflare, declared by the Config resource below, rather than
# in a config.yaml mounted into the pod. That is precisely why the cloudflared
# chart renders no ConfigMap and the pod holds nothing but a token.
tunnel_secret = random.RandomBytes("tunnel-secret", length=32)

tunnel = cloudflare.ZeroTrustTunnelCloudflared(
    "tunnel",
    account_id=cf_account_id,
    name=namespace_name,
    secret=tunnel_secret.base64,
    config_src="cloudflare",
)

# The origin the edge forwards to: the nginx Service, in-cluster. cloudflared
# never needs a Service of its own -- it dials OUT to Cloudflare, and this URL
# is resolved by the connector from inside the namespace.
origin_url = f"http://nginx.{namespace_name}.svc.cluster.local:80"

cloudflare.ZeroTrustTunnelCloudflaredConfig(
    "tunnel-config",
    account_id=cf_account_id,
    tunnel_id=tunnel.id,
    config=cloudflare.ZeroTrustTunnelCloudflaredConfigConfigArgs(
        ingress_rules=[
            cloudflare.ZeroTrustTunnelCloudflaredConfigConfigIngressRuleArgs(
                hostname=hostname,
                service=origin_url,
            ),
            # cloudflared REQUIRES the final rule to be a catch-all with no
            # hostname. Making it an explicit 404 rather than letting it fall
            # into the first rule stops an unmatched Host from silently
            # reaching Odoo.
            cloudflare.ZeroTrustTunnelCloudflaredConfigConfigIngressRuleArgs(
                service="http_status:404",
            ),
        ],
    ),
    opts=pulumi.ResourceOptions(parent=tunnel),
)

# A proxied CNAME to the tunnel. `proxied=True` is what puts the request through
# Cloudflare's edge (and therefore through the tunnel) rather than resolving to
# an address a client could reach directly; `ttl=1` means "automatic" and is
# required when proxied.
cloudflare.Record(
    "dns",
    zone_id=cf_zone_id,
    name=hostname,
    type="CNAME",
    content=tunnel.id.apply(lambda tid: f"{tid}.cfargotunnel.com"),
    proxied=True,
    ttl=1,
    comment=f"firestream cloudflare-edge example ({namespace_name})",
    opts=pulumi.ResourceOptions(parent=tunnel),
)

# --- 7. Namespace + the tunnel-token Secret --------------------------------
def _kubeconfig(name: str, endpoint: str, cluster_ca: str) -> str:
    """A kubeconfig for the cluster above, authenticating via gke-gcloud-auth-plugin.

    Built here rather than shelled out to `gcloud container clusters
    get-credentials` so the provider does not depend on the ambient contents of
    ~/.kube/config -- this stack must talk to the cluster it just created and
    nothing else.
    """
    return f"""apiVersion: v1
kind: Config
clusters:
- name: {name}
  cluster:
    server: https://{endpoint}
    certificate-authority-data: {cluster_ca}
contexts:
- name: {name}
  context:
    cluster: {name}
    user: {name}
current-context: {name}
users:
- name: {name}
  user:
    exec:
      apiVersion: client.authentication.k8s.io/v1beta1
      command: gke-gcloud-auth-plugin
      provideClusterInfo: true
"""


k8s_provider = k8s.Provider(
    "cluster",
    kubeconfig=pulumi.Output.all(
        cluster.name,
        cluster.endpoint,
        cluster.master_auth.cluster_ca_certificate,
    ).apply(lambda args: _kubeconfig(args[0], args[1], args[2])),
    # Pin the provider to this namespace's objects; nothing here should ever
    # touch another namespace.
    namespace=namespace_name,
)

namespace = k8s.core.v1.Namespace(
    "namespace",
    metadata=k8s.meta.v1.ObjectMetaArgs(name=namespace_name),
    opts=pulumi.ResourceOptions(provider=k8s_provider),
)

k8s.core.v1.Secret(
    "tunnel-token",
    metadata=k8s.meta.v1.ObjectMetaArgs(
        name=tunnel_secret_name,
        namespace=namespace_name,
    ),
    string_data={"tunnel-token": tunnel.tunnel_token},
    opts=pulumi.ResourceOptions(provider=k8s_provider, depends_on=[namespace]),
)

pulumi.export("clusterName", cluster.name)
pulumi.export("region", region)
pulumi.export("namespace", namespace_name)
pulumi.export("arHost", f"{region}-docker.pkg.dev")
pulumi.export("arRepo", ar_repo.repository_id.apply(lambda r: f"{project}/{r}"))
pulumi.export("deployerServiceAccount", deployer.email)
pulumi.export("secretIds", secret_ids)
pulumi.export("tunnelId", tunnel.id)
pulumi.export("hostname", hostname)
