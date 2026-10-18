//! Pattern #23 (`portable_timeout`) + miscellaneous small helpers shared
//! across modules. Pure-sync, no I/O on the hot path.

mod exit;
pub mod fs;
pub mod log_paths;
mod timeout;

pub use exit::{describe_exit, exit_is_signal, signal_from_exit};
pub use timeout::{PortableTimeout, portable_timeout_program, run_with_timeout};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("util: no portable timeout binary (neither `timeout` nor `gtimeout`) on PATH")]
    NoTimeoutBinary,
}
