{{/*
Expand the name of the chart.
*/}}
{{- define "supabase.envoy.name" -}}
{{- default (print .Chart.Name "-envoy") .Values.deployment.envoy.nameOverride | trunc 63 | trimSuffix "-" }}
{{- end }}

{{/*
Create a default fully qualified app name.
We truncate at 63 chars because some Kubernetes name fields are limited to this (by the DNS naming spec).
If release name contains chart name it will be used as a full name.
*/}}
{{- define "supabase.envoy.fullname" -}}
{{- if .Values.deployment.envoy.fullnameOverride }}
{{- .Values.deployment.envoy.fullnameOverride | trunc 63 | trimSuffix "-" }}
{{- else }}
{{- $name := default (print .Chart.Name "-envoy") .Values.deployment.envoy.nameOverride }}
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
{{- define "supabase.envoy.selectorLabels" -}}
app.kubernetes.io/name: {{ include "supabase.envoy.name" . }}
app.kubernetes.io/instance: {{ .Release.Name }}
{{- end }}

{{/*
Create the name of the service account to use
*/}}
{{- define "supabase.envoy.serviceAccountName" -}}
{{- if .Values.serviceAccount.envoy.create }}
{{- default (include "supabase.envoy.fullname" .) .Values.serviceAccount.envoy.name }}
{{- else }}
{{- default "default" .Values.serviceAccount.envoy.name }}
{{- end }}
{{- end }}
