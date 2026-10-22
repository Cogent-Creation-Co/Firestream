# Bitnami package for Odoo

## What is Odoo?

> Odoo is an open source ERP and CRM platform, formerly known as OpenERP, that can connect a wide variety of business operations such as sales, supply chain, finance, and project management.

[Overview of Odoo](https://www.odoo.com/)
Trademarks: This software listing is packaged by Bitnami. The respective trademarks mentioned in the offering are owned by the respective companies, and use of them does not imply any affiliation or endorsement.

## TL;DR

```console
docker run --name odoo bitnami/odoo:latest
```

**Warning**: This quick setup is only intended for development environments. You are encouraged to change the insecure default credentials and check out the available configuration options in the [Environment Variables](#environment-variables) section for a more secure d
eployment.

## ⚠️ Important Notice: Upcoming changes to the Bitnami Catalog

Beginning August 28th, 2025, Bitnami will evolve its public catalog to offer a curated set of hardened, security-focused images under the new [Bitnami Secure Images initiative](https://news.broadcom.com/app-dev/broadcom-introduces-bitnami-secure-images-for-production-ready-containerized-applications). As part of this transition:

- Granting community users access for the first time to security-optimized versions of popular container images.
- Bitnami will begin deprecating support for non-hardened, Debian-based software images in its free tier and will gradually remove non-latest tags from the public catalog. As a result, community users will have access to a reduced number of hardened images. These images are published only under the “latest” tag and are intended for development purposes
- Starting August 28th, over two weeks, all existing container images, including older or versioned tags (e.g., 2.50.0, 10.6), will be migrated from the public catalog (docker.io/bitnami) to the “Bitnami Legacy” repository (docker.io/bitnamilegacy), where they will no longer receive updates.
- For production workloads and long-term support, users are encouraged to adopt Bitnami Secure Images, which include hardened containers, smaller attack surfaces, CVE transparency (via VEX/KEV), SBOMs, and enterprise support.

These changes aim to improve the security posture of all Bitnami users by promoting best practices for software supply chain integrity and up-to-date deployments. For more details, visit the [Bitnami Secure Images announcement](https://github.com/bitnami/containers/issues/83267).

## Why use Bitnami Secure Images?

- Bitnami Secure Images and Helm charts are built to make open source more secure and enterprise ready.
- Triage security vulnerabilities faster, with transparency into CVE risks using industry standard Vulnerability Exploitability Exchange (VEX), KEV, and EPSS scores.
- Our hardened images use a minimal OS (Photon Linux), which reduces the attack surface while maintaining extensibility through the use of an industry standard package format.
- Stay more secure and compliant with continuously built images updated within hours of upstream patches.
- Bitnami containers, virtual machines and cloud images use the same components and configuration approach - making it easy to switch between formats based on your project needs.
- Hardened images come with attestation signatures (Notation), SBOMs, virus scan reports and other metadata produced in an SLSA-3 compliant software factory.

Only a subset of BSI applications are available for free. Looking to access the entire catalog of applications as well as enterprise support? Try the [commercial edition of Bitnami Secure Images today](https://www.arrow.com/globalecs/uk/products/bitnami-secure-images/).

## How to deploy Odoo in Kubernetes?

Deploying Bitnami applications as Helm Charts is the easiest way to get started with our applications on Kubernetes. Read more about the installation in the
[Bitnami Odoo Chart GitHub repository](https://github.com/bitnami/charts/tree/master/bitnami/odoo).

## Supported tags and respective `Dockerfile` links

Learn more about the Bitnami tagging policy and the difference between rolling tags and immutable tags [in our documentation page](https://techdocs.broadcom.com/us/en/vmware-tanzu/application-catalog/tanzu-application-catalog/services/tac-doc/apps-tutorials-understand-rolling-tags-containers-index.html).

You can see the equivalence between the different tags by taking a look at the `tags-info.yaml` file present in the branch folder, i.e `bitnami/ASSET/BRANCH/DISTRO/tags-info.yaml`.

Subscribe to project updates by watching the [bitnami/containers GitHub repo](https://github.com/bitnami/containers).

## Get this image

The recommended way to get the Bitnami Odoo Docker Image is to pull the prebuilt image from the [Docker Hub Registry](https://hub.docker.com/r/bitnami/odoo).

```console
docker pull bitnami/odoo:latest
```

To use a specific version, you can pull a versioned tag. You can view the [list of available versions](https://hub.docker.com/r/bitnami/odoo/tags/) in the Docker Hub Registry.

```console
docker pull bitnami/odoo:[TAG]
```

If you wish, you can also build the image yourself by cloning the repository, changing to the directory containing the Dockerfile and executing the `docker build` command. Remember to replace the `APP`, `VERSION` and `OPERATING-SYSTEM` path placeholders in the example command below with the correct values.

```console
git clone https://github.com/bitnami/containers.git
cd bitnami/APP/VERSION/OPERATING-SYSTEM
docker build -t bitnami/APP:latest .
```

## How to use this image

Odoo requires access to a PostgreSQL database to store information. We'll use the [Bitnami Docker Image for PostgreSQL](https://github.com/bitnami/containers/tree/main/bitnami/postgresql) for the database requirements.

### Using the Docker Command Line

#### Step 1: Create a network

```console
docker network create odoo-network
```

#### Step 2: Create a volume for PostgreSQL persistence and create a PostgreSQL container

```console
$ docker volume create --name postgresql_data
docker run -d --name postgresql \
  --env ALLOW_EMPTY_PASSWORD=yes \
  --env POSTGRESQL_PASSWORD=bitnami \
  --network odoo-network \
  --volume postgresql_data:/bitnami/postgresql \
  bitnami/postgresql:latest
```

#### Step 3: Create volumes for Odoo persistence and launch the container

```console
$ docker volume create --name odoo_data
docker run -d --name odoo \
  -p 80:8069 \
  --env ALLOW_EMPTY_PASSWORD=yes \
  --env ODOO_DATABASE_ADMIN_PASSWORD=bitnami \
  --network odoo-network \
  --volume odoo_data:/bitnami/odoo \
  bitnami/odoo:latest
```

Access your application at `http://your-ip/`

### Run the application using Docker Compose

```console
curl -sSL https://raw.githubusercontent.com/bitnami/containers/main/bitnami/odoo/docker-compose.yml > docker-compose.yml
docker-compose up -d
```

Please be aware this file has not undergone internal testing. Consequently, we advise its use exclusively for development or testing purposes. For production-ready deployments, we highly recommend utilizing its associated [Bitnami Helm chart](https://github.com/bitnami/charts/tree/main/bitnami/odoo).

If you detect any issue in the `docker-compose.yaml` file, feel free to report it or contribute with a fix by following our [Contributing Guidelines](https://github.com/bitnami/containers/blob/main/CONTRIBUTING.md).

## Persisting your application

If you remove the container all your data will be lost, and the next time you run the image the database will be reinitialized. To avoid this loss of data, you should mount a volume that will persist even after the container is removed.

For persistence you should mount a directory at the `/bitnami/odoo` path. If the mounted directory is empty, it will be initialized on the first run. Additionally you should [mount a volume for persistence of the PostgreSQL data](https://github.com/bitnami/containers/tree/main/bitnami/postgresql#persisting-your-database).

The above examples define the Docker volumes named `postgresql_data` and `odoo_data`. The Odoo application state will persist as long as volumes are not removed.

To avoid inadvertent removal of volumes, you can [mount host directories as data volumes](https://docs.docker.com/engine/tutorials/dockervolumes/). Alternatively you can make use of volume plugins to host the volume data.

### Mount host directories as data volumes with Docker Compose

This requires a minor change to the [`docker-compose.yml`](https://github.com/bitnami/containers/blob/main/bitnami/odoo/docker-compose.yml) file present in this repository:

```diff
   postgresql:
     ...
     volumes:
-      - postgresql_data:/bitnami/postgresql
+      - /path/to/postgresql-persistence:/bitnami/postgresql
   ...
   odoo:
     ...
     volumes:
-      - odoo_data:/bitnami/odoo
+      - /path/to/odoo-persistence:/bitnami/odoo
   ...
-volumes:
-  postgresql_data:
-    driver: local
-  odoo_data:
-    driver: local
```

> NOTE: As this is a non-root container, the mounted files and directories must have the proper permissions for the UID `1001`.

### Mount host directories as data volumes using the Docker command line

#### Step 1: Create a network (if it does not exist)

```console
docker network create odoo-network
```

#### Step 2. Create a PostgreSQL container with host volume

```console
docker run -d --name postgresql \
  --env ALLOW_EMPTY_PASSWORD=yes \
  --env POSTGRESQL_PASSWORD=bitnami \
  --network odoo-network \
  --volume /path/to/postgresql-persistence:/bitnami/postgresql \
  bitnami/postgresql:latest
```

#### Step 3. Create the Odoo container with host volumes

```console
docker run -d --name odoo \
  -p 80:8069 \
  --env ALLOW_EMPTY_PASSWORD=yes \
  --env ODOO_DATABASE_ADMIN_PASSWORD=bitnami \
  --network odoo-network \
  --volume /path/to/odoo-persistence:/bitnami/odoo \
  bitnami/odoo:latest
```

## Configuration

### Environment variables

#### Customizable environment variables

| Name                           | Description                                                                                                                | Default Value                                          |
|--------------------------------|----------------------------------------------------------------------------------------------------------------------------|--------------------------------------------------------|
| `ODOO_DATA_TO_PERSIST`         | Files to persist relative to the Odoo installation directory. To provide multiple values, separate them with a whitespace. | `${ODOO_ADDONS_DIR} ${ODOO_CONF_DIR} ${ODOO_DATA_DIR}` |
| `ODOO_PORT_NUMBER`             | Port number in which Odoo will run.                                                                                        | `8069`                                                 |
| `ODOO_LONGPOLLING_PORT_NUMBER` | Port number in which the Odoo Longpolling service will run.                                                                | `8072`                                                 |
| `ODOO_SKIP_BOOTSTRAP`          | Whether to perform initial bootstrapping for the application.                                                              | `no`                                                   |
| `ODOO_SKIP_MODULES_UPDATE`     | Whether to perform initial update of the plugins installed.                                                                | `no`                                                   |
| `ODOO_LOAD_DEMO_DATA`          | Whether to load demo data. First database init only: Odoo records the decision per-database, so flipping this later has no effect (and modules installed later into a no-demo DB skip their demo data too). To reseed, drop the database and data volumes and boot fresh. | `no`                                                   |
| `ODOO_LIST_DB`                 | Whether the database manager and selector are available (`list_db`). Keep `no` in production.                             | `no`                                                   |
| `ODOO_WORKERS`                 | HTTP worker processes (`workers`). `0` = threaded mode, gevent port not bound.                                             | `0`                                                    |
| `ODOO_LIMIT_TIME_CPU`          | Max CPU seconds per request (`limit_time_cpu`).                                                                            | `90`                                                   |
| `ODOO_LIMIT_TIME_REAL`         | Max wall-clock seconds per request (`limit_time_real`).                                                                    | `150`                                                  |
| `ODOO_LIMIT_TIME_REAL_CRON`    | Max wall-clock seconds per cron job (`limit_time_real_cron`); `-1` = same as `limit_time_real`.                            | `-1`                                                   |
| `ODOO_LIMIT_MEMORY_SOFT`       | Soft per-worker memory limit in bytes (`limit_memory_soft`).                                                               | `2147483648`                                           |
| `ODOO_LIMIT_MEMORY_HARD`       | Hard per-worker memory limit in bytes (`limit_memory_hard`).                                                               | `2684354560`                                           |
| `ODOO_LIMIT_REQUEST`           | Requests a worker serves before recycling (`limit_request`).                                                               | `65536`                                                |
| `ODOO_MAX_CRON_THREADS`        | Threads dedicated to cron jobs (`max_cron_threads`).                                                                       | `1`                                                    |
| `ODOO_EMAIL`                   | Odoo user e-mail address.                                                                                                  | `user@example.com`                                     |
| `ODOO_PASSWORD`                | Odoo user password.                                                                                                        | `bitnami`                                              |
| `ODOO_SMTP_HOST`               | Odoo SMTP server host.                                                                                                     | `nil`                                                  |
| `ODOO_SMTP_PORT_NUMBER`        | Odoo SMTP server port number.                                                                                              | `nil`                                                  |
| `ODOO_SMTP_USER`               | Odoo SMTP server user.                                                                                                     | `nil`                                                  |
| `ODOO_SMTP_PASSWORD`           | Odoo SMTP server user password.                                                                                            | `nil`                                                  |
| `ODOO_SMTP_PROTOCOL`           | Odoo SMTP server protocol to use.                                                                                          | `nil`                                                  |
| `ODOO_DATABASE_HOST`           | Database server host.                                                                                                      | `$ODOO_DEFAULT_DATABASE_HOST`                          |
| `ODOO_DATABASE_PORT_NUMBER`    | Database server port.                                                                                                      | `5432`                                                 |
| `ODOO_DATABASE_NAME`           | Database name.                                                                                                             | `bitnami_odoo`                                         |
| `ODOO_DATABASE_USER`           | Database user name.                                                                                                        | `bn_odoo`                                              |
| `ODOO_DATABASE_PASSWORD`       | Database user password.                                                                                                    | `nil`                                                  |
| `ODOO_DATABASE_FILTER`         | Database filter                                                                                                            | `nil`                                                  |

#### Read-only environment variables

| Name                         | Description                                     | Value                                         |
|------------------------------|-------------------------------------------------|-----------------------------------------------|
| `ODOO_BASE_DIR`              | Odoo installation directory.                    | `${BITNAMI_ROOT_DIR}/odoo`                    |
| `ODOO_BIN_DIR`               | Odoo directory for binary executables.          | `${ODOO_BASE_DIR}/bin`                        |
| `ODOO_CONF_DIR`              | Odoo directory for configuration files.         | `${ODOO_BASE_DIR}/conf`                       |
| `ODOO_CONF_FILE`             | Configuration file for Odoo.                    | `${ODOO_CONF_DIR}/odoo.conf`                  |
| `ODOO_DATA_DIR`              | Odoo directory for data files.                  | `${ODOO_BASE_DIR}/data`                       |
| `ODOO_ADDONS_DIR`            | Odoo directory for extra addons.                | `${ODOO_ADDONS_DIR:-${ODOO_BASE_DIR}/addons}` |
| `ODOO_ADDONS_PATH`           | Baked `addons_path` (Firestream). Carries the literal `{{ODOO_ADDONS_DIR}}` token, substituted at boot. Rendered by `addons-layout.nix`; see "Baked addons" below. | `${ODOO_BASE_DIR}/addons,${ODOO_BASE_DIR}/odoo/addons,${ODOO_BASE_DIR}/vendor-addons,{{ODOO_ADDONS_DIR}}` |
| `ODOO_TMP_DIR`               | Odoo directory for temporary files.             | `${ODOO_BASE_DIR}/tmp`                        |
| `ODOO_PID_FILE`              | PID file for Odoo.                              | `${ODOO_TMP_DIR}/odoo.pid`                    |
| `ODOO_LOGS_DIR`              | Odoo directory for log files.                   | `${ODOO_BASE_DIR}/log`                        |
| `ODOO_LOG_FILE`              | Log file for Odoo.                              | `${ODOO_LOGS_DIR}/odoo-server.log`            |
| `ODOO_VOLUME_DIR`            | Odoo directory for mounted configuration files. | `${BITNAMI_VOLUME_DIR}/odoo`                  |
| `ODOO_DAEMON_USER`           | Odoo system user.                               | `odoo`                                        |
| `ODOO_DAEMON_GROUP`          | Odoo system group.                              | `odoo`                                        |
| `ODOO_DEFAULT_DATABASE_HOST` | Default database server host.                   | `postgresql`                                  |

When you start the Odoo image, you can adjust the configuration of the instance by passing one or more environment variables either on the docker-compose file or on the `docker run` command line. If you want to add a new environment variable:

- For docker-compose add the variable name and value under the application section in the [`docker-compose.yml`](https://github.com/bitnami/containers/blob/main/bitnami/odoo/docker-compose.yml) file present in this repository:

    ```yaml
    odoo:
      ...
      environment:
        - ODOO_PASSWORD=my_password
      ...
    ```

- For manual execution add a `--env` option with each variable and value:

    ```console
    $ docker run -d --name odoo -p 80:8069 \
      --env ODOO_PASSWORD=my_password \
      --network odoo-tier \
      --volume /path/to/odoo-persistence:/bitnami \
      bitnami/odoo:latest
    ```

### Baked addons: `vendoredAddons`, `localAddons` and `addonLayers`

> Firestream-specific. Options live in [`options.nix`](./options.nix); the
> `addons_path` ordering has exactly one definition, in
> [`addons-layout.nix`](./addons-layout.nix).

There are two ways to bake addons into the image, and they coexist.

#### Flat (legacy): `odoo.vendoredAddons` / `odoo.localAddons`

Every module from every spec is laid into a single directory,
`/opt/firestream/odoo/vendor-addons/<module>`, by
[`vendor-addons.nix`](./vendor-addons.nix). Any duplicate module name is a fatal
build error. `localAddons` is sugar over `vendoredAddons` and additionally
auto-installs what it finds (via `odoo.installModules` →
`ODOO_INSTALL_MODULES`). Nothing about this path has changed.

#### Ordered: `odoo.addonLayers`

Use this when you need tiers — a shared base, an org tier, a per-deployment
tier — and a higher tier must be allowed to *override* a module from a lower
one. The list is ordered **base → specific; later entries win**:

```nix
config.odoo.addonLayers = [
  # lowest precedence
  { name = "oca-web"; owner = "OCA"; repo = "web"; rev = "..."; hash = "...";
    modules = [ "web_responsive" ]; }
  { name = "org"; src = ./org-addons; }
  # highest precedence — deliberately replaces org's copy of `sale_extras`
  { name = "deployment"; src = ./deployment-addons; shadows = [ "sale_extras" ]; }
];
```

On-disk layout, `<NN>` being the zero-padded declaration index:

```text
/opt/firestream/odoo/addons.d/00-oca-web/web_responsive/
/opt/firestream/odoo/addons.d/01-org/<module>/
/opt/firestream/odoo/addons.d/02-deployment/<module>/
/opt/firestream/odoo/addons.d/layers.json      # resolved order + modules, for debugging
```

Per-layer fields, in addition to every `vendoredAddons` field
(`name`/`owner`/`repo`/`rev`/`hash`/`src`/`sourceRoot`/`modules`):

| field | type | default | meaning |
|---|---|---|---|
| `shadows` | `listOf str` | `[ ]` | Modules this layer may override in a lower layer. |
| `autoInstall` | `nullOr bool` | `null` | Whether the layer's modules join `installModules`. `null` = auto. |

A module defined by two layers is a **build error** naming both layers, unless
the higher-precedence one lists it in `shadows`. The legacy
`/opt/firestream/odoo/vendor-addons` output participates in that check as an
implicit lowest-precedence pseudo-layer named `legacy-vendor-addons`.

`autoInstall = null` resolves to auto-install only when the module names are
knowable during Nix evaluation — that is, when `modules` is set explicitly or
`src` is a plain local path. For a source fetched at build time they are not
knowable, so the default is silently *not* to auto-install (the right default
for an OCA pin). `autoInstall = true` on such a layer is an evaluation error
telling you to set `modules`.

#### `addons_path` ordering and `odoo.addonsDirPrecedence`

```text
[{{ODOO_ADDONS_DIR}}]                     if addonsDirPrecedence == "first"
/opt/firestream/odoo/addons
/opt/firestream/odoo/odoo/addons          Odoo core
/opt/firestream/odoo/addons.d/<NN>-<name> layers, HIGHEST precedence first
/opt/firestream/odoo/vendor-addons        legacy output, lowest external precedence
[{{ODOO_ADDONS_DIR}}]                     if addonsDirPrecedence == "last"  (default)
```

Layers sit **after** Odoo core, so a layer can never silently shadow a core
Odoo addon — core is not a layer, so the `shadows` diagnostic could not see
such a collision. Shadowing core is out of scope.

`odoo.addonsDirPrecedence = "first"` moves the runtime-overridable
`ODOO_ADDONS_DIR` to the front, so a bind mount shadows every baked layer —
which is how one image can serve both a normal deployment and a live-edit
development loop.

With `addonLayers = [ ]` and the default precedence the rendered string is
byte-identical to what this image produced before `addonLayers` existed.

#### ⚠ Changing addons on an existing deployment: `ODOO_FORCE_OVERWRITE_CONF`

`/opt/firestream/odoo/conf` is declared **persistent**, and `odoo.conf` is only
generated when it does not already exist. So on an existing deployment, pulling
a new image with different layers leaves the old `odoo.conf` — and its old
`addons_path` — in place. **The new layers are simply invisible**, with no error
anywhere; Odoo just never looks in the new directories.

The lever already exists: set

```yaml
ODOO_FORCE_OVERWRITE_CONF: "yes"
```

(chart `extraEnvVars`, compose `environment`, or `docker run --env`) and
[`scripts/config.sh`](./scripts/config.sh) regenerates `odoo.conf` on every
boot. Do not invent a second mechanism.

The tradeoff is deliberate and worth stating: with this set, **hand edits to
`odoo.conf` inside the container are discarded on restart**. For a
declaratively-managed deployment that is the correct behaviour — the image is
the source of truth. If you are hand-tuning `odoo.conf` in production, leave it
off and update `addons_path` yourself when you change layers.

### Examples

#### SMTP configuration using a Gmail account

This would be an example of SMTP configuration using a Gmail account:

- Modify the [`docker-compose.yml`](https://github.com/bitnami/containers/blob/main/bitnami/odoo/docker-compose.yml) file present in this repository:

    ```yaml
      odoo:
        ...
        environment:
          - ALLOW_EMPTY_PASSWORD=yes
          - ODOO_SMTP_HOST=smtp.gmail.com
          - ODOO_SMTP_PORT_NUMBER=587
          - ODOO_SMTP_USER=your_email@gmail.com
          - ODOO_SMTP_PASSWORD=your_password
      ...
    ```

- For manual execution:

    ```console
    $ docker run -d --name odoo -p 80:8069 \
      --env ODOO_SMTP_HOST=smtp.gmail.com \
      --env ODOO_SMTP_PORT_NUMBER=587 \
      --env ODOO_SMTP_USER=your_email@gmail.com \
      --env ODOO_SMTP_PASSWORD=your_password \
      --network odoo-tier \
      --volume /path/to/odoo-persistence:/bitnami \
      bitnami/odoo:latest
    ```

#### Connect Odoo container to an existing database

The Bitnami Odoo container supports connecting the Odoo application to an external database. This would be an example of using an external database for Odoo.

- Modify the [`docker-compose.yml`](https://github.com/bitnami/containers/blob/main/bitnami/odoo/docker-compose.yml) file present in this repository:

    ```diff
       odoo:
         ...
         environment:
    -      - ODOO_DATABASE_HOST=mariadb
    +      - ODOO_DATABASE_HOST=mariadb_host
           - ODOO_DATABASE_PORT_NUMBER=3306
    -      - ALLOW_EMPTY_PASSWORD=yes
    +      - ODOO_DATABASE_ADMIN_PASSWORD=odoo_password
         ...
    ```

- For manual execution:

    ```console
    $ docker run -d --name odoo\
      -p 80:8069 \
      --network odoo-network \
      --env ODOO_DATABASE_HOST=mariadb_host \
      --env ODOO_DATABASE_PORT_NUMBER=3306 \
      --env ODOO_DATABASE_ADMIN_PASSWORD=odoo_password \
      --volume odoo_data:/bitnami/odoo \
      bitnami/odoo:latest
    ```

In case the database already contains data from a previous Odoo installation, you need to set the variable `ODOO_SKIP_BOOTSTRAP` to `yes`. Otherwise, the container would execute the installation wizard and could modify the existing data in the database. Note that, when setting `ODOO_SKIP_BOOTSTRAP` to `yes`, values for environment variables such as `ODOO_EMAIL` or `ODOO_PASSWORD` will be ignored.

## Logging

The Bitnami Odoo Docker image sends the container logs to `stdout`. To view the logs:

```console
docker logs odoo
```

Or using Docker Compose:

```console
docker-compose logs odoo
```

You can configure the containers [logging driver](https://docs.docker.com/engine/admin/logging/overview/) using the `--log-driver` option if you wish to consume the container logs differently. In the default configuration docker uses the `json-file` driver.

## Maintenance

### Module sync at release: `odoo.moduleSync`

`odoo.moduleSync` (env `ODOO_MODULE_SYNC`) sets when the database gets the
modules of the image.

| Value              | A boot on an existing database                    | Who installs and updates modules |
|--------------------|---------------------------------------------------|----------------------------------|
| `boot` (default)   | `--init=<installModules>` and `--update=all`      | each boot                        |
| `release`          | no module change                                  | `firestream-odoo-sync`           |

With `release`, the boot finds an existing database by its Odoo tables, not by
a marker file. So the pod can run without a persistent disk. The first boot on
an empty database still initializes it.

`firestream-odoo-sync` is on the image `PATH`. Run it once for each release,
before or right after the new pods start:

1. It installs each module of `installModules` that the database does not have.
2. It updates only the modules whose files changed since the last sync. The
   checksums come from OCA `module_auto_update`. That module must be on the
   addons path and in `installModules`.
3. It saves the new checksums.
4. It prints one JSON line on stdout, for example
   `{"status": "ok", "installed": [], "updated": ["sale_rental"], "baseline": false, "duration_seconds": 41.2, ...}`.
   The Odoo log goes to stderr. A failure prints `"status": "error"` and exits 1.

| Flag         | Effect                                                                  |
|--------------|-------------------------------------------------------------------------|
| `--dry-run`  | Report the modules to install and to update. Write nothing.            |
| `--baseline` | Update every installed module, then save all checksums.                |
| `--save-checksums` | Save the checksums of the installed modules. Update nothing.     |

In release mode, the first init of an empty database records its progress in
the table `firestream_odoo_init`. A boot after an interrupted first init
empties that database and installs again. The files of the first attempt were
on the disk of the stopped pod, and the server never started on that database. At the end, the first init checks that each module of
`installModules` is installed, runs the `odoo.firstInitCalls`, and saves the
module checksums (`firestream-odoo-sync --save-checksums`).

A boot stops with an error when a module change did not finish (a module in
`to install`, `to upgrade` or `to remove`). Set `ODOO_SYNC_ON_BOOT=yes` for one
boot to run `firestream-odoo-sync` before the server starts.

A database with no saved checksums (the first sync) always gets a full update,
because no checksum can prove a module unchanged. The report then says
`"baseline": true`. Use `--baseline` after a restore of an older database.

`firestream-odoo-call MODEL METHOD [--kwargs JSON] [--dry-run]` calls one model
method as the superuser and prints the result as one JSON line. Use it for
one-time data moves.

`--dry-run` works only for a method with a `dry_run` parameter:

- The call passes `dry_run=True` to the method, and it rolls the transaction
  back at the end.
- The transaction is read-only (`SET TRANSACTION READ ONLY`). PostgreSQL
  refuses each write.
- A commit during the call fails the call. Nothing is written.
- The call refuses a method without a `dry_run` parameter, and it does not
  call it. A rollback cannot undo a commit per batch, a file in an object
  store, or a sent mail.

Both commands read the container's `odoo.conf`. In a Job without that file,
they read the `ODOO_DATABASE_*`, `ODOO_ADDONS_PATH` and `ODOO_DATA_DIR`
variables. Two syncs on one database exclude each other with a PostgreSQL
advisory lock.

### Extra Python packages: `odoo.extraPythonPackages`

`odoo.extraPythonPackages = ps: [ ps.gcsfs ];` adds nixpkgs packages for the
image's interpreter. They go on `PYTHONPATH` after the image's own virtual
environment, so a package that both hold keeps the locked version.

### Backup sets: `firestream-odoo-backup`

`firestream-odoo-backup` writes one backup set. A set is one directory:
`<prefix>/<environment>/<UTC timestamp>/`, for example
`gs://acme-odoo-backups/odoo-backups/production/20261014T010000Z/`.

| File               | Content                                                                 |
|--------------------|-------------------------------------------------------------------------|
| `db.dump`          | `pg_dump --format=custom --no-owner --no-privileges` (compressed)       |
| `filestore.tar.gz` | The local filestore, `$ODOO_DATA_DIR/filestore/<db>`. In `objectManifest` mode, only the local files that attachments refer to, if any. |
| `filestore.json`   | Each attachment object in an `fs.storage`, with its size, md5 and the GCS generation that was live at the dump snapshot. Only in `objectManifest` mode. |
| `manifest.json`    | Written last. See below.                                                |

`manifest.json` holds:

- the tag (`nightly`, `pre-release`, `pre-restore` or `manual`) and the environment;
- the Odoo version, and the image: `FIRESTREAM_ODOO_IMAGE`, `FIRESTREAM_ODOO_IMAGE_DIGEST`
  and `FIRESTREAM_ODOO_CODE_ID`, when they are set;
- the installed modules and the `module_auto_update` checksums;
- the database name and size, and the PostgreSQL server version;
- the sha256, md5 and size of each file;
- the attachment counts and the object count;
- the start and end times.

A set without `manifest.json` is incomplete. A restore refuses it.

**Consistency.** `pg_dump` and the attachment queries read one database
snapshot. The object list then names exactly the attachments of the dump. For
each object, the backup lists all its GCS generations once, and it records
the generation that was live at the snapshot time.

**Disk.** `pg_dump` streams into the upload, and the backup hashes the bytes
on the way. The pod needs no disk for the dump.

**Filestore modes** (`--filestore-mode`, or `FIRESTREAM_BACKUP_FILESTORE_MODE`):

- `archive` (the default): a tar of the local filestore.
- `objectManifest`: a list of the objects, for attachments in OCA
  `fs_attachment` storage. The objects are not copied. GCS object versioning
  keeps the old generations, and the list records the generation of each
  object. Attachments in the database are in `db.dump` already.

**Verification.** The backup fails, and writes no manifest, when:

- the stored md5 of a file differs from the local file (GCS), or a read-back
  sha256 differs (the `file` target);
- the object list does not match the `ir_attachment` count;
- a referenced object, or a local file that an attachment refers to, is not
  there. `--allow-missing-objects` records the missing files and finishes
  the set.

An upload never replaces an object (GCS `ifGenerationMatch=0`). So the
backup account needs `roles/storage.objectCreator` only.

On success, the command prints one JSON line, for example
`{"command": "backup", "status": "ok", "set": "gs://...", "db_dump_sha256": "...", "object_count": 812, ...}`.
A failure prints `"status": "error"` and exits 1.

| Flag                      | Default (environment variable)                          |
|---------------------------|---------------------------------------------------------|
| `--target gcs\|file`      | `gcs` (`FIRESTREAM_BACKUP_TARGET`)                      |
| `--bucket NAME`           | (`FIRESTREAM_BACKUP_BUCKET`); an absolute directory for `file` |
| `--prefix PREFIX`         | `odoo-backups` (`FIRESTREAM_BACKUP_PREFIX`)             |
| `--env NAME`              | (`FIRESTREAM_BACKUP_ENV`)                               |
| `--tag TAG`               | `manual` (`FIRESTREAM_BACKUP_TAG`)                      |
| `--filestore-mode MODE`   | `archive` (`FIRESTREAM_BACKUP_FILESTORE_MODE`)          |
| `--allow-missing-objects` | off                                                     |

The `gcs` target uses the default Google credentials: Workload Identity in
GKE. It needs `google-cloud-storage` and `gcsfs` in the image, for example
`odoo.extraPythonPackages = ps: [ ps.gcsfs ps.fsspec ];`.

### Restore: `firestream-odoo-restore`

```console
firestream-odoo-restore --set gs://acme-odoo-backups/odoo-backups/production/20261014T010000Z \
  --database odoo_staging \
  --storage-root platform_filestore=gs://acme-staging-files/filestore \
  --filestore-bucket gs://acme-staging-files/filestore --neutralize
```

`--set latest` takes the newest complete set of `--env` (with `--target`,
`--bucket` and `--prefix`, as the backup does).

1. It reads `manifest.json`. It refuses a set without it.
2. It refuses a set of another Odoo major version (the image version, else
   the `base` module version). `--force` restores it anyway.
3. With `--filestore-bucket`, it checks that the source storage still holds
   each listed object generation. A missing one stops the restore here, before
   any database step, and the error names it.
4. It downloads `db.dump` and checks its sha256 against the manifest.
5. It creates a temporary database, `<database>_restore_<timestamp>`, and runs
   `pg_restore --no-owner --exit-on-error --jobs N` into it. It does not use
   one transaction: a full Odoo database needs more locks than
   `max_locks_per_transaction` gives one transaction. The temporary database
   has a name that no client knows, so a failure only drops it.
6. It checks the `ir_attachment` count against the manifest.
7. It points each `fs.storage` row at its `--storage-root CODE=URI`. Without
   this, a staging copy would write to the production bucket, and its garbage
   collection could delete production files. A restore refuses a database
   with an `fs.storage` row that has no `--storage-root`, unless
   `--keep-storage-roots` is given (a restore into the same environment).
8. `--neutralize` runs Odoo's neutralize (as `odoo-bin neutralize -d`): no
   outgoing mail, no crons, no payment providers.
9. The filestore:
   - `filestore.tar.gz`: unpacked into `$ODOO_DATA_DIR/filestore/<database>`.
   - `filestore.json`: with `--filestore-bucket URI`, it copies each listed
     object generation under that URI, 16 at a time. It lists the
     destination once, keeps each object that exists, and never deletes.
     The URI must equal the `--storage-root` of each storage of the set.
10. It renames the temporary database to `<database>`. A database that
    exists is refused, unless `--replace` is given. Then the restore stops
    new sessions on the old database (`ALLOW_CONNECTIONS false`), ends the
    open ones, and checks again. If clients keep a session, it stops and
    changes nothing. It renames the old database to
    `<database>_before_<timestamp>` and the copy to `<database>` in one
    transaction. The old database keeps `ALLOW_CONNECTIONS false`.

Any failure drops the temporary database, and the target stays as it was.
The Odoo database role needs `CREATEDB`, and it must own the database that
`--replace` renames.

| Flag                         | Use                                                     |
|------------------------------|---------------------------------------------------------|
| `--set URI\|latest`          | The set. Or `--archive PATH`, an `odoo_dump` archive.    |
| `--database NAME`            | The database to create.                                 |
| `--storage-root CODE=URI`    | Repeatable. The new root of the `fs.storage` CODE.      |
| `--keep-storage-roots`       | Keep the source roots (the same environment only).      |
| `--filestore-bucket URI`     | Copy the listed objects there.                          |
| `--neutralize`               | Neutralize the copy before it gets its name.            |
| `--replace`                  | Swap an existing database; keep the old one.            |
| `--force`                    | Restore another Odoo major version.                     |
| `--jobs N`                   | Parallel `pg_restore` jobs. The default is 4.           |
| `--maintenance-db NAME`      | The database for `CREATE DATABASE`. The default is `postgres`. |

**Permissions.** The restore account reads the backup bucket, and it lists
and reads the source filestore bucket (all generations). The backup account
also lists and reads the filestore bucket, to record the generations.

**Retention of the filestore bucket.** Turn on object versioning. Delete
noncurrent generations with the lifecycle condition
`daysSinceNoncurrentTime` (for example 30 days), not `age`. `age` counts from
the creation of a generation, so it can delete a generation that a set in the
window still lists.

### List: `firestream-odoo-backups list`

`firestream-odoo-backups list [--env NAME] [--latest]` prints one JSON line with each set:
its URI, the environment, the timestamp, `complete` (a manifest exists), and
the tag, database, Odoo version, image, object count and dump sha256 from the
manifest. Without `--env` it lists every environment under the prefix. It
takes the same `--target`, `--bucket` and `--prefix` flags as the backup.
`--latest` prints only the newest complete set, as `{"latest": {...}}`. It
fails when there is none.

### Dump and restore helpers

The image ships dump and restore helpers in `/opt/firestream/scripts/libhelpersodoo.sh`
(source: `scripts/helpers.sh`). One archive holds everything a restore needs:

| Member          | Content                                                             |
|-----------------|---------------------------------------------------------------------|
| `manifest.json` | database name, Odoo version, PostgreSQL version, installed modules  |
| `db.dump`       | `pg_dump --format=custom --no-owner --no-privileges` of the DB      |
| `filestore/`    | the attachment store from `$ODOO_DATA_DIR/filestore/<db>`          |

The helpers read the `ODOO_DATABASE_*` connection env (including
`ODOO_DATABASE_PASSWORD_FILE`) and `ODOO_DATA_DIR`.

| Function                      | Purpose                                                         |
|-------------------------------|-----------------------------------------------------------------|
| `odoo_dump <dir>`             | Write `<dir>/odoo-<db>-<timestamp>.tar.gz`; prints the path last |
| `odoo_restore <archive> [db]` | Replace the database and the filestore from an archive          |
| `odoo_backup_s3`              | `odoo_dump` + upload to `s3://$S3_BACKUP_BUCKET/$S3_BACKUP_PREFIX/` |
| `odoo_restore_s3`             | Download `$S3_BACKUP_KEY` and `odoo_restore` it                  |

`odoo_restore` runs `firestream-odoo-restore --archive <archive> --replace
--keep-storage-roots`. A failure leaves the database as it was, and the old
database stays as `<db>_before_<timestamp>`. An archive with the older
`dump.sql` member still restores, through `psql`.

`odoo_restore` refuses to run while an `odoo-bin` process is alive, because a
live restore corrupts sessions and asset bundles. Stop Odoo first, or set
`ODOO_RESTORE_FORCE=yes`. It also refuses an archive from a different Odoo
major version unless `ODOO_RESTORE_FORCE=yes`. After a restore it removes the
sessions directory and writes the `.odoo_initialized` marker, so the next boot
runs a module update instead of a fresh install.

#### Docker

Dump while Odoo runs (the dump is read-only):

```console
docker compose -p firestream-odoo exec odoo bash -c \
  'source /opt/firestream/scripts/libhelpersodoo.sh && odoo_dump /firestream/odoo/backups'
```

Restore with Odoo stopped. The container entrypoint is replaced so the server
does not start, and the data volume stays mounted:

```console
docker compose -p firestream-odoo stop odoo
docker compose -p firestream-odoo run --rm --no-deps --entrypoint bash odoo -c \
  'source /opt/firestream/scripts/libhelpersodoo.sh && odoo_restore /firestream/odoo/backups/odoo-firestream_odoo-<timestamp>.tar.gz'
docker compose -p firestream-odoo start odoo
```

#### Kubernetes

The chart renders a backup CronJob when `backup.enabled=true` (see
`src/charts/firestream/odoo/nix/options/backup.nix`).

| Option                       | Default        | Use                                                   |
|------------------------------|----------------|-------------------------------------------------------|
| `backup.target`              | `s3`           | `s3`: one archive (`odoo_backup_s3`). `gcs`: one set (`firestream-odoo-backup --tag nightly`). |
| `backup.gcs.bucket`          | none           | The GCS bucket. Required for `gcs`.                   |
| `backup.gcs.prefix`          | `odoo-backups` | The prefix in the bucket.                             |
| `backup.environment`         | none           | The environment name in the set path. Required for `gcs`: blue and green write to one folder. |
| `backup.filestoreMode`       | `archive`      | `archive` or `objectManifest`. `s3` allows `archive` only. `objectManifest` does not mount the data volume. |
| `backup.serviceAccountName`  | none           | The Kubernetes service account with the Workload Identity binding. |
| `backup.schedule`            | `@daily`       | The cron schedule.                                    |
| `backup.timeZone`            | none (UTC)     | The CronJob `spec.timeZone` (Kubernetes 1.27 or later). |
| `backup.startingDeadlineSeconds` | `3600`     | Skip a run that cannot start within this time.        |
| `backup.activeDeadlineSeconds`   | `21600`    | Stop a Job that runs longer.                          |

The pod does not mount a service account token
(`automountServiceAccountToken: false`); Workload Identity on GKE does not
need it. The CronJob sets `FIRESTREAM_ODOO_IMAGE` and, from `image.digest`,
`FIRESTREAM_ODOO_IMAGE_DIGEST`.

The `s3` target defaults to the in-cluster SeaweedFS store under
`odoo-backups/`. The CLI drives it:

```console
firestream helm backup odoo
firestream helm restore odoo --from odoo-backups/odoo-firestream_odoo-<timestamp>.tar.gz
```

`restore` scales the Odoo Deployment to zero, waits for the pod to go away,
runs a one-shot Job that inherits the CronJob's pod spec (image, PVC, database
Secret, S3 credentials) and calls `odoo_restore_s3`, then scales the Deployment
back and waits for the rollout. The Deployment is scaled back even when the Job
fails. `firestream helm restore` works with the `s3` target only. For a `gcs`
set, run `firestream-odoo-restore` in a Job.

### Upgrade this image

Bitnami provides up-to-date versions of PostgreSQL and Odoo, including security patches, soon after they are made upstream. We recommend that you follow these steps to upgrade your container. We will cover here the upgrade of the Odoo container. For the PostgreSQL upgrade see: <https://github.com/bitnami/containers/tree/main/bitnami/odoo#user-content-upgrade-this-image>

The `bitnami/odoo:latest` tag always points to the most recent release. To get the most recent release you can simple repull the `latest` tag from the Docker Hub with `docker pull bitnami/odoo:latest`. However it is recommended to use [tagged versions](https://hub.docker.com/r/bitnami/odoo/tags/).

#### Step 1: Get the updated image

```console
docker pull bitnami/odoo:latest
```

#### Step 2: Stop the running container

Stop the currently running container using the command

```console
docker-compose stop odoo
```

#### Step 3: Take a snapshot of the application state

Follow the steps in [Backing up your container](#backing-up-your-container) to take a snapshot of the current application state.

#### Step 4: Remove the currently running container

Remove the currently running container by executing the following command:

```console
docker-compose rm -v odoo
```

#### Step 5: Run the new image

Update the image tag in `docker-compose.yml` and re-create your container with the new image:

```console
docker-compose up -d
```

## Notable Changes

### 12.0.20210615-debian-10-r20, 13.0.20210610-debian-10-r24 and 14.0.20210610-debian-10-r22

- The size of the container image has been decreased.
- The configuration logic is now based on Bash scripts in the *rootfs/* folder.
- It is now possible to use an already populated Odoo database from another installation. In order to do this, use the environment variable `ODOO_SKIP_BOOTSTRAP`, which forces the container not to run the initial Odoo setup wizard.
- Removed port 8071 from list of exposed ports. This port was used by the Odoo XMLRPCS service, but was removed in Odoo 10.
- Added port 8072 to the list of exposed ports. This port is used by the [Odoo Longpolling service](https://www.odoo.com/documentation/14.0/administration/deployment/deploy.html#livechat).
- The `WITHOUT_DEMO` environment variable was deprecated in favor of the boolean `ODOO_LOAD_DEMO_DATA` environment variable.

## Contributing

We'd love for you to contribute to this container. You can request new features by creating an [issue](https://github.com/bitnami/containers/issues) or submitting a [pull request](https://github.com/bitnami/containers/pulls) with your contribution.

## Issues

If you encountered a problem running this container, you can file an [issue](https://github.com/bitnami/containers/issues/new/choose). For us to provide better support, be sure to fill the issue template.

## License

Copyright &copy; 2025 Broadcom. The term "Broadcom" refers to Broadcom Inc. and/or its subsidiaries.

Licensed under the Apache License, Version 2.0 (the "License");
you may not use this file except in compliance with the License.
You may obtain a copy of the License at

<http://www.apache.org/licenses/LICENSE-2.0>

Unless required by applicable law or agreed to in writing, software
distributed under the License is distributed on an "AS IS" BASIS,
WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
See the License for the specific language governing permissions and
limitations under the License.
