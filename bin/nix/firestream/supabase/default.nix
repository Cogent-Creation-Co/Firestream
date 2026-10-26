# Supabase upstream pins and the files derived from them
# Copyright Firestream. MIT License.
#
# One place for the upstream Supabase sources that the Supabase images read
# config, SQL and code from. Every pin belongs to the self-hosted docker
# release named in `release`; an upstream sync bumps them together, and
# `checks.<sys>.firestream-supabase-postgres-migrations` proves the result.

{ pkgs, lib }:

let
  sources = {
    # NOTE: the tag that the v0.8.3 compose file runs
    # (`supabase/postgres:17.6.1.136`): migrations, supautils.conf and the
    # extension custom scripts.
    postgres = pkgs.fetchFromGitHub {
      owner = "supabase";
      repo = "postgres";
      rev = "d156ba65c14694c12cc5e782bc15b9b8ed2d1376";
      hash = "sha256-MxuokBjgWTWMnz/IV9NIkoqkkLjXkRgWTMcObat0LIw=";
    };

    # NOTE: a sparse checkout, because the full supabase/supabase monorepo is
    # several GB and the database needs only the compose volume files.
    dockerVolumes = pkgs.fetchFromGitHub {
      owner = "supabase";
      repo = "supabase";
      rev = "bc27d2661edd09a5d0b8d76ce33da6d4071a756e";
      sparseCheckout = [ "docker/volumes" ];
      hash = "sha256-UcaVIvsZtJhm4uIPVrx8d8ogSQ9XYY2MyLjyOaDYmyY=";
    };
  };

  postgresFiles = "${sources.postgres}/ansible/files";
  volumesDb = "${sources.dockerVolumes}/docker/volumes/db";

  keygenPython = pkgs.python3.withPackages (ps: [ ps.cryptography ]);
in
{
  release = "self-hosted/v0.8.3";

  inherit sources;

  # WARNING: these values are published in this repository. Anyone can sign a
  # service_role token or decrypt Vault and Realtime data with them. Use them
  # only for local stacks and tests; a deployment takes a set from
  # `nix run .#supabase-keygen` instead.
  devSecrets = builtins.fromJSON (builtins.readFile ./dev-secrets.json);

  # Prints a fresh, consistent key set (env, json or a Kubernetes Secret).
  # Usage: supabase-keygen --output k8s-secret --name supabase-secrets
  keygen = pkgs.writeShellApplication {
    name = "supabase-keygen";
    text = ''exec ${keygenPython}/bin/python ${./keygen.py} "$@"'';
  };

  postgres = {
    supautilsConf = pkgs.runCommand "supautils.conf" { } ''
      sed -e "s|/etc/postgresql-custom/extension-custom-scripts|${postgresFiles}/postgresql_extension_custom_scripts|" \
        ${postgresFiles}/postgresql_config/supautils.conf.j2 > $out
    '';

    # The tree the upstream image bakes at /docker-entrypoint-initdb.d, plus
    # the compose file mounts of the same release.
    # NOTE: logs.sql (the `_analytics` schema) stays out, because Logflare is
    # out of scope.
    migrations = pkgs.runCommand "supabase-postgres-migrations" { } ''
      cp -r ${sources.postgres}/migrations/db $out
      chmod -R u+w $out
      cp ${postgresFiles}/pgbouncer_config/pgbouncer_auth_schema.sql $out/init-scripts/00-schema.sql
      cp ${postgresFiles}/stat_extension.sql $out/migrations/00-extension.sql
      cp ${volumesDb}/webhooks.sql $out/init-scripts/98-webhooks.sql
      cp ${volumesDb}/roles.sql $out/init-scripts/99-roles.sql
      cp ${volumesDb}/jwt.sql $out/init-scripts/99-jwt.sql
      cp ${volumesDb}/_supabase.sql $out/migrations/97-_supabase.sql
      cp ${volumesDb}/realtime.sql $out/migrations/99-realtime.sql
      cp ${volumesDb}/pooler.sql $out/migrations/99-pooler.sql
    '';
  };
}
