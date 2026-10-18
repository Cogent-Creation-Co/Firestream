# PostgreSQL chart flake-module (Phase 6b - Agent G1; Phase B image injection)
# Copyright Firestream. MIT License.
#
# Wires the postgresql Helm chart through the options-driven evalChart entrypoint
# and contributes:
#   - packages.postgresql-chart          (deployable chart bundle; builds on darwin)
#   - firestreamCharts.postgresql        (full evaluated chart result, for aggregate)
#   - firestreamChartImages.postgresql   (consumer override API, for flake.lib.charts)
#
# Mirrors nix/flake-modules/charts/airflow.nix. No isLinux gate — `helm template`
# runs in the build sandbox on every platform.
#
# Phase B image injection: an `imageInjectionModule` is appended to the chart's
# modules list so its writes to `_meta.containerRefs` merge into the chart eval.
# It sources the postgresql container's image triple from
# `config.firestreamImages.postgresql.eval (_: {})` and projects it into one
# slot named "postgresql" with `componentPath = [ "image" ]` (the chart's
# top-level `image:` block in values.yaml — Bitnami postgresql is a single-
# image chart). `global.security.allowInsecureImages` is flipped to bypass
# Bitnami's NOTES.txt whitelist (which rejects the `firestream-postgresql`
# repo name).
{ ... }: {
  perSystem = { pkgs, lib, config, evalChart, baseChart, ... }:
    let
      chartSrc = ../../../src/charts/firestream/postgresql;
      optionsPath = chartSrc + "/nix/default.nix";

      # Subcharts vendored from the in-repo Bitnami fork. PostgreSQL's Chart.yaml
      # lists `common` as its only dependency; the engine's vendor-subcharts.nix
      # handles the nested copy.
      subcharts = [
        { name = "common"; }
      ];

      # Source the postgresql container's image triple by re-evaluating the
      # container with no overrides. Reading `.config.postgresql.image` only
      # forces option evaluation - it does NOT force the (Linux-only) docker
      # image build, so this works on Darwin too.
      pgImg =
        let
          imgEval = config.firestreamImages.postgresql.eval (_: {});
          imgCfg = imgEval.config.postgresql.image;
        in {
          registry = imgCfg.registry;          # may be null
          repository = imgCfg.repository;      # always non-null per container schema
          tag = imgEval.imageTag;              # falls back to version when image.tag is null
        };

      # firestream-postgresql container bakes its FHS under
      # /opt/firestream/postgresql/{bin,conf.default,conf,tmp,logs} and
      # /firestream/postgresql/{data,conf}. The chart templates and values.yaml
      # are de-branded to mount/reference those firestream paths directly
      # (Phase 1 path-de-branding), so no per-chart extraEnvVars path-override
      # redirection is needed — the container's baked *_DIR vars already match
      # the chart's mount points.
      imageInjectionModule = { ... }: {
        config.postgresql._meta.containerRefs.postgresql = {
          inherit (pgImg) registry repository tag;
          componentPath = [ "image" ];
        };
        config.postgresql.global.security.allowInsecureImages = true;
        # The Bitnami chart defaults to preloading the `pgaudit` extension;
        # firestream-postgresql ships without it. Disable the preload at the
        # chart level so postgres doesn't refuse to start with
        # "could not access file pgaudit: No such file or directory".
        config.postgresql.postgresqlSharedPreloadLibraries = "";
      };

      # Backup overlay: opt-in scheduled logical dump streamed straight into an
      # S3 target. `storage.enabled = false` removes the throwaway backup PVC —
      # the dump never touches disk, it is piped pg_dumpall | gzip | aws s3 cp -.
      # `command` overrides the values.yaml default (which dumps to a file on the
      # PVC).
      #
      # The S3 destination is driven ENTIRELY by the typed `backup.s3` option
      # (src/charts/firestream/postgresql/nix/options/backup.nix), which an end
      # user overrides via `charts.postgresql.eval`. Defaults target the
      # in-cluster SeaweedFS dev store; the same code path serves real cloud S3
      # when `s3.existingSecret` is set + `s3.endpoint` cleared. We read `config`
      # so consumer overrides of `backup.s3.*` flow through to the rendered env.
      #
      # Cred injection: literal env (dev/SeaweedFS) when `existingSecret` is
      # null; secretKeyRef (secure, for real cloud S3) when it is set. The
      # secret must live in the release namespace (unlike the cross-namespace
      # SeaweedFS secret, which is why dev uses literals — see spark.nix:64-69).
      #
      # `enabled`/`schedule`/`storage.enabled` are mkDefault so a consumer (or
      # the production profile) can flip them via a plain override without a
      # "defined multiple times" conflict.
      backupModule = { config, lib, ... }:
        let
          s3 = config.postgresql.backup.s3;
          credEnv =
            if s3.existingSecret != null then [
              { name = "AWS_ACCESS_KEY_ID";     valueFrom.secretKeyRef = { name = s3.existingSecret; key = s3.accessKeyIdKey; }; }
              { name = "AWS_SECRET_ACCESS_KEY"; valueFrom.secretKeyRef = { name = s3.existingSecret; key = s3.secretAccessKeyKey; }; }
            ] else [
              { name = "AWS_ACCESS_KEY_ID";     value = toString s3.accessKeyId; }
              { name = "AWS_SECRET_ACCESS_KEY"; value = toString s3.secretAccessKey; }
            ];
          backupS3EnvVars = credEnv ++ [
            { name = "AWS_DEFAULT_REGION";  value = s3.region; }
            { name = "S3_ENDPOINT_URL";     value = (if s3.endpoint == null then "" else s3.endpoint); }
            { name = "S3_BACKUP_BUCKET";    value = s3.bucket; }
            { name = "S3_BACKUP_PREFIX";    value = s3.prefix; }
            { name = "S3_ADDRESSING_STYLE"; value = (if s3.addressingStyle == null then "" else s3.addressingStyle); }
            # The backup pod runs with readOnlyRootFilesystem=true; only /tmp is a
            # writable emptyDir. aws CLI v2 needs a writable HOME for its config/cache.
            { name = "HOME";                value = "/tmp"; }
          ];
        in {
          # Read by `firestream helm backup|restore` to find the CronJob
          # (`<fullname>-pgdumpall`). The restore streams into the running
          # primary, so no Deployment quiesce.
          config.postgresql._meta.backup = { cronJobSuffix = "pgdumpall"; };
          config.postgresql.backup = {
            enabled = lib.mkDefault false;   # opt-in; consumer/production flips it on
            cronjob = {
              schedule = lib.mkDefault (if config.postgresql.backup.schedule == null then "@daily" else config.postgresql.backup.schedule);
              storage.enabled = lib.mkDefault false;  # no backup PVC — stream straight to S3
              extraEnvVars = backupS3EnvVars;
              # The Bitnami backup NetworkPolicy default-denies egress except
              # postgres(5432)+DNS(53) — it assumes backup→local-PVC. Firestream
              # streams the dump to an S3 object store, so without this the
              # pg_dumpall pod's upload is REJECTED by kube-router (symptom:
              # botocore "Could not connect"). Open egress to the object store:
              # 8333 (in-cluster SeaweedFS) and 443 (external cloud S3 / HTTPS).
              # Ports-only rule (no `to`) → allowed to any destination on those
              # ports, covering both the in-cluster and cloud-override targets.
              networkPolicy.extraEgress = [
                {
                  ports = [
                    { port = 8333; protocol = "TCP"; }
                    { port = 443;  protocol = "TCP"; }
                  ];
                }
              ];
              command = [
                "bash" "-c"
                ''
                # NB: no `set -e` — the retry loop below owns exit handling.
                set -uo pipefail
                export PGPASSWORD="''${PGPASSWORD:-$(cat "$PGPASSWORD_FILE" 2>/dev/null || true)}"
                export HOME="''${HOME:-/tmp}"
                if [ -n "''${S3_ADDRESSING_STYLE:-}" ]; then
                  aws configure set default.s3.addressing_style "$S3_ADDRESSING_STYLE"
                fi
                # Preflight: wait for the S3 endpoint to be reachable before
                # dumping. A freshly-scheduled Job pod on a busy/just-deployed
                # cluster can take a while before kube-proxy/DNS lets it reach a
                # cross-namespace ClusterIP (manifests as botocore "Could not
                # connect"). A long-lived pod doesn't hit this, but our backup
                # runs seconds after the pod starts. Poll a cheap `s3 ls` until it
                # succeeds (default up to 60*5s = 5m). Tunable via
                # FIRESTREAM_BACKUP_S3_WAIT_ATTEMPTS / _S3_WAIT_INTERVAL.
                waitn="''${FIRESTREAM_BACKUP_S3_WAIT_ATTEMPTS:-60}"
                waiti="''${FIRESTREAM_BACKUP_S3_WAIT_INTERVAL:-5}"
                ready=""
                i=1
                while [ "$i" -le "$waitn" ]; do
                  if aws s3 ls "s3://$S3_BACKUP_BUCKET/" ''${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"} >/dev/null 2>&1; then
                    ready=1; break
                  fi
                  echo "[firestream] waiting for S3 endpoint ($i/$waitn)..." >&2
                  i=$((i + 1)); sleep "$waiti"
                done
                if [ -z "$ready" ]; then
                  echo "[firestream] S3 endpoint unreachable after $waitn attempts" >&2
                  exit 1
                fi
                ts="$(date '+%Y-%m-%d-%H-%M-%S')"
                key="s3://$S3_BACKUP_BUCKET/$S3_BACKUP_PREFIX/pg_dumpall-$ts.sql.gz"
                echo "[firestream] backing up to $key (endpoint=''${S3_ENDPOINT_URL:-<aws-default>})"
                # Upload with a small retry for blips during transfer. The dump
                # is re-run each attempt (streaming, no temp file → no
                # ephemeral-storage ceiling); pg_dumpall is read-only so this is
                # safe. Tunable via FIRESTREAM_BACKUP_MAX_ATTEMPTS.
                max="''${FIRESTREAM_BACKUP_MAX_ATTEMPTS:-3}"
                attempt=1
                while :; do
                  if pg_dumpall --clean --if-exists --load-via-partition-root --quote-all-identifiers --no-password \
                     | gzip -c \
                     | aws s3 cp - "$key" ''${S3_ENDPOINT_URL:+--endpoint-url "$S3_ENDPOINT_URL"}; then
                    echo "[firestream] backup complete: $key"
                    exit 0
                  fi
                  if [ "$attempt" -ge "$max" ]; then
                    echo "[firestream] backup failed after $max attempts" >&2
                    exit 1
                  fi
                  echo "[firestream] backup attempt $attempt failed; retrying in 10s..." >&2
                  attempt=$((attempt + 1))
                  sleep 10
                done
                ''
              ];
            };
          };
        };

      c = evalChart {
        name = "postgresql";
        inherit chartSrc subcharts;
        modules = [ optionsPath imageInjectionModule backupModule ];
      };
    in
    {
      packages.postgresql-chart = c.chartBundle;

      # Base (un-overlaid) chart: chart's OWN native defaults, no Firestream
      # values overlay / no image injection. Renders `bitnami/postgresql`.
      packages.postgresql-base-chart = baseChart {
        name = "postgresql";
        inherit chartSrc subcharts;
      };

      # Registry: full evaluated chart result (used by aggregate.nix / flake.lib).
      firestreamCharts.postgresql = c // { baseChart = config.packages.postgresql-base-chart; };

      # Registry: consumer override API exposed via flake.lib.<sys>.charts.postgresql.
      firestreamChartImages.postgresql = {
        chartBundle = c.chartBundle;
        baseChart = config.packages.postgresql-base-chart;
        render = c.render;
        eval = userMod: evalChart {
          name = "postgresql";
          inherit chartSrc subcharts;
          # Preserve the image injection AND the SeaweedFS backup overlay in user
          # re-evals so consumer overrides still receive the registry-derived
          # image triple and the backup config by default.
          modules = [ optionsPath imageInjectionModule backupModule userMod ];
        };
        options = c.options;
      };
    };
}
