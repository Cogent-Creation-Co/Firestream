//! Codegen for firestream-ci's self-contained wire protocol (`proto/firestream/ci/v1/`).
//!
//! Mirrors the repo convention (see `core-types`): the generated Rust is
//! committed under `src/wire/` and a normal build just `include!`s it — no
//! `protoc` needed. Regenerate explicitly with the `codegen` feature inside the
//! Nix dev-shell (where `protoc` is on PATH):
//!
//!     cargo build -p firestream-ci --features codegen
//!
//! Keeping codegen feature-gated means the default build is hermetic and the
//! crate stays self-contained when extracted.
fn main() {
    println!("cargo:rerun-if-changed=proto/firestream/ci/v1/ci.proto");

    #[cfg(feature = "codegen")]
    {
        let out = std::path::Path::new("src/wire");
        std::fs::create_dir_all(out).expect("create src/wire");
        let mut cfg = prost_build::Config::new();
        // JSON-over-stdio: derive serde on every generated type. proto3 omits
        // default-valued scalars on the wire, and our JSON frames omit the
        // fields irrelevant to a given `kind`, so `#[serde(default)]` lets the
        // consumer deserialize partial frames.
        cfg.type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]");
        cfg.type_attribute(".", "#[serde(default)]");
        cfg.out_dir(out);
        cfg.compile_protos(&["proto/firestream/ci/v1/ci.proto"], &["proto"])
            .expect("compile firestream_ci proto");
    }
}
