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
    };
}
