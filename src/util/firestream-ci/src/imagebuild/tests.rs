//! Plan-level parity tests. Nothing here spawns Nix or Docker: the whole point
//! of the pure [`super::plan`] is that the argv can be asserted for free, which
//! is the only honest way to check a build path whose real invocation costs
//! hours.

use super::*;

fn spec(dest: &Path, output: Output) -> BuildSpec {
    BuildSpec {
        flake_dir: PathBuf::from("/repo"),
        flake_ref: ".#redis-7".into(),
        dest: dest.to_path_buf(),
        target_arch: "x86_64".into(),
        output,
        docker_sock: true,
    }
}

fn docker_env() -> PlanEnv {
    PlanEnv {
        strategy: BuildStrategy::Docker,
        resources: DockerResources {
            cpus: Some("8".into()),
            memory: Some("23g".into()),
            swap: Some("46g".into()),
        },
        docker_cache_volume_template: "firestream-nix-store-{arch}".into(),
        builder_tag: String::new(),
    }
}

#[test]
fn native_plan_is_the_fs_nix_build_native_argv() {
    let p = plan(
        &spec(Path::new("/repo/_build/redis-7/redis-7.tar.gz"), Output::Tarball),
        &PlanEnv {
            strategy: BuildStrategy::Native,
            ..PlanEnv::default()
        },
    )
    .unwrap();
    let Plan::Native(n) = p else { panic!("expected native") };
    assert_eq!(
        n.argv,
        vec![
            "nix",
            "build",
            ".#redis-7",
            "--out-link",
            "/repo/_build/redis-7/redis-7.tar.gz",
            "-L",
            "--no-update-lock-file",
            "--extra-experimental-features",
            "nix-command flakes",
        ]
    );
    assert_eq!(n.cwd, PathBuf::from("/repo"));
}

/// `--dir` / `--sock` are accepted for signature parity and change NOTHING on
/// the native path — same as fs_nix_build_native's flag-swallowing loop.
#[test]
fn native_plan_ignores_dir_and_sock() {
    let a = plan(
        &spec(Path::new("/repo/_build/x/x.tar.gz"), Output::Tarball),
        &PlanEnv::default(),
    )
    .unwrap();
    let mut s = spec(Path::new("/repo/_build/x/x.tar.gz"), Output::Dir);
    s.docker_sock = false;
    let b = plan(&s, &PlanEnv::default()).unwrap();
    assert_eq!(a, b);
}

#[test]
fn docker_plan_mounts_out_volume_platform_and_sock() {
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("redis-7");
    std::fs::create_dir_all(&out).unwrap();
    let dest = out.join("redis-7.tar.gz");

    let p = plan(&spec(&dest, Output::Tarball), &docker_env()).unwrap();
    let Plan::Docker(d) = p else { panic!("expected docker") };

    assert_eq!(d.docker_platform, "linux/amd64");
    // THE volume the shell has been filling. Not -x86_64.
    assert_eq!(d.nix_volume, "firestream-nix-store-amd64");
    assert_eq!(d.builder_tag, "nixos/nix:latest");

    let joined = d.argv.join(" ");
    assert!(joined.contains("--rm"));
    assert!(joined.contains("--platform linux/amd64"));
    assert!(joined.contains(&format!("-v {}:/out", out.canonicalize().unwrap().display())));
    assert!(joined.contains("--mount type=volume,source=firestream-nix-store-amd64,target=/nix"));
    assert!(joined.contains("--cpus 8"));
    assert!(joined.contains("--memory 23g"));
    assert!(joined.contains("--memory-swap 46g"));
    assert!(joined.contains("-v /var/run/docker.sock:/var/run/docker.sock"));
    assert_eq!(d.argv[d.argv.len() - 3], "sh");
    assert_eq!(d.argv[d.argv.len() - 2], "-c");
}

#[test]
fn docker_plan_without_sock_omits_the_socket_mount() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("manifest");
    let mut s = spec(&dest, Output::Dir);
    s.docker_sock = false;
    let Plan::Docker(d) = plan(&s, &docker_env()).unwrap() else {
        panic!()
    };
    assert!(!d.argv.join(" ").contains("docker.sock"));
}

/// A `/nix/store` flake dir (external consumer, no checkout) is mounted at
/// `/flake` and the `.#attr` ref is rewritten to `/flake#attr`.
#[test]
fn store_path_flake_dir_is_rewritten_to_slash_flake() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out.tar.gz");
    let mut s = spec(&dest, Output::Tarball);
    s.flake_dir = PathBuf::from("/nix/store/abc123-source");
    let Plan::Docker(d) = plan(&s, &docker_env()).unwrap() else {
        panic!()
    };
    assert_eq!(d.workdir, "/flake");
    assert_eq!(d.container_ref, "/flake#redis-7");
    assert!(d.argv.join(" ").contains("-v /nix/store/abc123-source:/flake:ro"));
    assert!(!d.worktree_detected);
}

/// An empty template falls back to the shell's hard-coded volume name, so a
/// profile-less invocation still hits the warm cache.
#[test]
fn empty_volume_template_falls_back_to_get_nix_volume() {
    let tmp = tempfile::tempdir().unwrap();
    let dest = tmp.path().join("out.tar.gz");
    let mut env = docker_env();
    env.docker_cache_volume_template = String::new();
    let Plan::Docker(d) = plan(&spec(&dest, Output::Tarball), &env).unwrap() else {
        panic!()
    };
    assert_eq!(d.nix_volume, "firestream-nix-store-amd64");
}

#[test]
fn inner_script_tarball_branch_derefs_and_size_checks() {
    let s = inner_script("/flake#x", "x.tar.gz", Output::Tarball);
    assert!(s.contains("nix build \"/flake#x\" -o /tmp/result -L --no-update-lock-file"));
    assert!(s.contains("cp -L /tmp/result \"/out/x.tar.gz\""));
    assert!(s.contains("if [ ! -s \"/out/x.tar.gz\" ]"));
    assert!(s.contains("experimental-features = nix-command flakes"));
    assert!(s.contains("git config --global --add safe.directory"));
}

#[test]
fn inner_script_dir_branch_copies_via_writable_temp() {
    let s = inner_script(".#manifest", "manifest", Output::Dir);
    assert!(s.contains("rm -rf \"/out/manifest\""));
    assert!(s.contains("cp -rL /tmp/result /tmp/result-writable"));
    assert!(s.contains("chmod -R u+w /tmp/result-writable"));
    assert!(s.contains("cp -r /tmp/result-writable/* \"/out/manifest/\""));
    assert!(!s.contains("if [ ! -s"));
}

// ── docker load tag extraction ────────────────────────────────────────────

#[test]
fn loaded_tag_takes_the_last_match_first_field() {
    let out = "Loaded image: firestream-redis:7-nix\n";
    assert_eq!(parse_loaded_tag(out), "firestream-redis:7-nix");

    // `tail -1`: last wins.
    let multi = "Loaded image: a:1\nLoaded image: b:2\n";
    assert_eq!(parse_loaded_tag(multi), "b:2");

    // `awk '{print $1}'`: first whitespace-delimited field only.
    assert_eq!(parse_loaded_tag("Loaded image: a:1 extra junk\n"), "a:1");

    // No match ⇒ the shell's literal placeholder.
    assert_eq!(parse_loaded_tag("The image has been loaded\n"), "(unknown)");
}

// ── git mounts ────────────────────────────────────────────────────────────

#[test]
fn plain_repo_has_no_extra_git_mounts() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::create_dir(tmp.path().join(".git")).unwrap();
    assert!(shell_git_mounts(tmp.path()).is_empty());
}

#[test]
fn worktree_mounts_gitdir_and_commondir() {
    let tmp = tempfile::tempdir().unwrap();
    let main_git = tmp.path().join("main/.git");
    let wt_gitdir = main_git.join("worktrees/feature");
    std::fs::create_dir_all(&wt_gitdir).unwrap();
    std::fs::write(wt_gitdir.join("commondir"), "../..\n").unwrap();

    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(
        wt.join(".git"),
        format!("gitdir: {}\n", wt_gitdir.display()),
    )
    .unwrap();

    let mounts = shell_git_mounts(&wt);
    assert_eq!(mounts.len(), 2);
    assert_eq!(mounts[0], wt_gitdir.canonicalize().unwrap());
    assert_eq!(mounts[1], main_git.canonicalize().unwrap());
}

#[test]
fn worktree_without_commondir_falls_back_two_levels_up() {
    let tmp = tempfile::tempdir().unwrap();
    let main_git = tmp.path().join("main/.git");
    let wt_gitdir = main_git.join("worktrees/feature");
    std::fs::create_dir_all(&wt_gitdir).unwrap();

    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(&wt).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}", wt_gitdir.display())).unwrap();

    let mounts = shell_git_mounts(&wt);
    assert_eq!(mounts[1], main_git.canonicalize().unwrap());
}

/// The worktree mounts must show up in the docker argv, at their ORIGINAL
/// paths, read-only — that is the whole reason `resolve_git_mounts` exists.
#[test]
fn worktree_mounts_reach_the_docker_argv() {
    let tmp = tempfile::tempdir().unwrap();
    let main_git = tmp.path().join("main/.git");
    let wt_gitdir = main_git.join("worktrees/feature");
    std::fs::create_dir_all(&wt_gitdir).unwrap();
    std::fs::write(wt_gitdir.join("commondir"), "../..").unwrap();

    let wt = tmp.path().join("wt");
    std::fs::create_dir_all(wt.join("_build/redis-7")).unwrap();
    std::fs::write(wt.join(".git"), format!("gitdir: {}", wt_gitdir.display())).unwrap();

    let mut s = spec(&wt.join("_build/redis-7/redis-7.tar.gz"), Output::Tarball);
    s.flake_dir = wt.clone();
    let Plan::Docker(d) = plan(&s, &docker_env()).unwrap() else {
        panic!()
    };
    assert!(d.worktree_detected);
    let joined = d.argv.join(" ");
    let g = wt_gitdir.canonicalize().unwrap();
    assert!(joined.contains(&format!("-v {0}:{0}:ro", g.display())), "{joined}");
    // The working tree itself is mounted at its physical path.
    let phys = wt.canonicalize().unwrap();
    assert!(joined.contains(&format!("-v {0}:{0}:ro", phys.display())));
    assert_eq!(d.workdir, phys.display().to_string());
}

// ── layout + lock + summary ───────────────────────────────────────────────

#[test]
fn build_output_layout_matches_the_scripts() {
    let root = Path::new("/repo/_build");
    assert_eq!(
        image_dest(root, "redis-7"),
        PathBuf::from("/repo/_build/redis-7/redis-7.tar.gz")
    );
    let (d, r) = manifest_dest(root, None);
    assert_eq!(d, PathBuf::from("/repo/_build/manifest"));
    assert_eq!(r, ".#manifest");
    let (d, r) = manifest_dest(root, Some("airflow"));
    assert_eq!(d, PathBuf::from("/repo/_build/sbom-airflow"));
    assert_eq!(r, ".#sbom.airflow");
}

#[test]
fn batch_lock_is_exclusive_and_released_on_drop() {
    let tmp = tempfile::tempdir().unwrap();
    let lock = BatchLock::acquire(tmp.path()).unwrap();
    assert!(tmp.path().join(BatchLock::NAME).is_dir());
    // Same path as the bash, so the two implementations exclude each other.
    assert_eq!(BatchLock::NAME, ".build-batch.lock");

    match BatchLock::acquire(tmp.path()) {
        Err(BuildError::LockHeld { pid, .. }) => {
            assert_eq!(pid, std::process::id().to_string());
        }
        other => panic!("expected LockHeld, got {other:?}"),
    }

    drop(lock);
    assert!(!tmp.path().join(BatchLock::NAME).exists());
    // And it can be re-taken.
    let _again = BatchLock::acquire(tmp.path()).unwrap();
}

#[test]
fn stale_lock_is_reclaimed() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join(BatchLock::NAME);
    std::fs::create_dir(&path).unwrap();
    // PID 2^22 is above every default pid_max; nothing is running there.
    std::fs::write(path.join("pid"), "4194304").unwrap();
    let _lock = BatchLock::acquire(tmp.path()).expect("stale lock should be reclaimed");
}

#[test]
fn summary_box_has_the_shell_layout() {
    let results = vec![
        PackageResult {
            package: "redis-7".into(),
            container: "redis".into(),
            succeeded: true,
            image_tag: Some("firestream-redis:7-nix".into()),
            note: None,
        },
        PackageResult {
            package: "kafka-4".into(),
            container: "kafka".into(),
            succeeded: false,
            image_tag: None,
            note: Some("boom".into()),
        },
    ];
    let s = render_summary(&results, 42);
    assert!(s.contains("BUILD SUMMARY"));
    assert!(s.contains("Succeeded   1"));
    assert!(s.contains("Failed      1"));
    assert!(s.contains("Duration    42s"));
    // Bug-for-bug: the shell's own box does not line up. Borders and the title
    // row are 54 columns (`  ║  %-48s║`), the three data rows are 56
    // (`  ║  Succeeded   %-38s║` — 5 + 9 + 3 + 38 + 1). Asserted rather than
    // "fixed" because the point of this phase is parity, and a summary that
    // silently changed shape would be one more thing to re-verify later.
    let widths: Vec<usize> = s.lines().map(|l| l.chars().count()).collect();
    assert_eq!(widths, vec![54, 54, 54, 56, 56, 56, 54], "{s}");

    // No `Failed` row when nothing failed — matches the shell's branch.
    let ok_only = render_summary(&results[..1], 1);
    assert!(!ok_only.contains("Failed"));
}

#[test]
fn plan_render_is_pasteable() {
    let p = plan(
        &spec(Path::new("/repo/_build/x/x.tar.gz"), Output::Tarball),
        &PlanEnv::default(),
    )
    .unwrap();
    let r = p.render();
    assert!(r.starts_with("(cd /repo && nix build .#redis-7"));
    // The two-word option value has to be quoted or the paste is wrong.
    assert!(r.contains("'nix-command flakes'"), "{r}");
}
