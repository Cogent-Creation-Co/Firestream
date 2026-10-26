# Self-hosted Supabase

Firestream builds the self-hosted Supabase stack with Nix. Every image comes
from this flake, and no upstream Docker image runs. The stack follows the
upstream self-hosted release `self-hosted/v0.8.3`
(`firestream.supabase.release`).

## The services

| Service | Image (flake package) | Port | Gateway path |
|---|---|---|---|
| Postgres 17 with the Supabase extensions and migrations | `supabase-postgres` | 5432 | none |
| Auth (GoTrue) | `supabase-auth` | 9999 | `/auth/v1/` |
| PostgREST | `supabase-postgrest` | 3000 | `/rest/v1/`, `/graphql/v1` |
| Realtime | `supabase-realtime` | 4000 | `/realtime/v1/` |
| Supavisor (connection pooler) | `supabase-supavisor` | 5432, 6543 | none |
| postgres-meta | `supabase-postgres-meta` | 8080 | `/pg/` (service key only) |
| Storage | `supabase-storage` | 5000 | `/storage/v1/` |
| imgproxy | `supabase-imgproxy` | 5001 | none (Storage calls it) |
| Studio | `supabase-studio` | 3000 | `/` (dashboard login) |
| Envoy API gateway | `supabase-envoy` | 8000 | the entry point |

Storage keeps its objects in SeaweedFS (`seaweedfs`), in the bucket
`supabase-storage`, with S3 authentication on.

Out of scope: Edge Functions, Logflare (analytics) and Vector. The gateway
still has the `/functions/v1/` route, but no service answers it.

## Run the stack with docker compose

Start the stack:

```bash
nix run .#supabase-up
```

The command builds and loads the eleven images, then runs
`docker compose up -d`. Each service starts after the services it needs are
healthy. The first start runs every Supabase migration in Postgres.

Stop the stack:

```bash
nix run .#supabase-down          # keep the volumes
nix run .#supabase-down -- -v    # also delete the data
```

The compose file is `packages.supabase-compose`. The stack is declared in
`src/containers/firestream/supabase-envoy/compose.nix`.

### Ports on the host

| URL | What |
|---|---|
| `http://localhost:8000/` | Studio (log in with the dashboard user) |
| `http://localhost:8000/rest/v1/` | PostgREST |
| `http://localhost:8000/auth/v1/` | Auth |
| `http://localhost:8000/storage/v1/` | Storage |
| `ws://localhost:8000/realtime/v1/websocket` | Realtime |
| `localhost:19432` | Postgres, direct |
| `localhost:6543` | Supavisor, transaction mode |
| `http://localhost:19180/readyz` | healthd of the gateway |

The registry `bin/nix/firestream/ports.nix` sets these host ports. The
gateway and Supavisor get their canonical ports, 8000 and 6543. Postgres and
healthd get ports in the supabase block, 19000. See [ports.md](ports.md) for
all stacks.

### Development secrets

The compose stack and the chart use the published development key set in
`bin/nix/firestream/supabase/dev-secrets.json`
(`firestream.lib.<system>.supabase.devSecrets`).

- Studio login: user `supabase`, password `DASHBOARD_PASSWORD`.
- Client keys: `ANON_KEY` and `SERVICE_ROLE_KEY`. Send them in the `apikey`
  header.
- Postgres: user `postgres`, password `POSTGRES_PASSWORD`.
- Supavisor: user `postgres.firestream`, password `POSTGRES_PASSWORD`. The
  tenant id is `firestream`.

WARNING: anybody can read these keys in this repository. With them, a person
can sign a `service_role` token and read Vault secrets. Never use them on a
host that other people can reach.

## Deploy the stack to Kubernetes

The chart is `packages.supabase-chart`. The stack `firestreamStacks.supabase`
deploys SeaweedFS first, then Supabase:

```bash
firestream helm deploy-stack supabase
```

Each chart deploys into its own namespace (`seaweedfs`, `supabase`). Reach the
gateway with a port-forward:

```bash
kubectl -n supabase port-forward svc/supabase-supabase-envoy 8000:8000
```

Then open `http://localhost:8000/`.

### Supply production secrets

Do not deploy the development keys. Make a new key set and store it in a
Secret:

```bash
nix run .#supabase-keygen -- --output k8s-secret --name supabase-secrets \
  | kubectl -n supabase apply -f -
```

Then tell the chart to read that Secret. Set `secret.supabase.secretRef` to
`supabase-secrets`. With a Nix consumer, use the override API:

```nix
firestream.lib.${system}.charts.supabase.eval ({ ... }: {
  config.supabase.secret.supabase.secretRef = "supabase-secrets";
})
```

With plain helm, add `--set secret.supabase.secretRef=supabase-secrets`. The
chart then renders no key Secret. `helm install`/`helm template` prints a
warning in the notes when the chart still uses the development keys, and
prints no warning once `secretRef` is set.

Keep the key set in a safe place. The Postgres password and the pgsodium root
key on the database volume protect the data. If you lose the volume, you lose
the pgsodium root key, and Vault secrets cannot be read again.

Other secrets:

- `secret.s3`: the S3 credentials of the Storage backend. The defaults are the
  SeaweedFS chart credentials. Set `secretRef` to use an existing Secret.
- `secret.smtp`: the SMTP user and password for Auth. Without SMTP, set
  `GOTRUE_MAILER_AUTOCONFIRM=true` (the default of the chart).

### Public URLs

Set the public URL of the gateway in these values:

- `environment.auth`: `API_EXTERNAL_URL` (`<public URL>/auth/v1`) and
  `GOTRUE_SITE_URL` (your application).
- `environment.storage`: `STORAGE_PUBLIC_URL`.
- `environment.studio` and `environment.envoy`: `SUPABASE_PUBLIC_URL`.

To expose the gateway, set `ingress.enabled=true` and the `ingress` hosts.

## Add your own SQL

Put SQL files in the chart value `migration`. Postgres runs them once, on the
first boot, after the Supabase migrations:

```yaml
migration:
  10-app.sql: |
    create table public.todo (id bigint primary key, title text);
```

## Test the stack

Two end-to-end tests deploy the stack and drive every service through the
gateway:

```bash
make test-e2e-supabase       # docker compose
make test-e2e-k8s-supabase   # a new k3d cluster, then the two charts
```

### Setup

1. The make target runs `make e2e-sweep`. The sweep removes the objects that
   earlier test runs left.
2. The test takes the host lock
   `${XDG_RUNTIME_DIR:-/tmp}/firestream-e2e.lock`. A second test run or
   sweep on the same host waits for it.
3. The test gives each object a run name, `firestream-e2e-<target>-<id>`.
   The `<id>` is new for each test process. Set `FIRESTREAM_E2E_RUN_ID` to
   select the id.
4. The compose test checks the host ports first. A dev stack or a process
   that holds a port makes the test fail at once. The message gives the port
   and the container or process that holds it. Stop the holder before the
   test.

The test does not touch a dev stack. A dev stack has the project name
`firestream-supabase`, and the sweep finds only `firestream-e2e-*` names.

The k3d test does not change `~/.kube/config` or `$KUBECONFIG`. It writes
its kubeconfig to `$TMPDIR/firestream-e2e-<target>-<id>.kubeconfig`.

### Teardown

- Compose: `docker compose down -v --remove-orphans` for the run project.
  This also removes the volumes and the egress helper container.
- k3d: the test removes the registry, the cluster, the `k3d-<name>`
  network, the `k3d-<name>-images` volume, the kubeconfig file and the
  kubeconfig context, in that sequence.

The teardown also runs when a test fails, and when the test process gets
SIGINT, SIGTERM or SIGHUP. On a signal, the test first stops its
subprocesses (for example `k3d cluster create` or `docker compose up`).
Then it runs the teardown and exits with `128 + <signal>` (130 for SIGINT,
143 for SIGTERM).

### Leak check

After the teardown, the test looks for every object that has the run name.
If an object remains, the test fails. The failure lists each object and the
command that removes them:

```text
make e2e-sweep RUN=<id>
```

### Keep the objects for a check

```bash
FIRESTREAM_E2E_KEEP=1 make test-e2e-supabase
FIRESTREAM_E2E_K8S_KEEP=1 make test-e2e-k8s-supabase
```

With KEEP set, the test does not tear down and does not do the leak check.
It prints the run id and the sweep command. Remove the objects when you are
done:

```bash
make e2e-sweep RUN=<id>
```

The next e2e test target also removes them, because it sweeps first.

### Sweep by hand

```bash
make e2e-sweep DRY_RUN=1    # show the objects; remove nothing
make e2e-sweep              # remove every harness object on this host
make e2e-sweep RUN=<id>     # remove only the objects of one run
```

The sweep removes only these objects:

- k3d clusters with the name `firestream-e2e-*`;
- containers, networks and volumes with the name `k3d-firestream-e2e-*`
  and the label `app=k3d`;
- compose projects `firestream-e2e-*`, with their containers, volumes and
  networks;
- the `firestream-e2e-*.kubeconfig` files and the
  `k3d-firestream-e2e-*` kubeconfig contexts.

Outside the devshell, `make e2e-sweep` starts `nix develop`, because the
sweep needs `k3d`.

`make test-e2e-cleanup` runs three cleanup tests. Two tests send SIGTERM to
a test run during its setup, one for k3d and one for compose. They make sure
that nothing remains. The third test makes labelled objects and makes sure
that the leak check finds them.
