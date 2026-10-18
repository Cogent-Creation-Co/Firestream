//! Null client — emits nothing (non-recording mode).
//!
//! Go reference: `otlpclient/otlp_client_null.go`.

use async_trait::async_trait;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;

use super::{ClientError, OtlpClient};

#[derive(Default)]
pub struct NullClient;

#[async_trait]
impl OtlpClient for NullClient {
    async fn start(&mut self) -> Result<(), ClientError> {
        Ok(())
    }
    async fn upload_traces(
        &mut self,
        _spans: Vec<ResourceSpans>,
    ) -> Result<(), ClientError> {
        Ok(())
    }
    async fn stop(&mut self) -> Result<(), ClientError> {
        Ok(())
    }
}
