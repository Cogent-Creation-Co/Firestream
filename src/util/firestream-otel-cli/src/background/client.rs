//! Client helpers used by `span event`/`span end` to talk to the background
//! server over its unix socket.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use tokio::io::{BufReader, BufWriter};
use tokio::net::UnixStream;

use super::protocol::{read_frame, write_frame, Request, Response};
use super::server::socket_path;

/// Block until the background socket file shows up at `<sockdir>/otel-cli.sock`
/// or `timeout` elapses (whichever comes first). Polls every 25 ms — same
/// cadence as the Go upstream.
pub async fn wait_for_socket(sockdir: &Path, timeout: Duration) -> Result<()> {
    let path = socket_path(sockdir);
    let started = Instant::now();
    loop {
        if tokio::fs::metadata(&path).await.is_ok() {
            return Ok(());
        }
        if started.elapsed() >= timeout {
            anyhow::bail!(
                "timeout after {:?} waiting for background socket {}",
                timeout,
                path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// One-shot RPC: connect, send `req`, read one response, return.
pub async fn send_request(sockdir: &Path, req: &Request) -> Result<Response> {
    // Wait briefly for the socket — if the caller raced the background server
    // we'd otherwise see ECONNREFUSED. 5s matches what's reasonable for shell
    // pipelines.
    wait_for_socket(sockdir, Duration::from_secs(5)).await?;

    let path = socket_path(sockdir);
    let stream = UnixStream::connect(&path)
        .await
        .with_context(|| format!("connecting to {}", path.display()))?;
    let (reader, writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut writer = BufWriter::new(writer);
    write_frame(&mut writer, req).await.context("writing request")?;
    let resp: Response = read_frame(&mut reader).await.context("reading response")?;
    Ok(resp)
}
