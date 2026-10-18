# Cloudflared chart options: the tunnel surface -- THE HEADLINE OPTIONS.
#
# `existingSecret` / `tunnelTokenSecretKey` / `metrics` / `extraArgs` are
# top-level keys in values.yaml (the chart has a flat shape); they are grouped
# into this module because together they are the entire contract between the
# connector and the layer that provisions its tunnel.
#
# WHY A TOKEN AND NOTHING ELSE.
# A Cloudflare tunnel can be configured two ways: locally (a config.yaml plus a
# credentials JSON file, both mounted into the pod) or REMOTELY (ingress rules
# declared through the Cloudflare API, and the pod given only a token). This
# chart is remote-only, deliberately:
#
#   * the ingress rules are infrastructure state, and belong with the rest of
#     the Cloudflare resources in Pulumi (ZeroTrustTunnelCloudflaredConfig),
#     not duplicated into a Helm values file;
#   * a token is a single opaque string, so the chart's only secret handling is
#     a secretKeyRef -- it never sees, templates or stores the value.
#
# NO `originRequest` SURFACE, ON PURPOSE.
# The connector's per-origin behaviour (connectTimeout, keepAliveTimeout,
# noTLSVerify, httpHostHeader, ...) is part of the tunnel's ingress rules, so it
# is owned by whatever provisions the tunnel -- Pulumi's
# ZeroTrustTunnelCloudflaredConfig -- not by this chart. Adding options here
# would create a second place to configure it that Helm could not actually
# apply, since a remotely-managed connector reads its rules from the Cloudflare
# API and ignores anything local.
#
# One coupling is worth knowing when writing those rules: `httpHostHeader`
# rewrites the Host header on the way to the origin. If the origin is the
# firestream nginx chart, nginx routes purely on `server_name`, so any value
# other than the public hostname sends EVERY request to nginx's default server,
# which answers 404 -- with tunnel, proxy and app all healthy. Leave
# `originRequest` at its defaults unless you have a specific reason.
#
# STABILITY CONTRACT: `existingSecret` is a PLAIN SCALAR at the values path
# `existingSecret`, and `image.tag` at `image.tag`. A later Pulumi phase writes
# code against exactly those paths (plus `replicaCount`), so keep them simple:
# do not nest them, do not compute them from siblings.
{ lib, ... }:

let
  inherit (lib) mkOption types;
in
{
  options.cloudflared = {
    existingSecret = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        Name of an EXISTING Kubernetes Secret holding the tunnel token.

        The Secret is created out of band by the deploying layer (Pulumi),
        alongside the Cloudflare tunnel whose token it carries. This chart
        never creates it and never templates its value.

        Null/empty falls back to the convention `<fullname>-token` (for the
        default release name: `cloudflared-token`). The secretKeyRef is not
        marked optional, so a missing Secret blocks the pod in
        CreateContainerConfigError rather than yielding a tokenless connector.
      '';
      example = "centerpoint-main-tunnel-token";
    };

    tunnelTokenSecretKey = mkOption {
      type = types.nullOr types.str;
      default = null;
      description = ''
        Key within `existingSecret` holding the token (chart default
        `tunnel-token`). Override when the Secret was created by something with
        its own key convention.
      '';
      example = "tunnel-token";
    };

    metrics = mkOption {
      default = null;
      description = ''
        cloudflared's diagnostic HTTP server (`--metrics 0.0.0.0:<port>`),
        which serves /ready and /metrics.
      '';
      type = types.nullOr (types.submodule {
        freeformType = types.attrsOf types.anything;

        options = {
          enabled = mkOption {
            type = types.nullOr types.bool;
            default = null;
            description = ''
              Run the diagnostic server (chart default true).

              THIS ALSO CONTROLS THE PROBES. /ready is the connector's own
              answer to "am I registered with the Cloudflare edge?" -- 200 with
              at least one live connection, 503 otherwise -- and is the only
              health signal a connector exposes. With `enabled = false` there
              is no endpoint to probe, so the chart omits all three probes and
              a wedged connector will never be restarted.
            '';
          };
        };
      });
    };

    extraArgs = mkOption {
      type = types.nullOr (types.listOf types.str);
      default = null;
      description = ''
        Extra flags spliced into the connector's args, between `--metrics` and
        the trailing `run`. Escape hatch for `--protocol quic`,
        `--edge-ip-version`, `--loglevel debug`, and similar.

        Do NOT put `run` or a tunnel name here: `run` is appended by the chart
        and, with a token in TUNNEL_TOKEN, takes no operand.
      '';
      example = [ "--protocol" "quic" ];
    };
  };
}
