# imgproxy Container Module
# Copyright Firestream. MIT License.
#
# imgproxy is a single Go binary linked against libvips. It reads its
# configuration from IMGPROXY_* environment variables and keeps no state.

{ pkgs
, lib
, firestream

, version ? "3.31.4"

, paths ? {
    base = "/opt/firestream/imgproxy";
    conf = "/opt/firestream/imgproxy/config";
    data = "/firestream/imgproxy/data";
    logs = "/opt/firestream/imgproxy/logs";
  }

, envVars ? { }
, envVarsWithSecrets ? [ ]
, exposedPorts ? [ 5001 ]
, imageName ? "firestream-supabase-imgproxy"
, imageTag ? version
, health ? { enable = false; port = 9180; readinessCmd = null; }
}:

firestream.mkContainerModule {
  name = "imgproxy";
  inherit version paths envVars envVarsWithSecrets;
  inherit imageName imageTag exposedPorts health;

  runCmd = "imgproxy";
  enableStateTracking = false;

  systemDeps = with pkgs; [ cacert coreutils ];
  runtimeBinDeps = [ firestream.packages.imgproxy ] ++ (with pkgs; [ coreutils bash ]);

  user = { name = "imgproxy"; group = "imgproxy"; uid = 1001; gid = 1001; };
}
