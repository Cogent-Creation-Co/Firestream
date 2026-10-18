{{/*
Copyright Firestream. MIT License.
*/}}

{{/* vim: set filetype=mustache: */}}

{{/*
Return the proper cloudflared image name.

Unlike every other Firestream chart this resolves to an UPSTREAM image
(cloudflare/cloudflared). There is no Nix-built container to substitute; see
the header of Chart.yaml.
*/}}
{{- define "cloudflared.image" -}}
{{ include "common.images.image" (dict "imageRoot" .Values.image "global" .Values.global) }}
{{- end -}}

{{/*
Return the proper Docker Image Registry Secret Names
*/}}
{{- define "cloudflared.imagePullSecrets" -}}
{{ include "common.images.pullSecrets" (dict "images" (list .Values.image) "global" .Values.global) }}
{{- end -}}

{{/*
Create the name of the service account to use
*/}}
{{- define "cloudflared.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
    {{ default (include "common.names.fullname" .) .Values.serviceAccount.name }}
{{- else -}}
    {{ default "default" .Values.serviceAccount.name }}
{{- end -}}
{{- end -}}

{{/*
Name of the Secret holding the tunnel token.

The Secret is created OUT OF BAND (by Pulumi, alongside the Cloudflare tunnel
itself); this chart only references it. `existingSecret` empty falls back to the
convention `<fullname>-token`, which is what a deploying layer should create
when it has no reason to pick a different name.
*/}}
{{- define "cloudflared.tunnelSecretName" -}}
{{- if .Values.existingSecret -}}
{{- include "common.tplvalues.render" (dict "value" .Values.existingSecret "context" $) -}}
{{- else -}}
{{- printf "%s-token" (include "common.names.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}
{{- end -}}

{{/*
The container's args.

The upstream image's ENTRYPOINT is already `cloudflared --no-autoupdate`, so
these are the subcommand and its flags:

    tunnel --no-autoupdate [--metrics 0.0.0.0:<port>] [extraArgs...] run

`--no-autoupdate` is repeated on the subcommand on purpose - it is accepted by
`cloudflared tunnel` and keeps the args self-describing regardless of what
ENTRYPOINT the image ships. `run` MUST come last: with a token in
TUNNEL_TOKEN it takes no tunnel-name operand, and any flag placed after it
would be parsed as one.
*/}}
{{- define "cloudflared.args" -}}
- tunnel
- --no-autoupdate
{{- if .Values.metrics.enabled }}
- --metrics
- {{ printf "0.0.0.0:%v" .Values.containerPorts.metrics | quote }}
{{- end }}
{{- with .Values.extraArgs }}
{{- include "common.tplvalues.render" (dict "value" . "context" $) }}
{{- end }}
- run
{{- end -}}
