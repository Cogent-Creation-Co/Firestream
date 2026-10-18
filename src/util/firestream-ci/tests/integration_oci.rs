//! End-to-end OCI tests. Gated behind `BUILDER_E2E=1` AND the presence
//! of a running Docker daemon. Both gates must pass or the test silently
//! skips (returns Ok with a `println!` notice).
//!
//! Coverage:
//!   * Create a tiny `alpine:3` container, copy a synthetic source tree
//!     into it via `oci::source_sync::copy_source_to_container`, run a
//!     no-op command (`true`), capture exit code, assert success.
//!   * Inspect the synthesized tar contents from inside the container to
//!     verify the source-sync produced the expected files.
//!
//! Run with: `BUILDER_E2E=1 cargo test -p firestream-ci --test integration_oci`

use std::path::PathBuf;
use std::process::Command;

use firestream_ci::oci::{GitLsFiles, shared_docker_client};

fn e2e_enabled() -> bool {
    std::env::var("BUILDER_E2E").as_deref() == Ok("1")
}

async fn docker_available() -> bool {
    match shared_docker_client() {
        Ok(c) => c.inner().ping().await.is_ok(),
        Err(_) => false,
    }
}

#[tokio::test]
async fn source_sync_copy_works_against_real_alpine() {
    if !e2e_enabled() {
        println!("BUILDER_E2E unset; skipping integration_oci::source_sync");
        return;
    }
    if !docker_available().await {
        println!("docker daemon unavailable; skipping integration_oci::source_sync");
        return;
    }

    // Build a tiny synthetic git repo.
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path();
    let _ = Command::new("git")
        .args(["init", "-q", repo.to_str().unwrap()])
        .status();
    let _ = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "config", "user.email", "x@y"])
        .status();
    let _ = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "config", "user.name", "x"])
        .status();
    std::fs::write(repo.join("hello.txt"), b"hello from firestream-ci\n").unwrap();
    let _ = Command::new("git")
        .args(["-C", repo.to_str().unwrap(), "add", "."])
        .status();
    let _ = Command::new("git")
        .args([
            "-C",
            repo.to_str().unwrap(),
            "commit",
            "-q",
            "--no-gpg-sign",
            "-m",
            "init",
        ])
        .status();

    let client = shared_docker_client().expect("docker client");

    // Pull alpine:3 (small).
    let policy = firestream_ci::oci::RetryPolicy::default();
    firestream_ci::oci::pull_with_retry(&client, "alpine:3", "", policy)
        .await
        .expect("pull alpine");

    // Create + start a container that just sleeps.
    use bollard::container::{
        Config as ContainerConfig, CreateContainerOptions, StartContainerOptions,
    };
    let name = format!("firestream-ci-e2e-{}", std::process::id());
    let config: ContainerConfig<String> = ContainerConfig {
        image: Some("alpine:3".to_string()),
        cmd: Some(vec!["sleep".to_string(), "30".to_string()]),
        ..Default::default()
    };
    client
        .inner()
        .create_container(
            Some(CreateContainerOptions {
                name: name.clone(),
                platform: None,
            }),
            config,
        )
        .await
        .expect("create container");
    client
        .inner()
        .start_container(&name, None::<StartContainerOptions<String>>)
        .await
        .expect("start container");

    // Sync.
    let lsf = GitLsFiles::collect(repo).await.expect("ls-files");
    let bytes = firestream_ci::oci::copy_source_to_container(&client, &name, repo, "/tmp", &lsf)
        .await
        .expect("copy source");
    assert!(bytes > 0);

    // Exec ls /tmp/hello.txt inside and assert it succeeds.
    use bollard::exec::{CreateExecOptions, StartExecResults};
    let exec = client
        .inner()
        .create_exec(
            &name,
            CreateExecOptions {
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                cmd: Some(vec!["cat".to_string(), "/tmp/hello.txt".to_string()]),
                ..Default::default()
            },
        )
        .await
        .expect("create exec");
    let mut output = String::new();
    let exec_result = client
        .inner()
        .start_exec(&exec.id, None)
        .await
        .expect("start exec");
    if let StartExecResults::Attached {
        output: mut stream, ..
    } = exec_result
    {
        use futures::StreamExt;
        while let Some(chunk) = stream.next().await {
            if let Ok(out) = chunk {
                output.push_str(&format!("{out}"));
            }
        }
    }
    assert!(
        output.contains("hello from firestream-ci"),
        "exec output: `{output}`"
    );

    // Cleanup.
    let _ = client
        .inner()
        .remove_container(
            &name,
            Some(bollard::container::RemoveContainerOptions {
                force: true,
                ..Default::default()
            }),
        )
        .await;
}

#[tokio::test]
async fn classify_helpers_are_pure() {
    // This one runs always — it's a sanity check that the oci classify
    // helpers don't drift between unit and integration tests.
    use firestream_ci::oci::ReapClassification;
    let kinds = [
        ReapClassification::SameArchNonCanonical,
        ReapClassification::Legacy,
        ReapClassification::OtherArchCanonical,
        ReapClassification::NewCanonical,
        ReapClassification::NewlyDangling,
        ReapClassification::PreExistingDangling,
        ReapClassification::PreviousCanonical,
    ];
    let reap_count = kinds.iter().filter(|k| k.should_reap()).count();
    assert_eq!(reap_count, 4);
}

#[allow(dead_code)]
fn _absolute_path_compile_check() -> PathBuf {
    PathBuf::from("/tmp")
}
