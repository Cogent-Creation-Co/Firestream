//! Public API surface check.
//!
//! Phase 2 of the firestream-ci toolkit extraction relies on a small set of items
//! from `otel_cli` being re-exported from the crate root. This test pins
//! that surface: if anything is renamed, moved, or accidentally gated behind
//! `pub(crate)`, the build fails here rather than in the downstream sibling
//! crate. It deliberately uses the items rather than just naming them — a
//! `use` alone would still compile against private re-exports.

use std::path::Path;

use otel_cli::{
    Config, ConfigError, GrpcClient, HttpJsonClient, HttpProtoClient, JsonFileClient, NullClient,
    OtlpClient, ReconcileError, ReconcileReport, build_client, client_from_env, reconcile_spans,
};

#[test]
fn concrete_client_constructors_are_pub() {
    // Exercise every constructor we promise is `pub`. The point of these
    // bindings is the type system; none of them perform IO.
    let cfg = Config::defaults();
    let _grpc: GrpcClient = GrpcClient::new(&cfg);
    let _proto: HttpProtoClient = HttpProtoClient::new(&cfg);
    let _json: HttpJsonClient = HttpJsonClient::new(&cfg);
    let _file: JsonFileClient = JsonFileClient::new("/tmp");
    let _null: NullClient = NullClient;
}

#[test]
fn build_client_returns_dyn_otlpclient_box() {
    let cfg = Config::defaults(); // no endpoint → NullClient under the hood
    let client: Box<dyn OtlpClient> = build_client(&cfg);
    drop(client);
}

#[tokio::test]
async fn client_from_env_returns_typed_result() {
    // We don't manipulate the process environment here (other tests in the
    // crate already cover that and serialise on a mutex); we just check the
    // signature compiles and the error type is `ConfigError`.
    let result: Result<Box<dyn OtlpClient>, ConfigError> = client_from_env();
    // Defaults — with whatever env this test inherits — must produce a
    // client; load_env() only fails on malformed bool/map env vars.
    let client = result.expect("client_from_env on default env");
    drop(client);
}

#[tokio::test]
async fn reconcile_spans_lib_api_signature_matches_plan() {
    // Exercise the documented signature:
    //   pub async fn reconcile_spans(
    //       result_file: &Path,
    //       spans_dir: &Path,
    //       output_dir: &Path,
    //   ) -> Result<ReconcileReport, ReconcileError>
    let tmp = tempfile::TempDir::new().unwrap();
    let result_file = tmp.path().join("result.json");
    std::fs::write(&result_file, br#"{"results":[]}"#).unwrap();

    let report: Result<ReconcileReport, ReconcileError> = reconcile_spans(
        result_file.as_path(),
        tmp.path() as &Path,
        tmp.path() as &Path,
    )
    .await;
    let report = report.expect("empty results file should parse cleanly");
    assert!(report.skipped_no_verdicts);
    assert_eq!(report.scanned, 0);
    assert_eq!(report.updated, 0);
    assert_eq!(report.verdicts, 0);
}
