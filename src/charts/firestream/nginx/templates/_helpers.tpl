{{/*
Copyright Firestream. MIT License.
*/}}

{{/* vim: set filetype=mustache: */}}

{{/*
Return the proper nginx image name
*/}}
{{- define "nginx.image" -}}
{{ include "common.images.image" (dict "imageRoot" .Values.image "global" .Values.global) }}
{{- end -}}

{{/*
Return the proper Docker Image Registry Secret Names
*/}}
{{- define "nginx.imagePullSecrets" -}}
{{ include "common.images.pullSecrets" (dict "images" (list .Values.image) "global" .Values.global) }}
{{- end -}}

{{/*
Create the name of the service account to use
*/}}
{{- define "nginx.serviceAccountName" -}}
{{- if .Values.serviceAccount.create -}}
    {{ default (include "common.names.fullname" .) .Values.serviceAccount.name }}
{{- else -}}
    {{ default "default" .Values.serviceAccount.name }}
{{- end -}}
{{- end -}}

{{/*
Name of the ConfigMap holding the generated nginx.conf.
*/}}
{{- define "nginx.configMapName" -}}
{{- printf "%s-config" (include "common.names.fullname" .) | trunc 63 | trimSuffix "-" -}}
{{- end -}}

{{/*
Resolve whether a route is a websocket route.

Defaults to TRUE when the key is absent. `routes` exists to peel special
sub-paths off the default backend, and the motivating (and only) case is
websocket / longpolling traffic. A non-websocket route loses nothing from the
longer timeouts, whereas a websocket route silently given the 60s default
read timeout BREAKS Odoo longpolling, which holds connections ~55s. Failing in
the safe direction is worth the slightly surprising default; set
`websocket: false` to opt out.

Usage: {{ include "nginx.route.isWebsocket" $route }} -> "true" or "".
*/}}
{{- define "nginx.route.isWebsocket" -}}
{{- if hasKey . "websocket" -}}
{{- if .websocket }}true{{ end -}}
{{- else -}}
true
{{- end -}}
{{- end -}}

{{/*
Resolve a backend Service name to the name nginx should dial.

Input: dict "root" $ "service" <string>  ->  the name to use in proxy_pass.

With `nginxConfig.qualifyUpstreams` (default false) a DOTLESS name is expanded
to `<svc>.<release namespace>.svc.<clusterDomain>`; a name that already contains
a dot is assumed fully-qualified and passed through untouched.

The qualification happens HERE because this is the first point at which the
namespace is known. Values are frequently generated somewhere that cannot know
it -- a Nix-built bundle whose namespace the deploying layer chooses later --
and values strings are not `tpl`-rendered, so `{{ .Release.Namespace }}` cannot
be written into `upstreams` directly.
*/}}
{{- define "nginx.upstreamHost" -}}
{{- $root := .root -}}
{{- if and $root.Values.nginxConfig.qualifyUpstreams (not (contains "." .service)) -}}
{{ printf "%s.%s.svc.%s" .service (include "common.names.namespace" $root) $root.Values.clusterDomain }}
{{- else -}}
{{ .service }}
{{- end -}}
{{- end -}}

{{/*
Emit the `proxy_pass` line for one backend.

Input: dict "root" $ "service" <string> "port" <int>

TWO FORMS, selected by `nginxConfig.resolver`:

  * resolver EMPTY (default) -- the plain form:
        proxy_pass http://odoo:8069;
    nginx resolves the name ONCE, at config load. Simple and fast, but nginx
    REFUSES TO START ("host not found in upstream") if the Service does not
    exist yet. That is why nginx is ordered after its upstreams in
    firestreamStacks.dev.

  * resolver SET -- the deferred form:
        set $fs_upstream "odoo:8069";
        proxy_pass http://$fs_upstream$request_uri;
    A variable in proxy_pass defers resolution to request time, so the proxy
    starts (and stays up) even while a backend is absent, answering 502 until
    it returns. The trade-off is that a variable proxy_pass passes NO implicit
    URI, hence the explicit `$request_uri` -- which is the raw, undecoded
    original path plus query string, exactly what a transparent proxy wants.
*/}}
{{- define "nginx.proxyPass" -}}
{{- $root := .root -}}
{{- $host := include "nginx.upstreamHost" (dict "root" $root "service" .service) -}}
{{- if $root.Values.nginxConfig.resolver -}}
set $fs_upstream "{{ $host }}:{{ .port }}";
proxy_pass http://$fs_upstream$request_uri;
{{- else -}}
proxy_pass http://{{ $host }}:{{ .port }};
{{- end -}}
{{- end -}}

{{/*
The proxy directives shared by every generated `location` block.

Input: dict "root" $ "websocket" <bool>

X-Forwarded-Proto is taken from $firestream_forwarded_proto (see the map in
"nginx.config"), NOT from $scheme. TLS terminates at the namespace edge
(a Cloudflare tunnel) and the request reaches this proxy over plain HTTP, so
$scheme is always "http" here. Odoo runs with `proxy_mode = True` baked in
(src/containers/firestream/odoo/module.nix), meaning it TRUSTS this header:
sending it "http" would make every absolute URL Odoo generates - password
reset links, portal links, outbound mail - point at http://, and would defeat
its HTTPS detection entirely.

Upgrade/Connection are emitted on every location, not just websocket ones.
That is standard nginx practice and inert for ordinary requests: $http_upgrade
is empty, so the map below resolves $connection_upgrade to "close".
*/}}
{{- define "nginx.proxyDirectives" -}}
{{- $root := .root -}}
{{- $ws := .websocket -}}
proxy_http_version 1.1;
proxy_set_header Host $host;
proxy_set_header X-Real-IP $remote_addr;
proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
proxy_set_header X-Forwarded-Proto $firestream_forwarded_proto;
proxy_set_header X-Forwarded-Host $host;
proxy_set_header Upgrade $http_upgrade;
proxy_set_header Connection $connection_upgrade;
proxy_redirect off;
proxy_connect_timeout {{ $root.Values.proxy.connectTimeout }};
{{- if $ws }}
proxy_read_timeout {{ $root.Values.proxy.websocketReadTimeout }};
proxy_send_timeout {{ $root.Values.proxy.websocketSendTimeout }};
proxy_buffering off;
{{- else }}
proxy_read_timeout {{ $root.Values.proxy.readTimeout }};
proxy_send_timeout {{ $root.Values.proxy.sendTimeout }};
{{- end }}
{{- end -}}

{{/*
The complete generated nginx.conf.

Rendered unindented; the ConfigMap nindents it into place. Note that nginx
variables ($host, $http_upgrade, ...) are literal text to Go templates - only
`{{ }}` regions are interpolated - so they pass through verbatim.
*/}}
{{- define "nginx.config" -}}
# Generated by the Firestream nginx chart. DO NOT EDIT.
#
# Release:   {{ .Release.Name }}
# Namespace: {{ include "common.names.namespace" . }}

worker_processes {{ .Values.nginxConfig.workerProcesses }};
error_log /dev/stderr {{ .Values.nginxConfig.errorLogLevel }};
pid {{ .Values.nginxConfig.tmpDir }}/nginx.pid;

events {
  worker_connections {{ .Values.nginxConfig.workerConnections }};
}

http {
  # No mime.types include: a reverse proxy relays the upstream's Content-Type
  # verbatim, and the only locally generated responses set their own.
  default_type application/octet-stream;

  {{- if .Values.nginxConfig.accessLog }}
  access_log /dev/stdout;
  {{- else }}
  access_log off;
  {{- end }}

  # Every writable path lives under the emptyDir-backed scratch dir, because the
  # pod runs with readOnlyRootFilesystem: true.
  client_body_temp_path {{ .Values.nginxConfig.tmpDir }}/client_body;
  proxy_temp_path       {{ .Values.nginxConfig.tmpDir }}/proxy;
  fastcgi_temp_path     {{ .Values.nginxConfig.tmpDir }}/fastcgi;
  uwsgi_temp_path       {{ .Values.nginxConfig.tmpDir }}/uwsgi;
  scgi_temp_path        {{ .Values.nginxConfig.tmpDir }}/scgi;

  sendfile on;
  tcp_nopush on;
  keepalive_timeout {{ .Values.nginxConfig.keepaliveTimeout }};
  server_tokens {{ ternary "on" "off" .Values.nginxConfig.serverTokens }};

  # Generous body limit: Odoo attachment uploads routinely exceed nginx's 1m
  # default.
  client_max_body_size {{ .Values.proxy.clientMaxBodySize }};

  # Standard websocket upgrade map: "upgrade" for a real Upgrade request,
  # "close" for everything else.
  map $http_upgrade $connection_upgrade {
    default upgrade;
    ""      close;
  }

  # X-Forwarded-Proto passthrough. TLS terminates at the namespace edge, so the
  # request arrives here over plain HTTP and $scheme is always "http". Trust the
  # edge's header when it is present; fall back to $scheme only when it is not.
  map $http_x_forwarded_proto $firestream_forwarded_proto {
    default $http_x_forwarded_proto;
    ""      $scheme;
  }
  {{- with .Values.nginxConfig.resolver }}

  # Request-time DNS resolution (see the "nginx.proxyPass" helper): lets the
  # proxy start and stay up while a backend Service is absent or rolling.
  resolver {{ . }} valid={{ $.Values.nginxConfig.resolverValid }} ipv6=off;
  resolver_timeout {{ $.Values.nginxConfig.resolverTimeout }};
  {{- end }}

  {{- with .Values.nginxConfig.extraHttpConfig }}

  # -- extraHttpConfig --
  {{- include "common.tplvalues.render" (dict "value" . "context" $) | nindent 2 }}
  {{- end }}

  # Default server. Answers requests whose Host matches no upstream, and serves
  # the readiness endpoint on the pod IP (where kubelet probes arrive with no
  # meaningful Host header).
  server {
    listen {{ .Values.containerPorts.http }} default_server;
    server_name _;

    location {{ .Values.proxy.healthPath }} {
      access_log off;
      default_type text/plain;
      return 200 "ok\n";
    }

    location / {
      default_type text/plain;
      return 404 "no upstream configured for this host\n";
    }
  }
  {{- $root := . }}
  {{- range $name, $upstream := .Values.upstreams }}

  # ---------------------------------------------------------------- {{ $name }}
  server {
    listen {{ $root.Values.containerPorts.http }};
    server_name {{ $upstream.hostname }};

    location {{ $root.Values.proxy.healthPath }} {
      access_log off;
      default_type text/plain;
      return 200 "ok\n";
    }
    {{- with $root.Values.nginxConfig.extraServerConfig }}
    {{- include "common.tplvalues.render" (dict "value" . "context" $root) | nindent 4 }}
    {{- end }}
    {{- range $route := ($upstream.routes | default list) }}
    {{- $ws := include "nginx.route.isWebsocket" $route }}
    {{- range $path := $route.paths }}

    location {{ $path }} {
      {{- include "nginx.proxyPass" (dict "root" $root "service" $route.service "port" $route.port) | nindent 6 }}
      {{- include "nginx.proxyDirectives" (dict "root" $root "websocket" $ws) | nindent 6 }}
    }
    {{- end }}
    {{- end }}

    location / {
      {{- include "nginx.proxyPass" (dict "root" $root "service" $upstream.default.service "port" $upstream.default.port) | nindent 6 }}
      {{- include "nginx.proxyDirectives" (dict "root" $root "websocket" false) | nindent 6 }}
    }
  }
  {{- end }}
}
{{- end -}}
