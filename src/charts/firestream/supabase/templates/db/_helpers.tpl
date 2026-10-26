{{/*
Expand the name of the chart.
*/}}
{{- define "supabase.db.name" -}}
{{- default (print .Chart.Name "-db") .Values.deployment.db.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "supabase.db.fullname" -}}
{{- if .Values.deployment.db.fullnameOverride }}
{{- .Values.deployment.db.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default (print .Chart.Name "-db") .Values.deployment.db.nameOverride }}
{{- if contains $name .Release.Name }}
{{- .Release.Name | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- printf "%s-%s" .Release.Name $name | trunc 63 | trimSuffix "-" }}
{{- end }}
{{- end }}
{{- end }}

{{/*
Selector labels
*/}}
{{- define "supabase.db.selectorLabels" -}}
app.kubernetes.io/name: {{ include "supabase.db.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "supabase.db.serviceAccountName" -}}
{{- if .Values.serviceAccount.db.create }}
{{- default (include "supabase.db.fullname" .) .Values.serviceAccount.db.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.db.name }}
{{- end }}
{{- end }}

{{/*
Render a standardized DB host env var.
Usage: include "supabase.db.hostEnv" (dict "root" . "name" "DB_HOST")
*/}}
{{- define "supabase.db.hostEnv" -}}
- name: {{ .name }}
  value: {{ include "supabase.db.fullname" .root | quote }}
{{- end }}

{{/*
Render a standardized DB port env var.
Usage: include "supabase.db.portEnv" (dict "root" . "name" "DB_PORT")
*/}}
{{- define "supabase.db.portEnv" -}}
- name: {{ .name }}
  value: {{ .root.Values.service.db.port | quote }}
{{- end }}

{{/*
A Postgres URL for one Supabase service role. The pod must define
DB_PASSWORD first; Kubernetes expands $(DB_PASSWORD) in place.
Usage: include "supabase.db.url" (dict "root" . "user" "authenticator")
*/}}
{{- define "supabase.db.url" -}}
{{- printf "postgres://%s:$(DB_PASSWORD)@%s:%v/postgres" .user (include "supabase.db.fullname" .root) .root.Values.service.db.port -}}
{{- end }}

{{/*
An init container that waits until the database accepts connections. It runs
the db image without its entrypoint, only for pg_isready.
Usage: include "supabase.db.waitInit" (dict "root" . "securityContext" .Values.deployment.auth.securityContext)
*/}}
{{- define "supabase.db.waitInit" -}}
- name: init-db
  image: {{ include "supabase.image" .root.Values.image.db | quote }}
  imagePullPolicy: {{ .root.Values.image.db.pullPolicy }}
  env:
    {{- include "supabase.db.hostEnv" (dict "root" .root "name" "DB_HOST") | nindent 4 }}
    {{- include "supabase.db.portEnv" (dict "root" .root "name" "DB_PORT") | nindent 4 }}
  command: ["bash", "-c"]
  args:
    - |
      until pg_isready -h "$DB_HOST" -p "$DB_PORT"; do
        echo "Waiting for database to start..."
        sleep 2
      done
      echo "Database is ready"
  securityContext:
    {{- toYaml .securityContext | nindent 4 }}
{{- end }}
