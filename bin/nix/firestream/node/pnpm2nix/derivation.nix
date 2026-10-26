# Vendored from github:FliegendeWurst/pnpm2nix-nzbr at
# 9b15300b711a50bb96e553b66aad5cad94df9c7e (ISC, see ./LICENSE).

{ lib
, stdenv
, nodejs
, jq
, moreutils
, pkg-config
, callPackage
, writeText
, runCommand
, ...
}:

with builtins; with lib; with callPackage ./lockfile.nix { };
let
  nodePkg = nodejs;
  pkgConfigPkg = pkg-config;
in
{
  mkPnpmPackage =
    { workspace ? null
    , components ? []
    , src ? if (workspace != null && components != []) then workspace else null
    , packageJSON ? src + "/package.json"
    , componentPackageJSONs ? map (c: {
        name = "${c}/package.json";
        value = src + "/${c}/package.json";
      }) components
    # NOTE: pnpmLockJson, pname and version take no default from `src`. When
    # `src` is fetched, a read of its files at eval time is
    # import-from-derivation.
    , pnpmLockJson
    , pnpmWorkspaceYaml ? (if workspace == null then null else workspace + "/pnpm-workspace.yaml")
    , pname
    , version ? null
    , name ? if version != null then "${pname}-${version}" else pname
    , registry ? "https://registry.npmjs.org"
    , script ? "build"
    , scriptFull ? null
    , distDir ? "dist"
    , distDirs ? (if workspace == null then [distDir] else (map (c: "${c}/dist") components))
    , distDirIsOut ? true
    , installNodeModules ? false
    , installPackageFiles ? false
    , installEnv ? { }
    , buildEnv ? { }
    , noDevDependencies ? false
    # Platform filtering for optional dependencies (auto-detected from stdenv by default)
    # Set to null to disable filtering, or override with specific platform strings
    , targetOs ? (
        if stdenv.hostPlatform.isLinux then "linux"
        else if stdenv.hostPlatform.isDarwin then "darwin"
        else if stdenv.hostPlatform.isWindows then "win32"
        else if stdenv.hostPlatform.isFreeBSD then "freebsd"
        else null
      )
    , targetCpu ? (
        if stdenv.hostPlatform.isx86_64 then "x64"
        else if stdenv.hostPlatform.isAarch64 then "arm64"
        else if stdenv.hostPlatform.isx86 then "ia32"
        else if stdenv.hostPlatform.isAarch32 then "arm"
        else null
      )
    , targetLibc ? (
        if stdenv.hostPlatform.isLinux then (if stdenv.hostPlatform.isMusl then "musl" else "glibc")
        else null
      )
    # NOTE: package names, without a version, that the platform filter drops
    # too, so that a prebuilt native package that the caller builds from
    # source never reaches the pnpm store.
    , excludePackages ? [ ]
    , extraNodeModuleSources ? [ ]
    , copyNodeModules ? false
    , extraNativeBuildInputs ? [ ]
    , extraBuildInputs ? [ ]
    , nodejs ? nodePkg
    , pnpm ? nodejs.pkgs.pnpm
    , pkg-config ? pkgConfigPkg
    , preBuild ? ""
    , ...
    }@attrs:
    let
      # Flag that can be computed from arguments, indicating a workspace was
      # supplied. Only used in these let bindings.
      isWorkspace = workspace != null && components != [];
      # Utility functions
      forEachConcat = f: xs: concatStringsSep "\n" (map f xs);
      forEachComponent = f: forEachConcat f components;
      # Computed values used below that don't loop
      nativeBuildInputs = [
        nodejs
        pnpm
        pkg-config
      ] ++ extraNativeBuildInputs;
      buildInputs = extraBuildInputs;
      packageFilesWithoutLockfile =
        [
          { name = "package.json"; value = packageJSON; }
        ] ++ componentPackageJSONs ++ computedNodeModuleSources;
      computedNodeModuleSources =
        (if pnpmWorkspaceYaml == null
          then []
          else [
            {name = "pnpm-workspace.yaml"; value = pnpmWorkspaceYaml;}
          ]
        ) ++ extraNodeModuleSources;
      # Computed values that loop over something
      computedDistFiles =
        let
          packageFileNames = ["pnpm-lock.yaml"] ++
            map ({ name, ... }: name) packageFilesWithoutLockfile;
        in
          distDirs ++
            optionals installNodeModules nodeModulesDirs ++
            optionals installPackageFiles packageFileNames;
      nodeModulesDirs =
        if isWorkspace then
          ["node_modules"] ++ (map (c: "${c}/node_modules") components)
        else ["node_modules"];
      filterString = concatStringsSep " " (
        ["--recursive" "--stream"] ++
        map (c: "--filter ./${c}") components
      ) + " ";
      buildScripts = if scriptFull != null then scriptFull else ''
        pnpm run ${optionalString isWorkspace filterString}${script}
      '';
      # Flag derived from value computed above, indicating the single dist
      # should be copied as $out directly, rather than $out/${distDir}
      computedDistDirIsOut =
        length computedDistFiles == 1 && distDirIsOut && !isWorkspace;
    in
    stdenv.mkDerivation (
      recursiveUpdate
        (rec {
          inherit src name nativeBuildInputs buildInputs preBuild;

          strictDeps = true;

          # avoid ERR_PNPM_RESOLUTION_SHAPE_MISMATCH errors
          env.PNPM_CONFIG_TRUST_LOCKFILE = "true";

          postUnpack = ''
            ${optionalString (pnpmWorkspaceYaml != null) ''
              cp -v ${pnpmWorkspaceYaml} pnpm-workspace.yaml
            ''}
            ${forEachComponent (component:
              ''mkdir -p "${component}"'')
            }
          '';

          configurePhase = ''
            export HOME=$NIX_BUILD_TOP # Some packages need a writable HOME
            export npm_config_nodedir=${nodejs}

            runHook preConfigure

            store=$(pnpm store path)
            mkdir -p $(dirname $store)

            cp -fv ${passthru.patchedLockfileYaml} pnpm-lock.yaml

            pnpm store add --silent $(cat ${passthru.processResultAllDeps})

            ${concatStringsSep "\n" (
              mapAttrsToList
                (n: v: ''export ${n}="${v}"'')
                installEnv
            )}

            pnpm install ${optionalString noDevDependencies "--prod"} \
              --ignore-scripts \
              --force \
              --frozen-lockfile

            runHook postConfigure
          '';

          buildPhase = ''
            ${concatStringsSep "\n" (
              mapAttrsToList
                (n: v: ''export ${n}="${v}"'')
                buildEnv
            )}

            runHook preBuild

            ${buildScripts}

            runHook postBuild
          '';

          installPhase = ''
            runHook preInstall

            ${if computedDistDirIsOut then ''
                ${if distDir == "." then "cp -r" else "mv"} ${distDir} $out
              ''
              else ''
                mkdir -p $out
                ${forEachConcat (dDir: ''
                    cp -r --parents ${dDir} $out
                  '') computedDistFiles
                }
              ''
            }

            runHook postInstall
          '';

          passthru =
            let
              processResult = processLockfile { inherit registry noDevDependencies targetOs targetCpu targetLibc excludePackages; lockfile = pnpmLockJson; };
            in
            {
              inherit attrs;

              patchedLockfileYaml = writeText "pnpm-lock.yaml" (toJSON processResult.patchedLockfile);

              processResultAllDeps = runCommand "${name}-dependency-list" {} ''
                echo ${concatStringsSep " " (unique processResult.dependencyTarballs)} > $out
              '';
            };
        })
        (removeAttrs attrs [ "excludePackages" ] // { extraNodeModuleSources = null; installEnv = null; buildEnv = null;})
    );
}
