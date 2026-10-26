# Odoo on Docker Compose — a Firestream deployment

The lightest Firestream deployment: **no Kubernetes at all**. It builds a custom
Odoo image and runs it with the docker-compose stack Firestream generates for
the Odoo app (Odoo + PostgreSQL, wired together with healthchecks and a data
volume). Sibling of [`../odoo-k3s`](../odoo-k3s) and [`../odoo-gke`](../odoo-gke)
— same app, plainest runtime. See [`../README.md`](../README.md) for the
one-flake pattern.

## Layout

```
odoo-docker-compose/
├── flake.nix                  # imports firestream; packages.{odoo-image,postgresql-image,odoo-compose}
├── config.nix                 # ← YOU EDIT THIS: vendored addons (build-time image customization)
├── firestream/
│   └── odoo-image-overrides.nix  # vendored addons → custom firestream-odoo:18.0 image
├── scripts/
│   └── up.sh                  # build + load custom image → docker compose up
└── Makefile                   # up / down / logs / ps / clean
```

## How it works

Firestream already generates a docker-compose stack for every supported app
(`firestream.lib.<sys>.images.odoo.compose`). This example:

1. Builds a **custom** `firestream-odoo:18.0` image from
   [`firestream/odoo-image-overrides.nix`](./firestream/odoo-image-overrides.nix)
   (vendored addons in [`config.nix`](./config.nix)).
2. `docker load`s it (plus `firestream-postgresql:17`).
3. Runs `docker compose up` against the generated `docker-compose.yml`, which
   references those images **by name** — so your custom build is what runs.

We deliberately use `scripts/up.sh` rather than `nix run
firestream#odoo-up`: the latter rebuilds the *stock* image and would clobber the
custom load.

## Quickstart

### 0. Prerequisites
`nix` (flakes enabled) and `docker` with the Compose plugin. Everything else is
in the dev shell — `nix develop` (or `direnv allow`).

### 1. (optional) Edit `config.nix`
Add or remove `vendoredAddons`. The default vendors one OCA module for the demo.

### 2. Start it
```bash
make up        # builds + loads the custom image, then docker compose up -d
make ps        # wait for both services to be Up (postgresql: healthy)
```

### 3. Reach Odoo
Open <http://127.0.0.1:8069>.

### 4. Stop / reset
```bash
make down      # stop, keep data
make clean     # stop AND drop the postgresql_data volume (full reset)
```

## Ports & credentials

These come from Firestream's port registry, `bin/nix/firestream/ports.nix`
upstream. To change them, edit the odoo rows there.

| Service | Container port | Host port |
|---------|----------------|-----------|
| Odoo HTTP | 8069 | **8069** |
| Odoo gevent (longpolling) | 8072 | 8072 |
| Odoo healthd | 9180 | 18180 |
| PostgreSQL | 5432 | 18432 |

- **Odoo admin login:** `admin` / `admin` (the container's baked defaults — the
  compose file doesn't override them). To change them, bake
  `config.odoo.env.ODOO_EMAIL` / `ODOO_PASSWORD` into the custom image via
  `firestream/odoo-image-overrides.nix`.
- **Database:** user `odoo` / password `odoo`, database `firestream_odoo`,
  reachable on host port 18432.
- **Persistence:** the named volume `postgresql_data`. `make clean` drops it.

## Vendoring extra addons

Same value prop as the other examples — *inject preferences → custom image, no
fork.* Add repos to `vendoredAddons` in [`config.nix`](./config.nix); they're
baked into `/opt/odoo/vendor-addons` and wired into `addons_path`. After
`make up`, install a vendored module like any other (Apps → Update Apps List).
See [`../odoo-gke/README.md`](../odoo-gke/README.md) for the `hash` workflow and
non-GitHub sources.

## Notes

- **First build is slow.** The Odoo image is ~2.25 GB and the github flake input
  is fetched once; cached afterward.
- **In-repo testing (before this lands on `main`).** The flake pins the public
  github input, which only has the consumer API once merged. To test against
  your local checkout meanwhile:
  ```bash
  NIX_OVERRIDE="--override-input firestream path:../.." make up
  ```
