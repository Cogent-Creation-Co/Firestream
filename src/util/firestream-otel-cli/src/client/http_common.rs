//! Shared plumbing for the HTTP/protobuf and HTTP/JSON clients.
//!
//! Go references: `otlpclient/otlp_client_http.go::tracesEndpoint`,
//! `tracesURL`, and the retry loop in `otlpclient/otlp_client.go::retry`.
//!
//! Both clients share endpoint resolution, reqwest construction, header
//! translation, and the linear-backoff retry loop. The only differences are
//! the request `Content-Type` and how the body is encoded; the clients
//! supply those themselves.

use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use crate::config::{parse_duration, Config};

use super::retry::backoff_for_attempt;
use super::ClientError;

/// Max retry attempts, matching Go's effective behaviour (the Go retry loop
/// is bounded by deadline rather than a count, but it sleeps in 100ms steps
/// and we have a tighter deadline here).
const MAX_ATTEMPTS: u32 = 3;

/// Resolve the trace upload URL from a config. Mirrors Go's
/// `tracesEndpoint` + `tracesURL`: prefer the signal-specific override, fall
/// back to the generic endpoint, prepend a scheme when missing, and ensure
/// the path ends in `/v1/traces` when the user didn't supply one.
pub fn resolve_url(cfg: &Config) -> Result<Url, ClientError> {
    let base = if !cfg.traces_endpoint.is_empty() {
        cfg.traces_endpoint.as_str()
    } else {
        cfg.endpoint.as_str()
    };

    if base.is_empty() {
        return Err(ClientError::Transport(
            "no endpoint configured".to_string(),
        ));
    }

    // Prepend scheme if missing. Go's `tracesURL` defaults to `https` unless
    // `Insecure` is true; we mirror that.
    let with_scheme = if base.contains("://") {
        base.to_string()
    } else if cfg.insecure {
        format!("http://{base}")
    } else {
        format!("https://{base}")
    };

    let mut url = Url::parse(&with_scheme)
        .map_err(|e| ClientError::Transport(format!("invalid endpoint {with_scheme:?}: {e}")))?;

    // If the user only specified a signal-specific `traces_endpoint`, treat
    // their path as authoritative. Otherwise (using the generic `endpoint`)
    // ensure `/v1/traces` is appended when the path is empty or `/`.
    if cfg.traces_endpoint.is_empty() {
        let path = url.path();
        if path.is_empty() || path == "/" {
            url.set_path("/v1/traces");
        }
    }

    Ok(url)
}

/// Build a [`reqwest::Client`] with the configured TLS/timeout, plus a
/// pre-populated `HeaderMap` carrying user headers, the supplied
/// `Content-Type`, and a default `User-Agent`.
pub fn build_reqwest_client(
    cfg: &Config,
    content_type: &str,
) -> Result<(reqwest::Client, HeaderMap, Duration), ClientError> {
    let timeout = parse_duration(&cfg.timeout).unwrap_or(Duration::from_secs(1));

    let mut builder = reqwest::Client::builder().timeout(timeout);

    // TODO(phase 4-followup): cert file loading (cfg.tls_ca_cert,
    // cfg.tls_client_cert, cfg.tls_client_key) via rustls-pemfile. For now
    // honour only `tls_no_verify`, which covers the common dev-loop case.
    if cfg.tls_no_verify {
        builder = builder.danger_accept_invalid_certs(true);
    }

    let client = builder
        .build()
        .map_err(|e| ClientError::Transport(format!("reqwest build failed: {e}")))?;

    let mut headers = HeaderMap::with_capacity(cfg.headers.len() + 2);
    for (k, v) in &cfg.headers {
        let name = HeaderName::from_bytes(k.as_bytes())
            .map_err(|e| ClientError::Transport(format!("invalid header name {k:?}: {e}")))?;
        let value = HeaderValue::from_str(v)
            .map_err(|e| ClientError::Transport(format!("invalid header value for {k:?}: {e}")))?;
        headers.insert(name, value);
    }

    headers.insert(
        reqwest::header::CONTENT_TYPE,
        HeaderValue::from_str(content_type)
            .map_err(|e| ClientError::Transport(format!("bad content-type {content_type:?}: {e}")))?,
    );

    if !headers.contains_key(reqwest::header::USER_AGENT) {
        let ua = format!("otel-cli/{}", env!("CARGO_PKG_VERSION"));
        headers.insert(
            reqwest::header::USER_AGENT,
            HeaderValue::from_str(&ua)
                .map_err(|e| ClientError::Transport(format!("bad user-agent {ua:?}: {e}")))?,
        );
    }

    Ok((client, headers, timeout))
}

/// POST `body` to `url`, retrying on transient failures. Mirrors the Go
/// `retry` loop + `processHTTPStatus` decisions:
/// - 2xx → Ok
/// - 429, 502, 503, 504 → retry (honour `Retry-After` seconds if present)
/// - other 3xx/4xx/5xx → return `Status` error, no retry
/// - connection errors → retry
pub async fn send_with_retry(
    client: &reqwest::Client,
    url: &Url,
    headers: &HeaderMap,
    body: Vec<u8>,
) -> Result<(), ClientError> {
    let mut last_err: Option<ClientError> = None;
    // Server-suggested wait, consumed once before the next request.
    let mut next_wait: Option<Duration> = None;

    for attempt in 0..MAX_ATTEMPTS {
        if attempt > 0 {
            let wait = next_wait
                .take()
                .unwrap_or_else(|| backoff_for_attempt(attempt));
            tokio::time::sleep(wait).await;
        }

        let resp = match client
            .post(url.clone())
            .headers(headers.clone())
            .body(body.clone())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                // Connection-level failures are retriable.
                last_err = Some(ClientError::Transport(format!("request failed: {e}")));
                continue;
            }
        };

        let status = resp.status();
        if status.is_success() {
            return Ok(());
        }

        let retry_after = retry_after_secs(&resp);
        let code = status.as_u16();
        let body_text = resp.text().await.unwrap_or_default();

        if matches!(code, 429 | 502 | 503 | 504) {
            last_err = Some(ClientError::Status(format!("HTTP {code}: {body_text}")));
            next_wait = retry_after.map(Duration::from_secs);
            continue;
        }

        return Err(ClientError::Status(format!("HTTP {code}: {body_text}")));
    }

    Err(last_err.unwrap_or_else(|| ClientError::Transport("retries exhausted".to_string())))
}

fn retry_after_secs(resp: &reqwest::Response) -> Option<u64> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.trim().parse::<u64>().ok())
}
