# supautils: Supabase's role and extension guard for Postgres.
# Copyright Firestream. MIT License.
#
# Not in nixpkgs. The Supabase migrations set
# `session_preload_libraries = 'supautils'` for `authenticator`, so without
# this library every PostgREST login fails.

{
  fetchFromGitHub,
  lib,
  postgresql,
  postgresqlBuildExtension,
}:

postgresqlBuildExtension (finalAttrs: {
  pname = "supautils";
  version = "3.2.2";

  src = fetchFromGitHub {
    owner = "supabase";
    repo = "supautils";
    rev = "64792e14681bba81c9adccdcfd598715cd052eb5";
    hash = "sha256-Oi35fel2Yp58eHsWVOXtxAo/s0RAbUxJEBxPrYRK+cs=";
  };

  enableUpdateScript = false;

  meta = {
    description = "Guards superuser-only actions for non-superuser roles in Supabase";
    homepage = "https://github.com/supabase/supautils";
    license = lib.licenses.asl20;
    platforms = postgresql.meta.platforms;
  };
})
