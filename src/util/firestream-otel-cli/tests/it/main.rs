//! Consolidated integration-test binary.
//!
//! Every former top-level `tests/*.rs` file is a module here so the crate is
//! linked once instead of once per file (9 links → 1). cargo-nextest (the
//! repo standard) still runs each test in its own process, so runtime
//! isolation is unchanged; plain `cargo test` runs all of these tests in a
//! single process.

mod background_smoke;
mod exec_smoke;
mod grpc_smoke;
mod http_smoke;
mod lib_api_surface;
mod memory_sampler_smoke;
mod server_smoke;
mod span_json_file;
mod status_smoke;
