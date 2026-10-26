{{/*
Expand the name of the generated key Secret.
*/}}
{{- define "supabase.secret.supabase" -}}
{{- printf "%s-keys" (include "supabase.fullname" .) }}
{{- end -}}

{{/*
Name of the Secret that holds the Supabase key set. Its keys follow the
layout of `supabase-keygen --output k8s-secret` (POSTGRES_PASSWORD,
JWT_SECRET, ANON_KEY, ...).
*/}}
{{- define "supabase.secret.supabase.name" -}}
{{- if .Values.secret.supabase.secretRef -}}
{{- .Values.secret.supabase.secretRef -}}
{{- else -}}
{{- include "supabase.secret.supabase" . -}}
{{- end -}}
{{- end -}}

{{/*
A secretKeyRef env entry for one key of the Supabase key Secret. Set
"optional" for keys that supabase-keygen may omit; the image default applies.
Usage: include "supabase.secret.env" (dict "root" . "name" "PGRST_JWT_SECRET" "key" "JWT_JWKS")
*/}}
{{- define "supabase.secret.env" -}}
- name: {{ .name }}
  valueFrom:
    secretKeyRef:
      name: {{ include "supabase.secret.supabase.name" .root }}
      key: {{ .key }}
      {{- if .optional }}
      optional: true
      {{- end }}
{{- end -}}

{{/*
Expand the name of the SMTP secret.
*/}}
{{- define "supabase.secret.smtp" -}}
{{- printf "%s-smtp" (include "supabase.fullname" .) }}
{{- end -}}

{{/*
Expand the name of the S3 secret.
*/}}
{{- define "supabase.secret.s3" -}}
{{- printf "%s-s3" (include "supabase.fullname" .) }}
{{- end -}}
