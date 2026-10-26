//! Re-run one test of this test binary in a child process.
//!
//! The child test reads its instructions from env vars and returns at once
//! when they are absent, so it passes as a no-op in a normal `cargo test`.

use std::process::{Child, Command, Stdio};

pub fn spawn(test: &str, envs: &[(&str, &str)]) -> Child {
    let exe = std::env::current_exe().expect("current_exe");
    let mut cmd = Command::new(exe);
    cmd.args([test, "--exact", "--nocapture", "--test-threads=1", "--quiet"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.spawn().expect("spawn child test process")
}
