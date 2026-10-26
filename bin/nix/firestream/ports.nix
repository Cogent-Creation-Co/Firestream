# Firestream host-port registry
# Copyright Firestream. MIT License.
#
# NOTE: this table is the only place that sets the host port of a compose
# stack. compose.nix publishes each [ stack service containerPort hostPort ]
# row, and ports-lib.nix rejects a duplicate host port, a port outside
# 1024-32767, a published port with no row and a row that no stack uses.
# docs/ports.md must hold the same table; the check prints it on a mismatch.
#
# NOTE: a row whose host port equals its container port holds the canonical
# port, and only the home stack of a service holds it: postgresql owns 5432,
# redis 6379, kafka 9092. Every other row takes a port in its stack's block,
# block + (containerPort mod 1000), so airflow's postgresql 5432 is 11432.
# A block number reads as the stack, and the low digits as the service.
#
# NOTE: every block lies in 11000-28999. From 32768 starts the Linux
# ephemeral range, where an outgoing socket can hold a port for a moment;
# 30000-32767 is the Kubernetes NodePort range; kubelet and containerd
# listen in 10000-10999.
#
# NOTE: no stack owns 9180, the firestream-healthd port. healthd runs in
# every image, so no stack is its home.
#
# NOTE: where two home stacks claim one canonical port, one gets it: 8080
# and 8081 go to spark (not airflow, nginx, the seaweedfs volume server,
# postgres-meta or the jupyterhub hub API), 8000 to the supabase gateway
# (not jupyterhub), 3000 to nextjs (not postgrest or studio), and 4000 to
# realtime (not the supavisor API).
{
  # NOTE: these image keys render another stack's compose project with the
  # same services, so they never run beside it and share its rows. The
  # single service of a one-service stack has the stack's name here.
  stackOf = {
    "odoo-15" = "odoo";
    "odoo-16" = "odoo";
    "odoo-17" = "odoo";
    "odoo-18" = "odoo";
    "postgresql-16" = "postgresql";
    "postgresql-17" = "postgresql";
    "redis-7" = "redis";
    "redis-8" = "redis";
    "supabase-envoy" = "supabase";
  };

  rows = [
    # stack                   service                  container  host
    # ── 11000 airflow ────────────────────────────────────────────────────
    [ "airflow"                "airflow"                 8080     11080 ]
    [ "airflow"                "airflow"                 9180     11180 ]
    [ "airflow"                "postgresql"              5432     11432 ]
    [ "airflow"                "redis"                   6379     11379 ]
    # ── 12000 postgresql ─────────────────────────────────────────────────
    [ "postgresql"             "postgresql"              5432      5432 ]
    [ "postgresql"             "postgresql"              9180     12180 ]
    # ── 13000 redis ──────────────────────────────────────────────────────
    [ "redis"                  "redis"                   6379      6379 ]
    [ "redis"                  "redis"                   9180     13180 ]
    # ── 14000 kafka ──────────────────────────────────────────────────────
    [ "kafka"                  "kafka"                   9092      9092 ]
    [ "kafka"                  "kafka"                   9093      9093 ]
    [ "kafka"                  "kafka"                   9180     14180 ]
    # ── 15000 spark ──────────────────────────────────────────────────────
    [ "spark"                  "spark"                   7077      7077 ]
    [ "spark"                  "spark"                   8080      8080 ]
    [ "spark"                  "spark"                   8081      8081 ]
    [ "spark"                  "spark"                   4040      4040 ]
    [ "spark"                  "spark"                   6066      6066 ]
    [ "spark"                  "spark"                   9180     15180 ]
    # ── 16000 jupyterhub ─────────────────────────────────────────────────
    [ "jupyterhub"             "jupyterhub"              8000     16000 ]
    [ "jupyterhub"             "jupyterhub"              8081     16081 ]
    [ "jupyterhub"             "jupyterhub"              9180     16180 ]
    # ── 17000 superset ───────────────────────────────────────────────────
    [ "superset"               "superset"                8088      8088 ]
    [ "superset"               "superset"                5555      5555 ]
    [ "superset"               "superset"                9180     17180 ]
    [ "superset"               "postgresql"              5432     17432 ]
    [ "superset"               "redis"                   6379     17379 ]
    # ── 18000 odoo ───────────────────────────────────────────────────────
    [ "odoo"                   "odoo"                    8069      8069 ]
    [ "odoo"                   "odoo"                    8072      8072 ]
    [ "odoo"                   "odoo"                    9180     18180 ]
    [ "odoo"                   "postgresql"              5432     18432 ]
    # ── 19000 supabase ───────────────────────────────────────────────────
    [ "supabase"               "api-gw"                  8000      8000 ]
    [ "supabase"               "api-gw"                  9180     19180 ]
    [ "supabase"               "db"                      5432     19432 ]
    [ "supabase"               "supavisor"               6543      6543 ]
    # ── 20000 nextjs ─────────────────────────────────────────────────────
    [ "nextjs"                 "nextjs"                  3000      3000 ]
    [ "nextjs"                 "postgresql"              5432     20432 ]
    # ── 21000 nginx ──────────────────────────────────────────────────────
    [ "nginx"                  "nginx"                   8080     21080 ]
    [ "nginx"                  "nginx"                   9180     21180 ]
    # ── 22000 seaweedfs ──────────────────────────────────────────────────
    [ "seaweedfs"              "seaweedfs"               8333      8333 ]
    [ "seaweedfs"              "seaweedfs"               9333      9333 ]
    [ "seaweedfs"              "seaweedfs"               8888      8888 ]
    [ "seaweedfs"              "seaweedfs"               8080     22080 ]
    [ "seaweedfs"              "seaweedfs"               9324      9324 ]
    # ── 23000 supabase-postgres ──────────────────────────────────────────
    [ "supabase-postgres"      "supabase-postgres"       5432     23432 ]
    [ "supabase-postgres"      "supabase-postgres"       9180     23180 ]
    # ── 24000 supabase-postgres-meta ─────────────────────────────────────
    [ "supabase-postgres-meta" "supabase-postgres-meta"  8080     24080 ]
    [ "supabase-postgres-meta" "supabase-postgres-meta"  9180     24180 ]
    # ── 25000 supabase-postgrest ─────────────────────────────────────────
    [ "supabase-postgrest"     "supabase-postgrest"      3000     25000 ]
    [ "supabase-postgrest"     "supabase-postgrest"      9180     25180 ]
    # ── 26000 supabase-studio ────────────────────────────────────────────
    [ "supabase-studio"        "supabase-studio"         3000     26000 ]
    [ "supabase-studio"        "supabase-studio"         9180     26180 ]
    # ── 27000 supabase-supavisor ─────────────────────────────────────────
    [ "supabase-supavisor"     "supabase-supavisor"      4000     27000 ]
    [ "supabase-supavisor"     "supabase-supavisor"      5432     27432 ]
    [ "supabase-supavisor"     "supabase-supavisor"      6543     27543 ]
    [ "supabase-supavisor"     "supabase-supavisor"      9180     27180 ]
    # ── 28000 the single-port Supabase components: healthd at 28180 + n ──
    [ "supabase-auth"          "supabase-auth"           9999      9999 ]
    [ "supabase-auth"          "supabase-auth"           9180     28180 ]
    [ "supabase-imgproxy"      "supabase-imgproxy"       5001      5001 ]
    [ "supabase-imgproxy"      "supabase-imgproxy"       9180     28181 ]
    [ "supabase-realtime"      "supabase-realtime"       4000      4000 ]
    [ "supabase-realtime"      "supabase-realtime"       9180     28182 ]
    [ "supabase-storage"       "supabase-storage"        5000      5000 ]
    [ "supabase-storage"       "supabase-storage"        9180     28183 ]
  ];
}
