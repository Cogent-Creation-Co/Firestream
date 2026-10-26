# Supabase chart options: `secret.*`.
#
# `secret.supabase` is one Secret in the layout of
# `supabase-keygen --output k8s-secret`: set `secretRef` to an existing
# Secret, or `values` to let the chart render it. `smtp` and `s3` keep the
# upstream secretRef/secretRefKey shape.
{ lib, ... }:
let
  inherit (lib) mkOption types;
  opt = type: description: mkOption { type = types.nullOr type; default = null; inherit description; };
  refOptions = {
    secretRef = opt types.str "Name of an existing Secret; the chart then renders none.";
    secretRefKey = opt (types.attrsOf types.str) "Key names inside the existing Secret.";
  };
  sub = options: description: mkOption {
    default = null;
    inherit description;
    type = types.nullOr (types.submodule { inherit options; });
  };
in
{
  options.supabase.secret = sub {
    supabase = sub {
      secretRef = opt types.str "Name of an existing Secret in the supabase-keygen layout.";
      values = opt (types.attrsOf types.str) "Key set the chart renders when secretRef is empty.";
    } "The Supabase key set (JWT secret, API keys, database password, ...).";
    smtp = sub (refOptions // {
      username = opt types.str "SMTP username for Auth.";
      password = opt types.str "SMTP password for Auth.";
    }) "SMTP credentials for Auth.";
    s3 = sub (refOptions // {
      keyId = opt types.str "S3 access key id of the Storage backend.";
      accessKey = opt types.str "S3 secret access key of the Storage backend.";
    }) "S3 credentials of the Storage backend.";
  } "Credentials.";
}
