# Airflow on Docker Compose — a Firestream deployment (custom baked DAG)

The lightest Firestream Airflow deployment: **no Kubernetes**. Builds a custom
Airflow image with a **DAG baked in** and runs it with the multi-service
docker-compose stack Firestream generates for Airflow (postgresql + redis +
scheduler/triggerer/dag-processor/worker + api-server, CeleryExecutor). Sibling
of [`../airflow-k3s`](../airflow-k3s) and [`../airflow-gke`](../airflow-gke). See
[`../README.md`](../README.md) for the one-flake pattern.

## Layout

```
airflow-docker-compose/
├── flake.nix                       # packages.{airflow-image,postgresql-image,redis-image,airflow-compose}
├── config.nix                      # (build-time knobs; DAG source is ../dags)
├── dags/
│   └── hello_firestream.py         # ← the custom DAG, baked into the image
├── firestream/
│   └── airflow-image-overrides.nix # config.airflow.vendoredDags = [ { src = ../dags; } ]
├── scripts/
│   └── up.sh                       # build + load images → docker compose up
└── Makefile                        # up / down / logs / ps / clean
```

## How it works

Firestream generates a docker-compose stack for Airflow
(`firestream.lib.<sys>.images.airflow.compose`). This example:

1. Builds a **custom** `firestream-airflow:3.0.3` image with the DAGs from
   [`dags/`](./dags) baked into `/opt/firestream/airflow/dags` (via
   `config.airflow.vendoredDags`).
2. `docker load`s it (plus `firestream-postgresql:17` and `firestream-redis:8`).
3. `docker compose up` against the generated file, which references those images
   **by name** — so your custom build (with the DAG) is what runs.

## Quickstart

### 0. Prerequisites
`nix` (flakes) and `docker` with the Compose plugin. `nix develop` (or
`direnv allow`) for the rest.

### 1. Start it
```bash
make up         # builds + loads images, docker compose up -d
make ps         # wait for services to be healthy
```

### 2. Reach Airflow + see the DAG
Open <http://127.0.0.1:11080>, log in `admin` / `admin`, and confirm
**`hello_firestream`** is listed. Unpause + trigger it; the task logs
"Hello from a Firestream-baked DAG!".

### 3. Stop / reset
```bash
make down       # stop, keep data
make clean      # stop AND drop the postgresql_data + redis_data volumes
```

## Ports & credentials

From Firestream's port registry (`bin/nix/firestream/ports.nix` upstream):

| Service | Container port | Host port |
|---------|----------------|-----------|
| Airflow api-server (web UI) | 8080 | **11080** |
| Airflow healthd | 9180 | 11180 |
| PostgreSQL | 5432 | 11432 |
| Redis | 6379 | 11379 |

- **Web login:** `admin` / `admin` (baked container defaults; the compose file
  doesn't override them). Change by baking `config.airflow.env.AIRFLOW_PASSWORD`
  into the custom image via `firestream/airflow-image-overrides.nix`.
- **Persistence:** named volumes `postgresql_data` + `redis_data`; `make clean`
  drops them.

## Notes

- **First build is slow** (large Airflow image + one-time github fetch); cached after.
- **In-repo testing before merge** (the flake pins github main):
  ```bash
  NIX_OVERRIDE="--override-input firestream path:../.." make up
  ```
