# Host ports of the compose stacks

Each Firestream docker-compose stack publishes its container ports on the host.
One file sets every host port: `bin/nix/firestream/ports.nix`. No stack adds an
offset. Every stack can run at the same time, because no two rows use the same
host port.

## How to read the table

- A row has a stack, a compose service, a container port and a host port.
- "Canonical" means that the host port equals the container port. Only the
  home stack of a service gets its canonical port. For example, the
  `postgresql` stack gets 5432, and the `redis` stack gets 6379.
- Every other row gets a port in the block of its stack. The rule is
  block + (container port mod 1000). For example, the `airflow` block is 11000,
  so the airflow postgresql port 5432 becomes 11432.
- No stack gets 9180, the `firestream-healthd` port, because no stack is its
  home.
- The four single-port Supabase component stacks share block 28000. Their
  healthd ports are 28180 to 28183.
- The variants of an image use the rows of their base stack. For example,
  `odoo-15` uses the `odoo` rows, and `postgresql-16` uses the `postgresql`
  rows. A variant has the same compose project as its base, so the two do not
  run at the same time.

## Rules

`checks.<system>.firestream-port-registry` makes sure of these rules at
evaluation time:

- No host port is in two rows.
- Every host port is in the range 1024 to 32767.
- Every published port of every compose stack has a row.
- No row is stale. Each row is a port that a compose stack publishes.
- The table below is the same as the table in the registry.

All blocks are in the range 11000 to 28999. Do not use 32768 and higher: this is
the Linux ephemeral range, and an outgoing socket can hold a port there for a
short time. Do not use 30000 to 32767: this is the Kubernetes NodePort range.
Do not use 10000 to 10999: kubelet and containerd listen there.

Some canonical ports can be in use on a developer host. For example, macOS
uses 5000 for AirPlay, and a local PostgreSQL uses 5432. The e2e port
preflight stops a run at once and names the holder of the port.

## Change a port

1. Edit the row in `bin/nix/firestream/ports.nix`.
2. Run `nix eval --raw .#legacyPackages.x86_64-linux.portRegistry.markdownTable`.
3. Put the output between the markers below.
4. Run `nix build .#checks.x86_64-linux.firestream-port-registry`.

To publish a new container port, add it to the compose service (or to
`exposedPorts`) and add its row. The check fails until both are present.

## The table

<!-- port-table:begin -->
| Stack | Service | Container port | Host port | Canonical |
|---|---|---|---|---|
| airflow | airflow | 8080 | 11080 |  |
| airflow | airflow | 9180 | 11180 |  |
| airflow | postgresql | 5432 | 11432 |  |
| airflow | redis | 6379 | 11379 |  |
| postgresql | postgresql | 5432 | 5432 | yes |
| postgresql | postgresql | 9180 | 12180 |  |
| redis | redis | 6379 | 6379 | yes |
| redis | redis | 9180 | 13180 |  |
| kafka | kafka | 9092 | 9092 | yes |
| kafka | kafka | 9093 | 9093 | yes |
| kafka | kafka | 9180 | 14180 |  |
| spark | spark | 7077 | 7077 | yes |
| spark | spark | 8080 | 8080 | yes |
| spark | spark | 8081 | 8081 | yes |
| spark | spark | 4040 | 4040 | yes |
| spark | spark | 6066 | 6066 | yes |
| spark | spark | 9180 | 15180 |  |
| jupyterhub | jupyterhub | 8000 | 16000 |  |
| jupyterhub | jupyterhub | 8081 | 16081 |  |
| jupyterhub | jupyterhub | 9180 | 16180 |  |
| superset | superset | 8088 | 8088 | yes |
| superset | superset | 5555 | 5555 | yes |
| superset | superset | 9180 | 17180 |  |
| superset | postgresql | 5432 | 17432 |  |
| superset | redis | 6379 | 17379 |  |
| odoo | odoo | 8069 | 8069 | yes |
| odoo | odoo | 8072 | 8072 | yes |
| odoo | odoo | 9180 | 18180 |  |
| odoo | postgresql | 5432 | 18432 |  |
| supabase | api-gw | 8000 | 8000 | yes |
| supabase | api-gw | 9180 | 19180 |  |
| supabase | db | 5432 | 19432 |  |
| supabase | supavisor | 6543 | 6543 | yes |
| nextjs | nextjs | 3000 | 3000 | yes |
| nextjs | postgresql | 5432 | 20432 |  |
| nginx | nginx | 8080 | 21080 |  |
| nginx | nginx | 9180 | 21180 |  |
| seaweedfs | seaweedfs | 8333 | 8333 | yes |
| seaweedfs | seaweedfs | 9333 | 9333 | yes |
| seaweedfs | seaweedfs | 8888 | 8888 | yes |
| seaweedfs | seaweedfs | 8080 | 22080 |  |
| seaweedfs | seaweedfs | 9324 | 9324 | yes |
| supabase-postgres | supabase-postgres | 5432 | 23432 |  |
| supabase-postgres | supabase-postgres | 9180 | 23180 |  |
| supabase-postgres-meta | supabase-postgres-meta | 8080 | 24080 |  |
| supabase-postgres-meta | supabase-postgres-meta | 9180 | 24180 |  |
| supabase-postgrest | supabase-postgrest | 3000 | 25000 |  |
| supabase-postgrest | supabase-postgrest | 9180 | 25180 |  |
| supabase-studio | supabase-studio | 3000 | 26000 |  |
| supabase-studio | supabase-studio | 9180 | 26180 |  |
| supabase-supavisor | supabase-supavisor | 4000 | 27000 |  |
| supabase-supavisor | supabase-supavisor | 5432 | 27432 |  |
| supabase-supavisor | supabase-supavisor | 6543 | 27543 |  |
| supabase-supavisor | supabase-supavisor | 9180 | 27180 |  |
| supabase-auth | supabase-auth | 9999 | 9999 | yes |
| supabase-auth | supabase-auth | 9180 | 28180 |  |
| supabase-imgproxy | supabase-imgproxy | 5001 | 5001 | yes |
| supabase-imgproxy | supabase-imgproxy | 9180 | 28181 |  |
| supabase-realtime | supabase-realtime | 4000 | 4000 | yes |
| supabase-realtime | supabase-realtime | 9180 | 28182 |  |
| supabase-storage | supabase-storage | 5000 | 5000 | yes |
| supabase-storage | supabase-storage | 9180 | 28183 |  |
<!-- port-table:end -->
