//! Bollard `Docker` client construction + per-invocation sharing.
//!
//! Per the plan: don't statically cache the connection (we accept the
//! per-invocation cost), but within a single CLI invocation, share one
//! client across operations. The `DockerClient` newtype wraps `Arc<Docker>`
//! so cheap clones flow through to oci submodules.

use std::sync::Arc;

use bollard::Docker;

use super::Error;

/// Cheap-to-clone wrapper around a bollard `Docker` client.
#[derive(Clone)]
pub struct DockerClient(pub Arc<Docker>);

impl DockerClient {
    pub fn new() -> Result<Self, Error> {
        // `connect_with_local_defaults` reads DOCKER_HOST and falls back
        // to /var/run/docker.sock on Linux + the named pipe on Windows.
        // This is what the bash relies on (the `docker` CLI does the
        // same env probe internally).
        let docker = Docker::connect_with_local_defaults()?;
        Ok(Self(Arc::new(docker)))
    }

    pub fn inner(&self) -> &Docker {
        &self.0
    }
}

/// Construct a fresh `DockerClient`. Convenience over `DockerClient::new`
/// matching the call shape the plan uses (`shared_docker_client()`).
pub fn shared_docker_client() -> Result<DockerClient, Error> {
    DockerClient::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_docker_client_constructs_or_errors_cleanly() {
        // Docker may not be installed in the test environment; the
        // constructor MUST NOT panic. It returns Err on connect issues.
        match shared_docker_client() {
            Ok(c) => {
                let _ = c.inner();
            }
            Err(Error::Bollard(_)) => {}
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
