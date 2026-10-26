# plan_filter: rejects statements whose planned cost exceeds a limit.
# Copyright Firestream. MIT License.
#
# Not in nixpkgs. Supabase lists it in shared_preload_libraries and in
# supautils.privileged_role_allowed_configs (`plan_filter.*`).

{
  fetchFromGitHub,
  lib,
  postgresql,
  postgresqlBuildExtension,
}:

postgresqlBuildExtension {
  pname = "plan_filter";
  version = "0.1-unstable-2021-09-23";

  # NOTE: the Supabase recipe names `pgexperts/plan_filter`, which now returns
  # 404. The repository moved to `pg_plan_filter`; the commit is the same.
  src = fetchFromGitHub {
    owner = "pgexperts";
    repo = "pg_plan_filter";
    rev = "5081a7b5cb890876e67d8e7486b6a64c38c9a492";
    hash = "sha256-YNeIfmccT/DtOrwDmpYFCuV2/P6k3Zj23VWBDkOh6sw=";
  };

  enableUpdateScript = false;

  meta = {
    description = "Postgres module that refuses statements above a planner cost limit";
    homepage = "https://github.com/pgexperts/pg_plan_filter";
    license = lib.licenses.postgresql;
    platforms = postgresql.meta.platforms;
  };
}
