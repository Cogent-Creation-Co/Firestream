//! `otel-cli version`.

use anyhow::Result;

pub fn run() -> Result<u8> {
    println!("{}", crate::version());
    Ok(0)
}
