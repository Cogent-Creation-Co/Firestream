//! Transport-agnostic readiness probes.
//!
//! Two flavours live here:
//!
//! 1. **Pure-network probes** ([`Tcp`], [`HttpReady`]). These touch host-side
//!    TCP and don't need any backend command-execution facility — they're
//!    identical whether the service is in a docker container or a k8s pod
//!    (assuming the harness has port-forwarded or published the port).
//!
//! 2. **Exec-driven probes** ([`PgIsReady`], [`RedisPing`],
//!    [`KafkaApiVersions`]). These run a vendor CLI INSIDE the service to
//!    verify protocol-level invariants the wire-level TCP probe can't see
//!    (e.g. Kafka's listener binds before metadata is serviceable). They
//!    are generic over an [`Exec`](crate::exec::Exec) impl; the harness
//!    constructs them with a `target` that means whatever its backend
//!    needs — compose service name for docker, pod name for kubectl.
//!
//! All probes are synchronous. The retry loop ([`retry::retry_until_sync`](crate::retry::retry_until_sync))
//! drives them with a wall-clock deadline. Keeping probes sync eliminates the
//! "tokio runtime dropped during unwind" hazard called out in the original
//! docker-harness audit.

use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};

use crate::exec::Exec;

/// Trait implemented by every readiness probe. `name` is used in failure
/// messages; `run` must be cheap and idempotent because the harness calls it
/// in a retry loop until success-or-deadline.
pub trait Probe {
    fn name(&self) -> &str;
    fn run(&self) -> Result<()>;
}

impl<P: Probe + ?Sized> Probe for Box<P> {
    fn name(&self) -> &str {
        (**self).name()
    }
    fn run(&self) -> Result<()> {
        (**self).run()
    }
}

// ---------- TCP ----------

/// Synchronous TCP "connect succeeds" probe. Tries every resolved address for
/// `127.0.0.1:port` with a 2s connect timeout per address; `ECONNREFUSED` /
/// `ETIMEDOUT` yield a retryable `Err`.
pub struct Tcp {
    pub port: u16,
}

impl Probe for Tcp {
    fn name(&self) -> &str {
        "tcp"
    }
    fn run(&self) -> Result<()> {
        let addrs: Vec<SocketAddr> = ("127.0.0.1", self.port)
            .to_socket_addrs()
            .with_context(|| format!("resolve 127.0.0.1:{}", self.port))?
            .collect();
        let timeout = Duration::from_secs(2);
        let mut last_err: Option<std::io::Error> = None;
        for addr in &addrs {
            match TcpStream::connect_timeout(addr, timeout) {
                Ok(_) => return Ok(()),
                Err(e) => last_err = Some(e),
            }
        }
        Err(anyhow!(
            "tcp connect 127.0.0.1:{} failed: {}",
            self.port,
            last_err
                .map(|e| e.to_string())
                .unwrap_or_else(|| "no addresses".into())
        ))
    }
}

// ---------- HttpReady ----------

/// Generic HTTP readiness probe. `reqwest::blocking::Client` GET; redirect
/// policy is `none`; ready iff the connect succeeds AND status < `max_status`
/// (so 200/301/302/401/403 all count as "the app is alive"; 5xx is not ready).
pub struct HttpReady {
    pub url: String,
    pub max_status: u16,
}

impl Probe for HttpReady {
    fn name(&self) -> &str {
        "http_ready"
    }
    fn run(&self) -> Result<()> {
        let client = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(5))
            .build()
            .context("build reqwest blocking client")?;
        let resp = client
            .get(&self.url)
            .send()
            .with_context(|| format!("GET {}", self.url))?;
        let status = resp.status().as_u16();
        if status < self.max_status {
            Ok(())
        } else {
            Err(anyhow!(
                "GET {} status {} >= max {}",
                self.url,
                status,
                self.max_status
            ))
        }
    }
}

// ---------- Exec-driven probes ----------
//
// These run a vendor readiness CLI inside the service under test. The
// caller picks the backend by passing an `Arc<dyn Exec>` and a `target`
// string interpreted by that backend (compose service / pod / …).

/// `pg_isready -U <user> -d <db>` against a Postgres service.
pub struct PgIsReady {
    pub exec: Arc<dyn Exec>,
    pub target: String,
    pub user: String,
    pub db: String,
}

impl Probe for PgIsReady {
    fn name(&self) -> &str {
        "pg_isready"
    }
    fn run(&self) -> Result<()> {
        // Pin the host + port so we hit the TCP listener rather than
        // pg_isready's compile-time `unix_socket_directories` default
        // (`/run/postgresql` in this build). The firestream-postgresql
        // image's chart-injected `unix_socket_directories` value points
        // at a writable emptyDir under `/opt/bitnami/postgresql/tmp` so
        // probing via the default socket path always fails. TCP probe is
        // the canonical Bitnami readiness pattern anyway (matches the
        // chart's own readinessProbe).
        let out = self
            .exec
            .exec(
                &self.target,
                &[
                    "pg_isready",
                    "-U", &self.user,
                    "-d", &self.db,
                    "-h", "127.0.0.1",
                    "-p", "5432",
                ],
            )
            .context("exec pg_isready")?;
        if out.status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "pg_isready exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

/// `redis-cli ping` against a Redis service — succeeds iff stdout is `PONG`.
pub struct RedisPing {
    pub exec: Arc<dyn Exec>,
    pub target: String,
}

impl Probe for RedisPing {
    fn name(&self) -> &str {
        "redis_ping"
    }
    fn run(&self) -> Result<()> {
        let out = self
            .exec
            .exec(&self.target, &["redis-cli", "ping"])
            .context("exec redis-cli ping")?;
        if !out.status.success() {
            bail!(
                "redis-cli ping exit {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            );
        }
        let body = String::from_utf8_lossy(&out.stdout);
        if body.trim() == "PONG" {
            Ok(())
        } else {
            Err(anyhow!("redis-cli ping returned {:?}", body))
        }
    }
}

/// `kafka-broker-api-versions --bootstrap-server <bootstrap>` against a Kafka
/// service. Verifies broker metadata is serviceable, not just that a listener
/// is bound (Kafka binds well before metadata is ready, so the TCP probe
/// alone is insufficient).
pub struct KafkaApiVersions {
    pub exec: Arc<dyn Exec>,
    pub target: String,
    pub bootstrap: String,
}

impl Probe for KafkaApiVersions {
    fn name(&self) -> &str {
        "kafka_api_versions"
    }
    fn run(&self) -> Result<()> {
        let out = self
            .exec
            .exec(
                &self.target,
                &[
                    "kafka-broker-api-versions",
                    "--bootstrap-server",
                    &self.bootstrap,
                ],
            )
            .context("exec kafka-broker-api-versions")?;
        if out.status.success() {
            Ok(())
        } else {
            Err(anyhow!(
                "kafka-broker-api-versions exited with {}: {}",
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retry::retry_until_sync;
    use std::io::Write;
    use std::net::TcpListener;
    use std::thread;
    use std::time::{Duration, Instant};

    // Get an ephemeral local TCP port by binding 0 then re-using the port.
    fn ephemeral_port() -> u16 {
        let l = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral");
        let port = l.local_addr().unwrap().port();
        drop(l);
        port
    }

    #[test]
    fn tcp_probe_succeeds_against_listening_socket() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 0");
        let port = listener.local_addr().unwrap().port();
        // Keep the listener alive for the duration of the probe by
        // moving it into a thread that accepts-and-drops a single conn.
        let handle = thread::spawn(move || {
            // Accept one connection so the probe sees an open socket;
            // immediately drop it to close.
            if let Ok((conn, _)) = listener.accept() {
                drop(conn);
            }
        });

        let probe = Tcp { port };
        probe.run().expect("connect to live listener");
        // Ensure the accept thread finishes so the listener is reaped.
        let _ = handle.join();
    }

    #[test]
    fn tcp_probe_fails_on_closed_port_within_budget() {
        // Bind+drop to lock in a port that is now definitely closed.
        let port = ephemeral_port();
        let probe = Tcp { port };
        let deadline = Instant::now() + Duration::from_millis(200);
        let result = retry_until_sync(&deadline, || probe.run());
        assert!(result.is_err(), "expected closed port to error out");
    }

    #[test]
    fn http_ready_probe_succeeds_against_in_test_server() {
        // Spin a tiny HTTP/1.1 server on an ephemeral port that returns
        // "HTTP/1.1 200 OK" once to the first connection. No external
        // dep — just std::net.
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind 0");
        let port = listener.local_addr().unwrap().port();
        let handle = thread::spawn(move || {
            if let Ok((mut conn, _)) = listener.accept() {
                let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
                // Read one line of the request (we don't validate it; just
                // drain enough to keep the client happy).
                let mut buf = [0u8; 1024];
                let _ = std::io::Read::read(&mut conn, &mut buf);
                let body = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK";
                let _ = conn.write_all(body);
            }
        });

        let probe = HttpReady {
            url: format!("http://127.0.0.1:{}/", port),
            max_status: 500,
        };
        probe.run().expect("HTTP 200 < 500 is ready");
        let _ = handle.join();
    }
}
