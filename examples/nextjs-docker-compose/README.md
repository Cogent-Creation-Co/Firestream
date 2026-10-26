# Next.js on Docker Compose — a Firestream deployment (custom app)

The lightest Firestream Next.js deployment: **no Kubernetes**. Builds a custom
Next.js image with **your app baked in** and runs it with the docker-compose
stack Firestream generates for nextjs (firestream-nextjs + firestream-postgresql).
Sibling of [`../nextjs-k3s`](../nextjs-k3s) and [`../nextjs-gke`](../nextjs-gke).
See [`../README.md`](../README.md) for the one-flake pattern.

## Layout

```
nextjs-docker-compose/
├── flake.nix                       # packages.{nextjs-image,postgresql-image,nextjs-compose}
├── config.nix                      # (build-time knobs; app source is ./app)
├── app/                            # ← YOUR Next.js app (standalone output), baked into the image
│   ├── app/page.jsx                #    homepage runs a live SELECT against postgres
│   ├── app/api/health/route.js     #    /api/health probe endpoint
│   ├── next.config.js              #    output: "standalone"
│   └── package.json / package-lock.json
├── firestream/
│   └── nextjs-image-overrides.nix  # config.nextjs.vendoredApp = { src = ./app; npmDepsHash = ...; }
├── scripts/
│   └── up.sh                       # build + load images → docker compose up
└── Makefile                        # up / down / logs / ps / clean
```

## How it works

Firestream generates a docker-compose stack for nextjs
(`firestream.lib.<sys>.images.nextjs.compose`). This example:

1. Builds a **custom** `firestream-nextjs:1.0.0` image from [`app/`](./app) (the
   Next.js sources are compiled to standalone output by `mkNodePackage`).
2. `docker load`s it (plus `firestream-postgresql:17`).
3. `docker compose up` against the generated file, which references those images
   **by name** — so your custom build is what runs.

## Quickstart

### 0. Prerequisites
`nix` (flakes) and `docker` with the Compose plugin. `nix develop` (or
`direnv allow`) for the rest.

### 1. Start it
```bash
make up         # builds + loads images, docker compose up -d
make ps         # wait for services to be healthy
```

### 2. Reach the app
Open <http://127.0.0.1:3000>. The homepage opens a `pg` pool and renders the
live `select version(), now()` from firestream-postgresql.
Health: `curl http://127.0.0.1:3000/api/health` → `{"status":"ok"}`.

### 3. Stop / reset
```bash
make down       # stop, keep data
make clean      # stop AND drop the postgresql_data volume
```

## Ports & credentials

From Firestream's port registry (`bin/nix/firestream/ports.nix` upstream):

| Service | Container port | Host port |
|---------|----------------|-----------|
| Next.js (web) | 3000 | **3000** |
| PostgreSQL | 5432 | 20432 |

- **Database:** user `firestream`, password `firestream`, db `firestream_nextjs`
  (the compose `sharedEnv`/postgres service env). The app reads
  `PGHOST/PGPORT/PGUSER/PGPASSWORD/PGDATABASE`.
- **Persistence:** named volume `postgresql_data`; `make clean` drops it.

## Notes

- **Make it your own:** edit anything under [`app/`](./app). If you change
  dependencies (`package.json`/`package-lock.json`), update `npmDepsHash` in
  `firestream/nextjs-image-overrides.nix` — build once, copy the reported
  `got: sha256-...`.
- **In-repo testing before merge** (the flake pins github main):
  ```bash
  NIX_OVERRIDE="--override-input firestream path:../.." make up
  ```
