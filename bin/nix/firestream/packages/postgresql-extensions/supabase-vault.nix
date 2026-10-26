# supabase_vault: encrypted secret storage for Postgres (Supabase Vault).
# Copyright Firestream. MIT License.
#
# Not in nixpkgs. Without it, migration 20221207154255 falls back to pgsodium
# and creates no `vault` schema, so Studio's Vault page and `vault.*` fail.

{
  fetchFromGitHub,
  lib,
  libsodium,
  postgresql,
  postgresqlBuildExtension,
}:

postgresqlBuildExtension (finalAttrs: {
  pname = "supabase_vault";
  version = "0.3.1";

  src = fetchFromGitHub {
    owner = "supabase";
    repo = "vault";
    rev = "6e0cd916242d922a646e4d611cc215e09dd429f4";
    hash = "sha256-MC87bqgtynnDhmNZAu96jvfCpsGDCPB0g5TZfRQHd30=";
  };

  buildInputs = [ libsodium ];

  enableUpdateScript = false;

  meta = {
    description = "Postgres extension for storing encrypted secrets";
    homepage = "https://github.com/supabase/vault";
    license = lib.licenses.asl20;
    platforms = postgresql.meta.platforms;
  };
})
