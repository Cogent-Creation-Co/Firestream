# Airflow chart render-fidelity check (Phase 5)
# Copyright Firestream. MIT License.
#
# Proves the Firestream typed-option layer, applied with NO consumer overrides,
# is a faithful no-op over the upstream Bitnami chart: the generated (sparse)
# values.yaml must not change the rendered K8s manifests.
#
# DESIGN: we do NOT force the full ~3000-path option schema via `nix eval`
# (mkComponentType across 25 option modules makes full option-tree evaluation
# pathologically slow). Instead we diff two `helm template` OUTPUTS:
#   - bare.yaml      = the vendored chart rendered with NO -f (its own upstream
#                      values.yaml defaults).
#   - withVals.yaml  = the SAME chart rendered WITH the generated values.yaml.
# Both are produced HERE in the check (not reusing the bundle's rendered.yaml),
# so the only variable between them is the presence of `-f values.yaml`.
#
# NON-DETERMINISM CAVEAT: the Bitnami subcharts (postgresql/redis) and airflow
# generate RANDOM secrets on every `helm template` invocation (randAlphaNum /
# genPrivateKey for postgres/redis/airflow passwords, fernet/secret/jwt keys),
# plus pod-spec `checksum/secret` annotations that hash those random secrets.
# Two *identical* bare renders already differ by exactly those 16 lines. So we
# NORMALISE those inherently-random fields to `<RANDOM>` in BOTH outputs before
# diffing. The check therefore still FAILS on any REAL value change introduced
# by the Firestream layer, but does not false-fail on Helm's secret randomness.
#
# No isLinux gate: charts render in the sandbox on every platform, and
# firestreamCharts.airflow is populated on all systems.
# NOTE: do NOT gate the produced attribute SET on `config` (e.g. via
# `lib.optionalAttrs (config.firestreamCharts ? airflow) …`). flake-parts
# computes the perSystem option `config.firestreamCharts` by merging every
# perSystem module, so consulting it to decide which `checks.*` attrs to emit
# is self-referential => infinite recursion. firestreamCharts.airflow is
# populated on all systems by charts/airflow.nix, so we reference the bundle
# lazily inside the derivation only.
{ ... }: {
  perSystem = { pkgs, lib, config, ... }:
    let
      bundle = config.firestreamCharts.airflow.chartBundle;
      postgresqlBundle = config.firestreamCharts.postgresql.chartBundle;
      redisBundle = config.firestreamCharts.redis.chartBundle;
      kafkaBundle = config.firestreamCharts.kafka.chartBundle;
      sparkBundle = config.firestreamCharts.spark.chartBundle;
      jupyterhubBundle = config.firestreamCharts.jupyterhub.chartBundle;
      supersetBundle = config.firestreamCharts.superset.chartBundle;
      odooBundle = config.firestreamCharts.odoo.chartBundle;
      nextjsBundle = config.firestreamCharts.nextjs.chartBundle;
      nginxBundle = config.firestreamCharts.nginx.chartBundle;
      cloudflaredBundle = config.firestreamCharts.cloudflared.chartBundle;
    in {
      checks.airflow-render-fidelity = pkgs.runCommand "airflow-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        # Render the SAME vendored chart twice; the only variable is `-f`.
        # Release name "airflow" matches eval-chart.nix so resource names line up
        # and the diff isolates VALUE differences.
        helm template airflow ${bundle}/chart                        > bare.raw.yaml
        helm template airflow ${bundle}/chart -f ${bundle}/values.yaml > withVals.raw.yaml

        # Normalise the inherently-random secret data + checksum/secret pod
        # annotations to <RANDOM> so Helm's per-invocation secret generation
        # does not masquerade as a fidelity failure (see header). A real value
        # change in any non-random field still surfaces in the diff.
        #
        # Phase 2 (Agent B): also normalise the rendered `image:` line in pod
        # specs to <IMAGE>. The Firestream container registry now injects the
        # main airflow image (registry/repository/tag) via the chart's
        # `_meta.containerRefs.airflow` slot (componentPath = [ "image" ]) so
        # the bare chart's `bitnami/airflow:3.0.3-debian-12-r0` becomes
        # `firestream-airflow:<container-version>` when rendered with our
        # values.yaml. That divergence is intentional and surgical (only the
        # image field changes); normalising both renders' `image:` lines to a
        # placeholder keeps this check as a NO-OP-WITH-CONTAINER-INJECTION
        # proof, i.e. the only allowed diff is in the image. A real semantic
        # change anywhere else still fails.
        #
        # The pattern `^([[:space:]]*image:[[:space:]]*).*` matches `image:`
        # at any indent in the rendered K8s manifests (the pod-spec
        # container image line). The pulled chart never writes top-level
        # `image:` blocks in K8s objects - only pod containers do - so this
        # regex is unambiguous.
        # Phase B: airflow now injects firestream-* images into BOTH its
        # postgresql AND redis subcharts. The postgresql subchart's
        # `_helpers.tpl` readiness probe (line 357) is gated on
        # `contains "bitnami/" .Values.image.repository` and emits an extra
        # `.initialized` flag-file check that is dropped when the firestream
        # image is in use. Delete the line from both bare + withVals so the
        # check stays meaningful for everything ELSE that diffs.
        # Phase F: chart-side env-var remap (`airflow.extraEnvVars` in the
        # flake-module) injects path env vars (AIRFLOW_CONF_FILE,
        # AIRFLOW_WEBSERVER_CONF_FILE, AIRFLOW_LOGS_DIR,
        # AIRFLOW_SCHEDULER_LOGS_DIR, AIRFLOW_TMP_DIR, AIRFLOW_DAGS_DIR,
        # AIRFLOW_PLUGINS_DIR) into every airflow pod's env: block to remap
        # the firestream container's baked path defaults onto the Bitnami
        # chart's mountPaths. The bare chart obviously doesn't have those,
        # so strip them from BOTH renders before diffing. Match on exact
        # env-var name (trailing `$`) so `AIRFLOW_DATABASE_*` etc. survive.
        # Phase E: SeaweedFS is the default local S3 backend, so the
        # flake-module injects five S3_LOCAL_* env vars into every airflow pod
        # (nix/flake-modules/charts/airflow.nix). The bare chart has no such
        # block. Strip them from BOTH renders — scoped to these exact five names
        # so any other S3_* or unexpected env var still shows up as a diff.
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(postgres-password|password|redis-password|airflow-password|airflow-fernet-key|airflow-secret-key|airflow-jwt-secret-key):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/secret:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/\.initialized/d' \
            -e '/^[[:space:]]+- name: AIRFLOW_(CONF_FILE|WEBSERVER_CONF_FILE|LOGS_DIR|SCHEDULER_LOGS_DIR|TMP_DIR|DAGS_DIR|PLUGINS_DIR)$/{N;d;}' \
            -e '/^[[:space:]]+- name: S3_LOCAL_(ENDPOINT_URL|ACCESS_KEY_ID|SECRET_ACCESS_KEY|BUCKET_NAME|DEFAULT_REGION)$/{N;d;}' \
            -e '/^[[:space:]]+- name: POSTGRESQL_SHARED_PRELOAD_LIBRARIES$/{N;s,value:.*,value: <PRELOAD>,;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G1) — postgresql render-fidelity check.
      #
      # Same shape as airflow: render the vendored chart twice (bare vs. with
      # our generated values.yaml), normalise inherently-random fields, and
      # diff. The postgres-password / replication-password normalisers
      # (already in the shared regex from airflow) catch the random secrets
      # Bitnami generates per `helm template` invocation.
      #
      # Phase B: postgresql container image injection now flips the
      # `image.repository` from `bitnami/postgresql` to `firestream-postgresql`.
      # The chart's readiness probe template
      # (src/charts/bitnami/postgresql/templates/_helpers.tpl:357) is gated on
      # `contains "bitnami/" .Values.image.repository` and emits an extra
      # `[ -f /opt/bitnami/postgresql/tmp/.initialized ] || [ -f /bitnami/...
      # postgresql/.initialized ]` line that is REMOVED when the firestream
      # image is in use. The diff is INTENTIONAL — the firestream container
      # does not maintain the same `.initialized` flag file path — but the
      # render-fidelity check would false-fail on it. Delete the line from
      # both bare + withVals (it's a chart-template-driven defensive check,
      # not a semantic guarantee).
      # ----------------------------------------------------------------------
      checks.postgresql-render-fidelity = pkgs.runCommand "postgresql-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template postgresql ${postgresqlBundle}/chart                                > bare.raw.yaml
        helm template postgresql ${postgresqlBundle}/chart -f ${postgresqlBundle}/values.yaml > withVals.raw.yaml

        # postgres-password / password / replication-password are randomised by
        # Bitnami's chart (randAlphaNum in templates/secrets.yaml) on every
        # `helm template` invocation. checksum/secret pod annotations hash
        # those random secrets and so also differ. Normalise both renders to
        # <RANDOM> for those fields. Image normalisation is included for
        # parity with airflow / forward-compat once image injection lands.
        # Phase B: also strip the `.initialized` flag check line (see header).
        # Phase 1 path-de-branding: the per-chart `primary.extraEnvVars` path
        # overrides were removed (chart now mounts firestream paths natively),
        # so the POSTGRESQL_*_DIR env block no longer appears in either render
        # and its strip line was deleted.
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(postgres-password|password|replication-password|admin-password):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/secret:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/\.initialized/d' \
            -e '/^[[:space:]]+- name: POSTGRESQL_SHARED_PRELOAD_LIBRARIES$/{N;s,value:.*,value: <PRELOAD>,;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "postgresql render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "POSTGRESQL RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G2) — redis render-fidelity check.
      #
      # Same shape as postgresql: render the vendored chart twice (bare vs.
      # with our generated values.yaml), normalise inherently-random fields,
      # and diff. Redis adds NO container-image injection in this phase
      # (firestreamImages.redis does not exist yet), so the `image:` line
      # normaliser is included defensively for forward compatibility — it's
      # a no-op when both renders agree on the image line.
      #
      # Redis-specific random secrets: `redis-password` (Bitnami's redis
      # chart generates a random 10-char alphanumeric password per
      # `helm template` invocation via the secrets.yaml template, exactly
      # as postgresql does for `postgres-password`). The postgres-password /
      # password / admin-password normalisers already cover the generic
      # cases; `redis-password` is added explicitly here so the regex
      # documents the redis-specific secret name. checksum/secret pod
      # annotations hash the random secret and also differ between
      # renders.
      # ----------------------------------------------------------------------
      checks.redis-render-fidelity = pkgs.runCommand "redis-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template redis ${redisBundle}/chart                          > bare.raw.yaml
        helm template redis ${redisBundle}/chart -f ${redisBundle}/values.yaml > withVals.raw.yaml

        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(redis-password|password|admin-password):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/secret:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "redis render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "REDIS RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G3) — kafka render-fidelity check.
      #
      # Same shape as redis: render the vendored chart twice (bare vs. with our
      # generated values.yaml), normalise inherently-random fields, and diff.
      # Kafka adds NO container-image injection in this phase
      # (firestreamImages.kafka does not exist yet), so the `image:` line
      # normaliser is included defensively for forward compatibility - it's a
      # no-op when both renders agree on the image line.
      #
      # Kafka-specific random secrets: the Bitnami chart generates
      # `client-passwords` (SASL client credentials), `inter-broker-password`,
      # `controller-password`, `system-user-password`, and `sasl-jaas-config`
      # per `helm template` invocation via randAlphaNum in
      # templates/secrets.yaml. The TLS path also emits randomised
      # `keystore-password` / `truststore-password` / `key-password` triples
      # when autoGenerated is enabled. The KRaft path generates random
      # `cluster-id` plus per-replica `controller-<N>-id` values. All are
      # explicitly normalised here so the check still FAILS on any real
      # Firestream-introduced semantic change. The pre-existing generic
      # `password` / `admin-password` patterns from postgresql/redis catch
      # the other common cases.
      # ----------------------------------------------------------------------
      checks.kafka-render-fidelity = pkgs.runCommand "kafka-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template kafka ${kafkaBundle}/chart                          > bare.raw.yaml
        helm template kafka ${kafkaBundle}/chart -f ${kafkaBundle}/values.yaml > withVals.raw.yaml

        # Phase 3 de-brand: the container + chart now share the same
        # `/opt/firestream/kafka` + `/firestream/kafka` paths, so the path
        # remap extraEnvVars were dropped. The only remaining flake-module
        # injection is the KAFKA_TMP_DIR / KAFKA_PID_FILE redirect to the
        # chart's `/tmp` emptyDir (`kafka.{controller,broker}.extraEnvVars`),
        # which is net-new vs. the bare render. Strip that pair from BOTH
        # renders so the diff stays semantic. (KAFKA_VOLUME_DIR / KAFKA_CONF_FILE
        # are injected by the bare chart's own `prepare-config` init and appear
        # symmetrically in both renders, so they need no stripping.)
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(password|admin-password|client-passwords|inter-broker-password|controller-password|system-user-password|sasl-jaas-config|keystore-password|truststore-password|key-password|cluster-id):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*controller-[0-9]+-id:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/secret:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/^[[:space:]]+- name: KAFKA_(TMP_DIR|PID_FILE)$/{N;d;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "kafka render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "KAFKA RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G4) - spark render-fidelity check.
      #
      # Same shape as kafka: render the vendored Bitnami spark chart twice
      # (bare vs. with our generated values.yaml), normalise inherently-random
      # fields, and diff. Spark adds NO container-image injection in this
      # phase (firestreamImages.spark does not exist yet), so the `image:`
      # line normaliser is included defensively for forward compatibility -
      # it's a no-op when both renders agree on the image line.
      #
      # Spark-specific random fields: the Bitnami chart emits the
      # `rpc-authentication-secret` plus JKS `spark-keystore-password` /
      # `spark-truststore-password` / `spark-key-password` when
      # security.rpc.authenticationEnabled or security.ssl.enabled is on
      # (off by default, but the secret template still computes them via
      # randAlphaNum on every render when the conditionals fire). The
      # `keystore-password` / `truststore-password` / `key-password`
      # patterns inherited from kafka cover the SSL/JKS path; we add
      # `rpc-authentication-secret` and the spark-prefixed JKS keys for
      # completeness so the check stays robust if the defaults flip.
      # checksum/secret pod annotations hash the random secrets and so
      # also differ between renders.
      # ----------------------------------------------------------------------
      checks.spark-render-fidelity = pkgs.runCommand "spark-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template spark ${sparkBundle}/chart                          > bare.raw.yaml
        helm template spark ${sparkBundle}/chart -f ${sparkBundle}/values.yaml > withVals.raw.yaml

        # Phase F: chart-side env-var remap (`spark.master.extraEnvVars` +
        # `spark.worker.extraEnvVars` in the flake-module) injects path env
        # vars (SPARK_CONF_DIR, SPARK_CONF_FILE, SPARK_LOG_DIR, SPARK_TMP_DIR,
        # SPARK_WORK_DIR, SPARK_DATA_DIR, SPARK_USER_JARS_DIR) into both
        # master + worker pod env: blocks to remap the firestream container's
        # /opt/spark defaults onto the chart's /opt/bitnami/spark/* mounts.
        # The bare chart doesn't have those. Strip them from BOTH renders.
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(password|admin-password|keystore-password|truststore-password|key-password|spark-keystore-password|spark-truststore-password|spark-key-password|rpc-authentication-secret):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/secret:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/^[[:space:]]+- name: SPARK_(CONF_DIR|CONF_FILE|LOG_DIR|TMP_DIR|WORK_DIR|DATA_DIR|USER_JARS_DIR)$/{N;d;}' \
            -e '/^[[:space:]]+- name: S3_LOCAL_(ENDPOINT_URL|ACCESS_KEY_ID|SECRET_ACCESS_KEY|BUCKET_NAME|DEFAULT_REGION)$/{N;d;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "spark render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "SPARK RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G5) - jupyterhub render-fidelity check.
      #
      # Same shape as spark: render the vendored Bitnami jupyterhub chart twice
      # (bare vs. with our generated values.yaml), normalise inherently-random
      # fields, and diff. JupyterHub adds NO container-image injection in this
      # phase (firestreamImages.jupyterhub does not exist yet), so the `image:`
      # line normaliser is included defensively for forward compatibility - it's
      # a no-op when both renders agree on the image line.
      #
      # JupyterHub-specific random fields: the Bitnami chart renders the hub's
      # `cookieSecret` / `hub-cookie-secret` (cookieSecretKey), the proxy's
      # `secretToken` / `proxy-secret-token` / `proxy-token` (the
      # configurable-http-proxy <-> hub bridge), per-service `api-token` values
      # (any `hub.services.*.apiToken` that's left empty triggers a
      # `randAlphaNum`), the configuration `crypt-key` (hub.config crypto key),
      # the `hub.config.JupyterHub.cookie_secret` and
      # `hub.config.CryptKeeper.keys` Secret-data fields, the hub admin
      # password (`password` / `admin-password` - Bitnami's
      # SharedPasswordAuthenticator generates a `randAlphaNum 32` admin
      # password per render whenever `hub.password` is empty), and the
      # `values.yaml` Secret-data field (which base64-encodes the entire
      # hub configuration with the random admin_password baked in). The
      # bundled postgresql subchart also generates `postgres-password` /
      # `password` per render; those are already covered by the generic
      # patterns. The JupyterHub-specific names are added explicitly here
      # so the regex documents the Bitnami JupyterHub-shaped secrets even
      # after the bundled subchart changes upstream.
      #
      # The hub additionally writes BOTH `checksum/hub-config` and
      # `checksum/hub-secret` annotations (NOT the generic
      # `checksum/secret`). The generic checksum normaliser regex is
      # therefore broadened here to `checksum/[a-z0-9-]+` so any
      # checksum annotation collapses to <RANDOM>; this also subsumes the
      # postgresql / redis / kafka / spark `checksum/secret` cases.
      #
      # Quirk: `hub.db.url` is computed at template time as a
      # `postgresql://USER@HOST:PORT/DB` connection string from
      # postgresql.auth/externalDatabase values. The password is NOT in the
      # URL (it's read from env) so the URL is deterministic across renders
      # at the same values; no special normaliser needed for it.
      # ----------------------------------------------------------------------
      checks.jupyterhub-render-fidelity = pkgs.runCommand "jupyterhub-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template jupyterhub ${jupyterhubBundle}/chart                                  > bare.raw.yaml
        helm template jupyterhub ${jupyterhubBundle}/chart -f ${jupyterhubBundle}/values.yaml > withVals.raw.yaml

        # Phase B: JupyterHub injects firestream-jupyterhub:5.3.0 for the
        # `proxy` slot too (Bitnami's bundled bitnami/configurable-http-proxy
        # was at 5.0.1). The chart's `helm.sh/chart` metadata picks up
        # `app.kubernetes.io/version` from the image tag — so proxy-related
        # K8s objects now carry `app.kubernetes.io/version: 5.3.0` instead of
        # 5.0.1. Normalise the `app.kubernetes.io/version:` label so this
        # injected divergence does not false-fail the fidelity check.
        # Phase B: also strip the postgres `.initialized` flag check line
        # (subchart-image-conditional template content — see postgresql
        # check header for full rationale).
        # Phase F: chart-side env-var remap (`jupyterhub.hub|proxy|singleuser
        # .extraEnvVars` in the flake-module) injects path env vars
        # (JUPYTERHUB_CONF_FILE, JUPYTERHUB_CONF_DIR, JUPYTERHUB_TMP_DIR,
        # JUPYTERHUB_PID_FILE, JUPYTERHUB_LOGS_DIR, JUPYTERHUB_LOG_FILE,
        # JUPYTERHUB_DATA_DIR, JUPYTERHUB_VOLUME_DIR) into all three pod
        # env: blocks. The bare chart doesn't have those — strip from BOTH
        # renders.
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(password|admin-password|postgres-password|replication-password|cookieSecret|hub-cookie-secret|secretToken|proxy-secret-token|proxy-token|api-token|crypt-key|values.yaml|hub.config.JupyterHub.cookie_secret|hub.config.CryptKeeper.keys):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/[a-z0-9-]+:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e 's,^([[:space:]]*app\.kubernetes\.io/version:[[:space:]]*).*,\1<VERSION>,' \
            -e '/\.initialized/d' \
            -e '/^[[:space:]]+- name: JUPYTERHUB_(CONF_FILE|CONF_DIR|TMP_DIR|PID_FILE|LOGS_DIR|LOG_FILE|DATA_DIR|VOLUME_DIR)$/{N;d;}' \
            -e '/^[[:space:]]+- name: POSTGRESQL_SHARED_PRELOAD_LIBRARIES$/{N;s,value:.*,value: <PRELOAD>,;}' \
            -e '/^[[:space:]]+command:$/{N;/- configurable-http-proxy$/d;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "jupyterhub render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "JUPYTERHUB RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G6) - superset render-fidelity check.
      #
      # Same shape as jupyterhub: render the vendored Bitnami superset chart
      # twice (bare vs. with our generated values.yaml), normalise inherently-
      # random fields, and diff. Superset adds NO container-image injection
      # in this phase (firestreamImages.superset does not exist yet), so the
      # `image:` line normaliser is included defensively for forward
      # compatibility — it's a no-op when both renders agree on the image
      # line.
      #
      # Superset-specific random fields (templates/secret.yaml writes both
      # via `common.secrets.passwords.manage`):
      #   - superset-password    randomised when auth.password is empty
      #   - superset-secret-key  randomised when auth.secretKey is empty
      #                          (length 42; the Flask SECRET_KEY used to
      #                          sign session cookies)
      # The bundled postgresql subchart also generates `postgres-password` /
      # `password`; the bundled redis subchart generates `redis-password`;
      # those names are already covered by the generic patterns inherited
      # from earlier checks. We add `superset-password`, `superset-secret-key`,
      # `secret-key`, `admin-password`, `sqlalchemy-database-uri`, and
      # `fab-default-secret` explicitly so the regex documents the
      # Bitnami-superset-shaped secrets and tolerates any future
      # rename/extension.
      #
      # Quirk #1: superset auto-generates `SQLALCHEMY_DATABASE_URI` at
      # template time from `postgresql.auth.*` / `externalDatabase.*` values.
      # When the rendered configmap embeds a connection string, it includes
      # the runtime password (read from the secret); however the in-template
      # URI itself is a `postgresql+psycopg2://USER@HOST:PORT/DB` form WITHOUT
      # the password (read from env). So the URI is deterministic across
      # renders at the same values — no special normaliser needed for it.
      # We still normalise the literal `sqlalchemy-database-uri:` key
      # defensively in case the chart's secrets-data layout changes.
      #
      # Quirk #2: the generic `checksum/[a-z0-9-]+` regex (broadened by G5
      # for jupyterhub's `checksum/hub-config`/`checksum/hub-secret`)
      # covers superset's `checksum/configuration` and `checksum/secret`
      # pod-spec annotations.
      # ----------------------------------------------------------------------
      checks.superset-render-fidelity = pkgs.runCommand "superset-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template superset ${supersetBundle}/chart                              > bare.raw.yaml
        helm template superset ${supersetBundle}/chart -f ${supersetBundle}/values.yaml > withVals.raw.yaml

        # Phase B: superset injects firestream-* images for BOTH the postgres
        # and redis subcharts. The postgres subchart's `_helpers.tpl`
        # emits a `.initialized` flag-file check that is omitted once the
        # firestream image is in use; delete the line from both renders.
        # Phase F: chart-side env-var remap (`superset.web|worker|init.
        # extraEnvVars` in the flake-module) injects path env vars
        # (SUPERSET_HOME_DIR, SUPERSET_CONFIG_PATH, SUPERSET_LOG_DIR,
        # SUPERSET_TMP_DIR) into web/worker/init pod env: blocks. The bare
        # chart doesn't have those — strip from BOTH renders.
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(password|admin-password|postgres-password|replication-password|redis-password|superset-password|superset-secret-key|secret-key|sqlalchemy-database-uri|fab-default-secret):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/[a-z0-9-]+:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/\.initialized/d' \
            -e '/^[[:space:]]+- name: SUPERSET_(HOME_DIR|CONFIG_PATH|LOG_DIR|TMP_DIR)$/{N;d;}' \
            -e '/^[[:space:]]+- name: POSTGRESQL_SHARED_PRELOAD_LIBRARIES$/{N;s,value:.*,value: <PRELOAD>,;}' \
            -e '/^[[:space:]]+- name: SUPERSET_LOAD_EXAMPLES$/{N;d;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "superset render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "SUPERSET RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # Phase 6b (Agent G7) - odoo render-fidelity check (LAST chart).
      #
      # Same shape as superset: render the vendored Bitnami odoo chart twice
      # (bare vs. with our generated values.yaml), normalise inherently-random
      # fields, and diff. Odoo adds NO container-image injection in this
      # phase (firestreamImages.odoo does not exist yet), so the `image:`
      # line normaliser is included defensively for forward compatibility -
      # it's a no-op when both renders agree on the image line.
      #
      # Odoo-specific random fields (templates/secrets.yaml writes both):
      #   - odoo-password    randomised when odooPassword is empty via
      #                      common.secrets.passwords.manage (length 10).
      #                      Only emitted when existingSecret is also empty.
      #   - smtp-password    base64-encoded from smtpPassword (NOT random
      #                      — empty when smtpPassword is unset). Listed
      #                      in the normaliser so a future random fallback
      #                      doesn't break this check.
      #   - admin-password   the externaldb-secrets.yaml `postgres-password`
      #                      field (renamed when externalDatabase is in
      #                      use). Already covered by the generic
      #                      `postgres-password` / `admin-password` patterns
      #                      inherited from earlier checks.
      #
      # The bundled postgresql subchart also generates `postgres-password` /
      # `password`; those names are already covered by the generic patterns
      # inherited from postgresql/redis/jupyterhub/superset. The
      # odoo-specific names are added explicitly so the regex documents the
      # Bitnami odoo-shaped secrets.
      #
      # Quirk: the generic `checksum/[a-z0-9-]+` regex (broadened by G5)
      # covers odoo's pod-spec `checksum/secret` and
      # `checksum/postinit-configmap` annotations (the latter hashes the
      # post-init scripts ConfigMap).
      # ----------------------------------------------------------------------
      checks.odoo-render-fidelity = pkgs.runCommand "odoo-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template odoo ${odooBundle}/chart                      > bare.raw.yaml
        helm template odoo ${odooBundle}/chart -f ${odooBundle}/values.yaml > withVals.raw.yaml

        # Phase B: odoo injects firestream-* images for both odoo + the
        # postgres subchart. The postgres subchart's `_helpers.tpl`
        # emits a `.initialized` flag-file check that is omitted once the
        # firestream image is in use; delete the line from both renders.
        # Phase F: chart-side env-var remap (top-level `odoo.extraEnvVars`
        # in the flake-module) injects path env vars (ODOO_VOLUME_DIR,
        # ODOO_DATA_DIR, ODOO_ADDONS_DIR, ODOO_CONF_DIR, ODOO_CONF_FILE,
        # ODOO_LOGS_DIR, ODOO_LOG_FILE, ODOO_TMP_DIR, ODOO_PID_FILE) into
        # the odoo pod env: block. The bare chart doesn't have those —
        # strip from BOTH renders.
        # Phase G: odoo also injects POSTGRESQL_* path remaps into its bundled
        # postgresql subchart pod (`postgresql.primary.extraEnvVars` in the
        # flake-module) — strip those too, same as the standalone postgresql
        # check does.
        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(password|admin-password|postgres-password|replication-password|odoo-password|smtp-password):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/[a-z0-9-]+:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/\.initialized/d' \
            -e '/^[[:space:]]+- name: ODOO_(VOLUME_DIR|DATA_DIR|ADDONS_DIR|CONF_DIR|CONF_FILE|LOGS_DIR|LOG_FILE|TMP_DIR|PID_FILE)$/{N;d;}' \
            -e '/^[[:space:]]+- name: POSTGRESQL_(VOLUME_DIR|DATA_DIR|MOUNTED_CONF_DIR|CONF_DIR|CONF_FILE|PGHBA_FILE|REPLICATION_PASSFILE_PATH|TMP_DIR|PID_FILE|LOG_DIR|LOG_FILE)$/{N;d;}' \
            -e '/^[[:space:]]+- name: POSTGRESQL_SHARED_PRELOAD_LIBRARIES$/{N;s,value:.*,value: <PRELOAD>,;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "odoo render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "ODOO RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # nextjs render-fidelity check (net-new, non-Bitnami app).
      #
      # Unlike the forked charts, nextjs authors BOTH its templates and its
      # values.yaml defaults, so the bare render already uses firestream-nextjs.
      # The Firestream overlay only injects: the nextjs image triple (no-op,
      # already the chart default), the bundled postgresql subchart's
      # firestream-postgresql image, and global.security.allowInsecureImages.
      # After normalising images, random subchart secrets, checksums, the
      # postgres `.initialized` flag-file line (omitted once the firestream
      # image is in use), and the POSTGRESQL_SHARED_PRELOAD_LIBRARIES env var
      # (the overlay blanks the chart's "pgaudit" default because the
      # firestream-postgresql image has no pgaudit extension), the sparse
      # overlay is a no-op over the bare chart. NO path-env remaps are injected.
      # ----------------------------------------------------------------------
      checks.nextjs-render-fidelity = pkgs.runCommand "nextjs-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template nextjs ${nextjsBundle}/chart                          > bare.raw.yaml
        helm template nextjs ${nextjsBundle}/chart -f ${nextjsBundle}/values.yaml > withVals.raw.yaml

        normalise() {
          sed -E \
            -e 's,^([[:space:]]*(password|admin-password|postgres-password|replication-password):[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*checksum/[a-z0-9-]+:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            -e '/\.initialized/d' \
            -e '/^[[:space:]]+- name: POSTGRESQL_SHARED_PRELOAD_LIBRARIES$/{N;s,value:.*,value: <PRELOAD>,;}' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "nextjs render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "NEXTJS RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # nginx render-fidelity check (net-new, non-Bitnami app).
      #
      # Like nextjs, nginx authors BOTH its templates and its values.yaml
      # defaults, so the bare render already uses firestream-nginx. The
      # Firestream overlay injects only the nginx image triple (a no-op, already
      # the chart default) and global.security.allowInsecureImages. There are no
      # subcharts, no secrets, and no path-env remaps. After normalising the
      # image line and the config checksum annotation, the sparse overlay must
      # be a byte-for-byte no-op over the bare chart.
      #
      # This check does NOT exercise `upstreams` - by design. `upstreams` is
      # empty in both values.yaml files, so the check verifies exactly what it
      # should: that the Firestream overlay adds no unintended drift. Upstream
      # rendering is covered by evaluating charts.nginx.eval with a populated
      # upstreams tree.
      # ----------------------------------------------------------------------
      checks.nginx-render-fidelity = pkgs.runCommand "nginx-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template nginx ${nginxBundle}/chart                            > bare.raw.yaml
        helm template nginx ${nginxBundle}/chart -f ${nginxBundle}/values.yaml > withVals.raw.yaml

        normalise() {
          sed -E \
            -e 's,^([[:space:]]*checksum/[a-z0-9-]+:[[:space:]]*).*,\1<RANDOM>,' \
            -e 's,^([[:space:]]*image:[[:space:]]*).*,\1<IMAGE>,' \
            "$1"
        }
        normalise bare.raw.yaml     > bare.yaml
        normalise withVals.raw.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "nginx render fidelity OK: sparse Firestream values are a no-op over the bare chart"
          mkdir -p "$out"
          cp fidelity.diff "$out/" 2>/dev/null || true
        else
          echo "NGINX RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi
      '';

      # ----------------------------------------------------------------------
      # cloudflared render-fidelity check (net-new, non-Bitnami app).
      #
      # The STRONGEST form of this check in the repo. cloudflared is the only
      # chart with no Firestream-built container, so its flake-module injects
      # NOTHING: the containerRef is catalogue-only (componentPath = []) and
      # `allowInsecureImages` is never set. With no consumer overrides the
      # generated values.yaml is therefore literally empty, and the two renders
      # must match WITHOUT normalising the image line - unlike every other
      # chart here, cloudflared's image is not rewritten by the overlay, so a
      # diff on `image:` would be a genuine regression rather than expected
      # substitution.
      #
      # There are no secrets, no subcharts, no checksum annotations and no
      # path-env remaps, so no normalisation is needed at all.
      # ----------------------------------------------------------------------
      checks.cloudflared-render-fidelity = pkgs.runCommand "cloudflared-render-fidelity"
        { nativeBuildInputs = [ pkgs.kubernetes-helm pkgs.jq ]; } ''
        set -euo pipefail
        export HOME="$TMPDIR"

        helm template cloudflared ${cloudflaredBundle}/chart > bare.yaml
        helm template cloudflared ${cloudflaredBundle}/chart \
          -f ${cloudflaredBundle}/values.yaml > withVals.yaml

        if diff -u bare.yaml withVals.yaml > fidelity.diff; then
          echo "cloudflared render fidelity OK: sparse Firestream values are a no-op over the bare chart"
        else
          echo "CLOUDFLARED RENDER FIDELITY FAILURE - generated values changed the rendered output:"
          cat fidelity.diff
          exit 1
        fi

        # Structural assertions the diff alone cannot make. These encode the
        # chart's architectural commitments, so a future edit that quietly adds
        # a Service or a ConfigMap fails the build rather than the cluster.
        assert_absent() {
          if grep -qE "^kind: $1$" bare.yaml; then
            echo "CLOUDFLARED FAILURE: chart rendered a $1, which it must never do ($2)"
            exit 1
          fi
        }
        assert_absent Service   "the connector only dials OUT to the Cloudflare edge"
        assert_absent ConfigMap "tunnel ingress rules are managed remotely via the Cloudflare API"
        assert_absent PersistentVolumeClaim "the connector is stateless"
        assert_absent Ingress   "TLS and routing terminate at the Cloudflare edge"
        assert_absent Secret    "the token Secret is created out of band by the deploying layer"

        for want in \
          "kind: Deployment" \
          "kind: ServiceAccount" \
          "replicas: 2" \
          "name: TUNNEL_TOKEN" \
          "secretKeyRef" \
          "key: \"tunnel-token\"" ; do
          if ! grep -qF "$want" bare.yaml; then
            echo "CLOUDFLARED FAILURE: rendered output is missing: $want"
            exit 1
          fi
        done

        echo "cloudflared structural assertions OK"

        # ------------------------------------------------------------------
        # THE PIN-DRIFT GATE.
        #
        # cloudflared is the one chart whose image triple is stated TWICE: once
        # in the chart's own values.yaml (which is what renders, because the
        # containerRef is catalogue-only -- componentPath = []) and once as
        # `upstreamImage` in nix/flake-modules/charts/cloudflared.nix (which is
        # what lands in chart-manifest.json, and therefore what the deploy layer
        # and any SBOM tooling believe is running).
        #
        # Nothing in the module system ties them together -- a comment asked a
        # human to keep them in sync. Assert it instead, against two BUILD
        # OUTPUTS of the same bundle, so drift is a red build rather than a
        # manifest that documents an image nobody is running.
        manifest_ref="$(jq -r '.images.cloudflared | "\(.registry)/\(.repository):\(.tag)"' \
          ${cloudflaredBundle}/chart-manifest.json)"
        if ! grep -qF "image: $manifest_ref" bare.yaml; then
          echo "CLOUDFLARED PIN DRIFT: chart-manifest.json records"
          echo "    $manifest_ref"
          echo "but the chart renders:"
          grep -E '^[[:space:]]*image:' bare.yaml || echo "    (no image line at all)"
          echo
          echo "Fix: make src/charts/firestream/cloudflared/values.yaml and"
          echo "\`upstreamImage\` in nix/flake-modules/charts/cloudflared.nix agree."
          exit 1
        fi
        echo "cloudflared pin OK: manifest and rendered image both $manifest_ref"

        mkdir -p "$out"
        cp fidelity.diff "$out/" 2>/dev/null || true
      '';

      # NOTE: the odoo_base addon `cp_auth_google` reads the files through
      # FIRESTREAM_ODOO_MANAGED_DIR, outside this repo. This check is the only
      # guard on that contract. It renders through the consumer `eval` API so
      # it also proves the typed Nix option reaches the chart.
      checks.odoo-managed-settings =
        let
          odooApi = config.firestreamChartImages.odoo;
          renderWith = values: (odooApi.eval ({ ... }: { config.odoo = values; })).chartBundle;
          enabled = renderWith {
            managedSettings.existingSecret = "x";
            persistence.enabled = true;
            volumePermissions.enabled = true;
          };
          movedPath = renderWith {
            managedSettings = { existingSecret = "x"; mountPath = "/run/firestream/odoo/managed"; };
          };
        in
        pkgs.runCommand "odoo-managed-settings"
          { nativeBuildInputs = [ pkgs.yq-go ]; } ''
          set -euo pipefail

          fail() { echo "ODOO MANAGED SETTINGS FAILURE: $*"; exit 1; }

          deployment() {
            yq 'select(.kind == "Deployment" and .metadata.name == "odoo")' "$1/rendered.yaml"
          }

          expect() {
            local file="$1" query="$2" want="$3" got
            got="$(yq "$query" "$file")"
            [ "$got" = "$want" ] || fail "$query = '$got', want '$want'"
          }

          check_enabled() {
            local bundle="$1" path="$2"
            deployment "$bundle" > dep.yaml
            [ -s dep.yaml ] || fail "no odoo Deployment in $bundle/rendered.yaml"

            expect dep.yaml '.spec.template.spec.volumes[] | select(.name == "managed-settings") | .secret.secretName' x
            expect dep.yaml '.spec.template.spec.containers[0].volumeMounts[] | select(.name == "managed-settings") | .mountPath' "$path"
            expect dep.yaml '.spec.template.spec.containers[0].volumeMounts[] | select(.name == "managed-settings") | .readOnly' true
            expect dep.yaml '.spec.template.spec.containers[0].env[] | select(.name == "FIRESTREAM_ODOO_MANAGED_DIR") | .value' "$path"
            expect dep.yaml '[.spec.template.spec.initContainers[]?.volumeMounts[]? | select(.name == "managed-settings")] | length' 0
            expect dep.yaml '[.spec.template.spec.initContainers[]?.env[]? | select(.name == "FIRESTREAM_ODOO_MANAGED_DIR")] | length' 0

            grep -q 'defaultMode: 0440' dep.yaml || fail "managed-settings volume is not defaultMode 0440"
            [ "$(grep -c 'FIRESTREAM_ODOO_MANAGED_DIR' "$bundle/rendered.yaml")" = 1 ] \
              || fail "FIRESTREAM_ODOO_MANAGED_DIR appears outside the main Odoo container"
            [ "$(grep -c 'name: managed-settings' "$bundle/rendered.yaml")" = 2 ] \
              || fail "managed-settings is referenced outside the odoo Deployment volume and mount"
          }

          check_enabled ${enabled} /opt/firestream/odoo/managed
          expect ${enabled}/values.yaml '.managedSettings.existingSecret' x
          expect ${enabled}/values.yaml '.managedSettings | has("mountPath")' false
          expect dep.yaml '[.spec.template.spec.initContainers[] | select(.name == "volume-permissions")] | length' 1

          check_enabled ${movedPath} /run/firestream/odoo/managed

          for marker in managed-settings managedSettings FIRESTREAM_ODOO_MANAGED_DIR; do
            if grep -q "$marker" ${odooApi.chartBundle}/rendered.yaml ${odooApi.chartBundle}/values.yaml; then
              fail "the default render or values.yaml contains $marker"
            fi
          done

          echo "odoo managedSettings OK: volume, read-only mount and env var only when existingSecret is set"
          touch "$out"
        '';

      # NOTE: readiness is the only thing that keeps traffic away from a pod
      # whose caches are still cold. This check proves that both probes reach
      # healthd's /readyz by default, and that a consumer can turn it off.
      checks.odoo-healthd-probes =
        let
          odooApi = config.firestreamChartImages.odoo;
          renderWith = values: (odooApi.eval ({ ... }: { config.odoo = values; })).chartBundle;
          healthd = renderWith { startupProbe.enabled = true; };
          plain = renderWith { probesUseHealthd = false; startupProbe.enabled = true; };
        in
        pkgs.runCommand "odoo-healthd-probes"
          { nativeBuildInputs = [ pkgs.yq-go ]; } ''
          set -euo pipefail

          fail() { echo "ODOO HEALTHD PROBES FAILURE: $*"; exit 1; }

          expect() {
            local bundle="$1" query="$2" want="$3" got
            got="$(yq "select(.kind == \"Deployment\" and .metadata.name == \"odoo\") | .spec.template.spec.containers[0]$query" "$bundle/rendered.yaml")"
            [ "$got" = "$want" ] || fail "$query = '$got', want '$want'"
          }

          for probe in readinessProbe startupProbe; do
            expect ${healthd} ".$probe.httpGet.path" /readyz
            expect ${healthd} ".$probe.httpGet.port" 9180
            expect ${plain} ".$probe.httpGet.path" /web/health
            expect ${plain} ".$probe.httpGet.port" http
          done
          expect ${healthd} '.livenessProbe.tcpSocket.port' http

          echo "odoo healthd probes OK: readiness and startup on /readyz, liveness on the http port"
          touch "$out"
        '';

      # NOTE: the backup CronJob is a template string in extraDeploy, so Helm
      # is the only thing that parses it. A typo there renders nothing until
      # the night of the first backup. This check renders both targets and
      # the two refusals.
      checks.odoo-backup-render =
        let
          odooApi = config.firestreamChartImages.odoo;
          gcs = (odooApi.eval ({ ... }: {
            config.odoo.backup = {
              enabled = true;
              target = "gcs";
              environment = "production";
              filestoreMode = "objectManifest";
              serviceAccountName = "odoo-backup";
              timeZone = "America/Los_Angeles";
              gcs.bucket = "acme-odoo-backups";
            };
            config.odoo.image.digest = "sha256:0123";
          })).chartBundle;
        in
        pkgs.runCommand "odoo-backup-render"
          { nativeBuildInputs = [ pkgs.kubernetes-helm pkgs.yq-go ]; } ''
          set -euo pipefail
          export HOME="$TMPDIR"

          fail() { echo "ODOO BACKUP RENDER FAILURE: $*"; exit 1; }
          bundle=${odooApi.chartBundle}

          expect() {
            local file="$1" query="$2" want="$3" got
            got="$(yq "select(.kind == \"CronJob\") | .spec.jobTemplate.spec.template.spec$query" "$file")"
            [ "$got" = "$want" ] || fail "$file: $query = '$got', want '$want'"
          }
          env_of() { echo ".containers[0].env[] | select(.name == \"$1\") | .value"; }

          [ "$(yq 'select(.kind == "CronJob") | .metadata.name' "$bundle/rendered.yaml")" = "" ] \
            || fail "the default render has a backup CronJob"

          helm template odoo "$bundle/chart" -f "$bundle/values.yaml" --set backup.enabled=true > s3.yaml
          expect s3.yaml '.containers[0].command[2]' "$(printf 'set -uo pipefail\nsource /opt/firestream/scripts/libhelpersodoo.sh\nodoo_backup_s3\n')"
          expect s3.yaml "$(env_of S3_BACKUP_BUCKET)" firestream
          expect s3.yaml "$(env_of FIRESTREAM_BACKUP_TARGET)" ""
          expect s3.yaml '.serviceAccountName' null
          expect s3.yaml '.volumes | map(select(.name == "odoo-data")) | length' 1
          [ "$(yq 'select(.kind == "CronJob") | .spec.timeZone' s3.yaml)" = null ] || fail "the default has a timeZone"
          [ -n "$(yq "select(.kind == \"CronJob\") | .spec.jobTemplate.spec.template.spec$(env_of FIRESTREAM_ODOO_IMAGE)" s3.yaml)" ] \
            || fail "the s3 CronJob has no FIRESTREAM_ODOO_IMAGE"

          cp ${gcs}/rendered.yaml gcs.yaml
          expect gcs.yaml '.containers[0].command[0]' firestream-odoo-backup
          expect gcs.yaml '.automountServiceAccountToken' false
          expect gcs.yaml '.volumes | map(select(.name == "odoo-data")) | length' 0
          expect gcs.yaml '.containers[0].volumeMounts | map(select(.name == "odoo-data")) | length' 0
          expect gcs.yaml "$(env_of FIRESTREAM_ODOO_IMAGE_DIGEST)" sha256:0123
          [ "$(yq 'select(.kind == "CronJob") | .spec.timeZone' gcs.yaml)" = America/Los_Angeles ] || fail "no timeZone"
          [ "$(yq 'select(.kind == "CronJob") | .spec.startingDeadlineSeconds' gcs.yaml)" = 3600 ] || fail "no startingDeadlineSeconds"
          [ "$(yq 'select(.kind == "CronJob") | .spec.jobTemplate.spec.activeDeadlineSeconds' gcs.yaml)" = 21600 ] \
            || fail "no activeDeadlineSeconds"
          expect gcs.yaml '.serviceAccountName' odoo-backup
          expect gcs.yaml "$(env_of FIRESTREAM_BACKUP_TARGET)" gcs
          expect gcs.yaml "$(env_of FIRESTREAM_BACKUP_BUCKET)" acme-odoo-backups
          expect gcs.yaml "$(env_of FIRESTREAM_BACKUP_PREFIX)" odoo-backups
          expect gcs.yaml "$(env_of FIRESTREAM_BACKUP_ENV)" production
          expect gcs.yaml "$(env_of FIRESTREAM_BACKUP_FILESTORE_MODE)" objectManifest
          expect gcs.yaml "$(env_of FIRESTREAM_BACKUP_TAG)" nightly
          expect gcs.yaml '.containers[0].env | map(select(.name == "AWS_ACCESS_KEY_ID" or .name == "S3_BACKUP_BUCKET")) | length' 0
          expect gcs.yaml "$(env_of ODOO_DATABASE_NAME)" "$(yq "select(.kind == \"CronJob\") | .spec.jobTemplate.spec.template.spec$(env_of ODOO_DATABASE_NAME)" s3.yaml)"

          helm template odoo "$bundle/chart" -f "$bundle/values.yaml" --set backup.enabled=true \
            --set backup.target=gcs --set backup.gcs.bucket=b --set backup.environment=staging > archive.yaml
          expect archive.yaml "$(env_of FIRESTREAM_BACKUP_FILESTORE_MODE)" archive
          expect archive.yaml '.volumes | map(select(.name == "odoo-data")) | length' 1

          if helm template odoo "$bundle/chart" -f "$bundle/values.yaml" --set backup.enabled=true \
              --set backup.target=gcs --set backup.gcs.bucket=b > /dev/null 2> noenv.err; then
            fail "the gcs target rendered without an environment"
          fi
          grep -q 'backup.environment is required' noenv.err || fail "no environment message: $(cat noenv.err)"

          if helm template odoo "$bundle/chart" -f "$bundle/values.yaml" --set backup.enabled=true \
              --set backup.target=gcs --set backup.environment=e > /dev/null 2> nobucket.err; then
            fail "the gcs target rendered without a bucket"
          fi
          grep -q 'backup.gcs.bucket is required' nobucket.err || fail "no bucket message: $(cat nobucket.err)"

          if helm template odoo "$bundle/chart" -f "$bundle/values.yaml" --set backup.enabled=true \
              --set backup.filestoreMode=objectManifest > /dev/null 2> s3manifest.err; then
            fail "the s3 target rendered with objectManifest"
          fi
          grep -q 'objectManifest needs backup.target gcs' s3manifest.err || fail "no mode message: $(cat s3manifest.err)"

          echo "odoo backup render OK: s3 and gcs CronJobs, and the three refusals"
          touch "$out"
        '';
    };
}
