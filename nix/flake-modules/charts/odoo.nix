# Odoo chart flake-module (Phase 6b - Agent G7; Phase B image injection)
# Copyright Firestream. MIT License.
#
# Wires the Odoo Helm chart through the options-driven evalChart
# entrypoint and contributes:
#   - packages.odoo-chart            (deployable chart bundle; builds on darwin)
#   - firestreamCharts.odoo          (full evaluated chart result, for aggregate)
#   - firestreamChartImages.odoo     (consumer override API, for flake.lib.charts)
#
# Mirrors nix/flake-modules/charts/superset.nix. No isLinux gate —
# `helm template` runs in the build sandbox on every platform.
#
# The Bitnami Odoo chart bundles `common` + `postgresql` as vendored
# subcharts (Chart.yaml v28.2.7 lists both). Odoo does NOT use redis,
# so the subchart list is shorter than superset's. vendor-subcharts.nix
# consumes the in-repo Bitnami fork at src/charts/bitnami/. The
# recursive vendoring handles postgresql's own `common` dependency.
#
# Phase B image injection: Bitnami Odoo ships ONE main image (bitnami/odoo,
# top-level `image:` shared by app + init containers) plus the bundled
# postgresql subchart. We wire both with firestream-* containers.
# `global.security.allowInsecureImages` is flipped to bypass Bitnami's
# NOTES.txt whitelist.
{ ... }: {
  perSystem = { pkgs, lib, config, evalChart, baseChart, ... }:
    let
      chartSrc = ../../../src/charts/firestream/odoo;
      optionsPath = chartSrc + "/nix/default.nix";

      # Subcharts vendored from the in-repo Bitnami fork. Odoo's
      # Chart.yaml lists `postgresql` (16.x.x) and `common` (2.x.x).
      # NO redis (unlike superset). The engine's vendor-subcharts.nix
      # handles the nested copy AND postgresql's own `common`
      # dependency.
      subcharts = [
        { name = "common"; }
        { name = "postgresql"; }
      ];

      odooImg =
        let
          imgEval = config.firestreamImages.odoo.eval (_: {});
          imgCfg = imgEval.config.odoo.image;
        in {
          registry = imgCfg.registry;
          repository = imgCfg.repository;
          tag = imgEval.imageTag;
        };

      pgImg =
        let
          imgEval = config.firestreamImages.postgresql.eval (_: {});
          imgCfg = imgEval.config.postgresql.image;
        in {
          registry = imgCfg.registry;
          repository = imgCfg.repository;
          tag = imgEval.imageTag;
        };

      # Path de-branding: firestream-odoo now bakes its app tree at
      # /opt/firestream/odoo and its data/volume at /firestream/odoo (see
      # src/containers/firestream/odoo/options.nix + module.nix). The chart
      # templates were de-branded to mount the data PVC at /firestream/odoo and
      # the Secret at /opt/firestream/odoo/secrets — i.e. the chart mount now
      # EQUALS the container's baked ODOO_VOLUME_DIR/ODOO_DATA_DIR. No
      # extraEnvVars path-remap is needed (and any remap would re-introduce a
      # mount/baked mismatch), so the former firestreamPathOverrides is gone.

      # Phase 1 path-de-branding: the postgresql subchart templates + values now
      # mount/reference firestream paths directly, matching the firestream-postgresql
      # container's baked *_DIR vars. No subchart pg path-override extraEnvVars needed.
      imageInjectionModule = { ... }: {
        config.odoo._meta.containerRefs = {
          odoo = {
            inherit (odooImg) registry repository tag;
            componentPath = [ "image" ];
          };
          postgresql = {
            inherit (pgImg) registry repository tag;
            componentPath = [ "postgresql" "image" ];
          };
        };
        config.odoo.global.security.allowInsecureImages = true;
        config.odoo.postgresql.postgresqlSharedPreloadLibraries = "";
      };

      # Backup overlay: CronJob that dumps the database AND the filestore into
      # one archive and uploads it to S3. The odoo chart has no backup
      # template, so the CronJob is appended to `extraDeploy` as a templated
      # YAML string: `templates/extra-list.yaml` passes each item through
      # `common.tplvalues.render`, so the `{{ ... }}` below are evaluated by
      # Helm at render time. That keeps `backup.*` a helm-time value
      # (`--set backup.enabled=true` works, and the stock render is unchanged
      # because the whole block is inside `if .Values.backup.enabled`) and
      # lets the Job resolve the same fullname, image, pull secrets, security
      # context, PVC and database Secret the Deployment does.
      #
      # Credentials: `s3.existingSecret` set → secretKeyRef (the secure path
      # for real cloud S3); empty → literal dev creds (the SeaweedFS secret is
      # cross-namespace, see spark.nix).
      #
      # The Job runs the firestream-odoo image: `pg_dump`, `tar`, `aws` and
      # libhelpersodoo.sh (`odoo_backup_s3`) are baked in. The data PVC is
      # ReadWriteOnce, so the backup Job lands on the Deployment's node and the
      # restore Job needs the Deployment scaled to zero first — hence
      # `_meta.backup.quiesceDeployment`.
      backupModule = { lib, ... }:
        let
          # Deliberately NOT `common.labels.standard` on the pod: the Deployment
          # pod is selected by name+instance, and the restore quiesce waits for
          # those pods to be gone. A finished backup pod carrying the same
          # labels would block that wait until its TTL.
          cronJob = ''
            {{- if .Values.backup.enabled }}
            apiVersion: batch/v1
            kind: CronJob
            metadata:
              name: {{ include "common.names.fullname" . }}-odoodump
              namespace: {{ include "common.names.namespace" . | quote }}
              labels: {{- include "common.labels.standard" ( dict "customLabels" .Values.commonLabels "context" $ ) | nindent 4 }}
                app.kubernetes.io/component: odoodump
            spec:
              schedule: {{ .Values.backup.schedule | quote }}
              concurrencyPolicy: Forbid
              successfulJobsHistoryLimit: {{ .Values.backup.successfulJobsHistoryLimit }}
              failedJobsHistoryLimit: {{ .Values.backup.failedJobsHistoryLimit }}
              jobTemplate:
                spec:
                  backoffLimit: 0
                  {{- if .Values.backup.ttlSecondsAfterFinished }}
                  ttlSecondsAfterFinished: {{ .Values.backup.ttlSecondsAfterFinished }}
                  {{- end }}
                  template:
                    metadata:
                      labels:
                        app.kubernetes.io/component: odoodump
                        app.kubernetes.io/managed-by: firestream
                    spec:
                      restartPolicy: Never
                      {{- include "odoo.imagePullSecrets" . | nindent 10 }}
                      {{- if .Values.podSecurityContext.enabled }}
                      securityContext: {{- include "common.compatibility.renderSecurityContext" (dict "secContext" .Values.podSecurityContext "context" $) | nindent 12 }}
                      {{- end }}
                      containers:
                        - name: odoodump
                          image: {{ template "odoo.image" . }}
                          imagePullPolicy: {{ .Values.image.pullPolicy | quote }}
                          {{- if .Values.containerSecurityContext.enabled }}
                          securityContext: {{- include "common.compatibility.renderSecurityContext" (dict "secContext" .Values.containerSecurityContext "context" $) | nindent 16 }}
                          {{- end }}
                          command:
                            - bash
                            - -c
                            - |
                              set -uo pipefail
                              source /opt/firestream/scripts/libhelpersodoo.sh
                              odoo_backup_s3
                          env:
                            {{- if .Values.backup.s3.existingSecret }}
                            - name: AWS_ACCESS_KEY_ID
                              valueFrom:
                                secretKeyRef:
                                  name: {{ .Values.backup.s3.existingSecret | quote }}
                                  key: {{ .Values.backup.s3.accessKeyIdKey | quote }}
                            - name: AWS_SECRET_ACCESS_KEY
                              valueFrom:
                                secretKeyRef:
                                  name: {{ .Values.backup.s3.existingSecret | quote }}
                                  key: {{ .Values.backup.s3.secretAccessKeyKey | quote }}
                            {{- else }}
                            - name: AWS_ACCESS_KEY_ID
                              value: {{ .Values.backup.s3.accessKeyId | default "" | quote }}
                            - name: AWS_SECRET_ACCESS_KEY
                              value: {{ .Values.backup.s3.secretAccessKey | default "" | quote }}
                            {{- end }}
                            - name: AWS_DEFAULT_REGION
                              value: {{ .Values.backup.s3.region | quote }}
                            - name: S3_ENDPOINT_URL
                              value: {{ .Values.backup.s3.endpoint | default "" | quote }}
                            - name: S3_BACKUP_BUCKET
                              value: {{ .Values.backup.s3.bucket | quote }}
                            - name: S3_BACKUP_PREFIX
                              value: {{ .Values.backup.s3.prefix | quote }}
                            - name: S3_ADDRESSING_STYLE
                              value: {{ .Values.backup.s3.addressingStyle | default "" | quote }}
                            - name: HOME
                              value: /tmp
                            - name: TMPDIR
                              value: /tmp
                            - name: ODOO_DATA_DIR
                              value: /firestream/odoo/data
                            - name: ODOO_DATABASE_HOST
                              value: {{ template "odoo.databaseHost" . }}
                            - name: ODOO_DATABASE_PORT_NUMBER
                              value: {{ template "odoo.databasePort" . }}
                            - name: ODOO_DATABASE_NAME
                              value: {{ template "odoo.databaseName" . }}
                            - name: ODOO_DATABASE_USER
                              value: {{ template "odoo.databaseUser" . }}
                            - name: ODOO_DATABASE_PASSWORD
                              valueFrom:
                                secretKeyRef:
                                  name: {{ include "odoo.databaseSecretName" . }}
                                  key: {{ include "odoo.databaseSecretPasswordKey" . }}
                          {{- if .Values.backup.resources }}
                          resources: {{- toYaml .Values.backup.resources | nindent 16 }}
                          {{- end }}
                          volumeMounts:
                            - name: odoo-data
                              mountPath: /firestream/odoo
                              {{- if .Values.persistence.subPath }}
                              subPath: {{ .Values.persistence.subPath }}
                              {{- end }}
                            - name: tmp
                              mountPath: /tmp
                      volumes:
                        - name: odoo-data
                          {{- if .Values.persistence.enabled }}
                          persistentVolumeClaim:
                            claimName: {{ (tpl .Values.persistence.existingClaim $) | default (include "common.names.fullname" .) }}
                          {{- else }}
                          emptyDir: {}
                          {{- end }}
                        - name: tmp
                          emptyDir: {}
            {{- end }}
          '';
        in {
          config.odoo._meta.backup = {
            cronJobSuffix = "odoodump";
            quiesceDeployment = true;
          };
          config.odoo.extraDeploy = [ cronJob ];
        };

      c = evalChart {
        name = "odoo";
        inherit chartSrc subcharts;
        modules = [ optionsPath imageInjectionModule backupModule ];
      };
    in
    {
      packages.odoo-chart = c.chartBundle;

      # Base (un-overlaid) chart: chart's OWN native defaults, no Firestream
      # values overlay / no image injection. Renders `bitnami/odoo`.
      packages.odoo-base-chart = baseChart {
        name = "odoo";
        inherit chartSrc subcharts;
      };

      # Registry: full evaluated chart result (used by aggregate.nix / flake.lib).
      firestreamCharts.odoo = c // { baseChart = config.packages.odoo-base-chart; };

      # Registry: consumer override API exposed via flake.lib.<sys>.charts.odoo.
      firestreamChartImages.odoo = {
        chartBundle = c.chartBundle;
        baseChart = config.packages.odoo-base-chart;
        render = c.render;
        eval = userMod: evalChart {
          name = "odoo";
          inherit chartSrc subcharts;
          modules = [ optionsPath imageInjectionModule backupModule userMod ];
        };
        options = c.options;
      };
    };
}
