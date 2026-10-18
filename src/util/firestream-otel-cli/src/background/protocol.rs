//! Length-prefixed JSON-frame IPC protocol for the `span background` mode.
//!
//! Wire format: 4-byte big-endian `u32` length followed by that many bytes of
//! JSON. The Go upstream uses `net/rpc/jsonrpc` which is conceptually similar
//! but uses newline-terminated frames; we use length prefixing to avoid
//! quirks with embedded newlines in JSON-encoded attribute values.
//!
//! Go reference: `otelcli/span_background_server.go` (the RPC frames there are
//! `BgSpanEvent`, `BgEnd`, and the `BgSpan` reply).

use std::collections::BTreeMap;
use std::io;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Request frames the background server understands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Request {
    /// Append an event to the running span.
    AddEvent {
        name: String,
        /// RFC3339Nano timestamp or "now".
        time: String,
        #[serde(default)]
        attrs: BTreeMap<String, String>,
    },
    /// End the running span and shut the server down.
    End {
        /// Optional explicit end time. `None` means "now".
        #[serde(default)]
        time: Option<String>,
        #[serde(default)]
        attrs: BTreeMap<String, String>,
        #[serde(default)]
        status_code: Option<String>,
        #[serde(default)]
        status_description: Option<String>,
    },
    /// No-op ping used by `--wait` to confirm the server is ready.
    Wait,
}

/// Response frames the server sends back to a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Response {
    Ok {
        /// Lowercase-hex trace id.
        trace_id: String,
        /// Lowercase-hex span id.
        span_id: String,
        /// Encoded `traceparent` for the running span.
        traceparent: String,
    },
    Err {
        message: String,
    },
}

/// Write one length-prefixed JSON frame to the stream.
pub async fn write_frame<W, T>(w: &mut W, msg: &T) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
    T: Serialize + ?Sized,
{
    let body = serde_json::to_vec(msg)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("serialize: {e}")))?;
    let len = u32::try_from(body.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "frame too large"))?;
    w.write_all(&len.to_be_bytes()).await?;
    w.write_all(&body).await?;
    w.flush().await?;
    Ok(())
}

/// Read one length-prefixed JSON frame from the stream.
pub async fn read_frame<R, T>(r: &mut R) -> io::Result<T>
where
    R: AsyncReadExt + Unpin,
    T: DeserializeOwned,
{
    let mut len_buf = [0u8; 4];
    r.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    // 16 MiB sanity cap — actual frames are much smaller (well under 64 KiB).
    if len > 16 * 1024 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame length {len} exceeds 16 MiB cap"),
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body).await?;
    serde_json::from_slice(&body)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("deserialize: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn write_and_read_frame_roundtrip() {
        let (mut a, mut b) = duplex(4096);
        let req = Request::End {
            time: None,
            attrs: BTreeMap::new(),
            status_code: None,
            status_description: None,
        };
        write_frame(&mut a, &req).await.unwrap();
        let got: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(req, got);
    }

    #[tokio::test]
    async fn multiple_frames_on_same_stream() {
        let (mut a, mut b) = duplex(4096);
        let mut attrs = BTreeMap::new();
        attrs.insert("foo".to_string(), "bar".to_string());
        let r1 = Request::AddEvent {
            name: "evt-1".to_string(),
            time: "now".to_string(),
            attrs: attrs.clone(),
        };
        let r2 = Request::AddEvent {
            name: "evt-2".to_string(),
            time: "now".to_string(),
            attrs,
        };
        write_frame(&mut a, &r1).await.unwrap();
        write_frame(&mut a, &r2).await.unwrap();

        let got1: Request = read_frame(&mut b).await.unwrap();
        let got2: Request = read_frame(&mut b).await.unwrap();
        assert_eq!(got1, r1);
        assert_eq!(got2, r2);
    }

    #[tokio::test]
    async fn response_roundtrip() {
        let (mut a, mut b) = duplex(4096);
        let resp = Response::Ok {
            trace_id: "0af7651916cd43dd8448eb211c80319c".to_string(),
            span_id: "b7ad6b7169203331".to_string(),
            traceparent: "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01".to_string(),
        };
        write_frame(&mut a, &resp).await.unwrap();
        let got: Response = read_frame(&mut b).await.unwrap();
        assert_eq!(resp, got);
    }

    #[tokio::test]
    async fn err_response_roundtrip() {
        let (mut a, mut b) = duplex(4096);
        let resp = Response::Err {
            message: "kaboom".to_string(),
        };
        write_frame(&mut a, &resp).await.unwrap();
        let got: Response = read_frame(&mut b).await.unwrap();
        assert_eq!(resp, got);
    }
}
