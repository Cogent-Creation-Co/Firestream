//! Host-wide e2e lock.
//!
//! Both harnesses create real docker objects and k3d clusters, and the
//! sweeper deletes them. Two processes doing this at once would let a sweep
//! remove a live run's resources. So every harness test and every sweep runs
//! under an exclusive `flock(2)` on [`lock_path`]
//! (`${XDG_RUNTIME_DIR:-/tmp}/firestream-e2e.lock`).
//!
//! # Use from a harness
//!
//! Take the in-process mutex first, then the host lock, and hold both for
//! the whole test (setup, probes, teardown, leak check):
//!
//! ```no_run
//! # use std::sync::Mutex;
//! # static HARNESS: Mutex<()> = Mutex::new(());
//! let _in_process = HARNESS.lock().unwrap_or_else(|p| p.into_inner());
//! let _host = firestream_e2e_core::lock::HostLock::acquire()?;
//! // ... create, probe, tear down, leak-check ...
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! [`HostLock::acquire`] is re-entrant inside one process: while one
//! `HostLock` is alive, a second `acquire` returns a clone of it instead of
//! deadlocking on its own `flock`. So a harness that holds the lock can call
//! [`crate::sweep::sweep`] (which needs a `&HostLock`) without extra work.
//! Exclusion between threads of one process remains the job of the
//! in-process mutex.
//!
//! The lock file holds the pid of the holder, so a waiting process prints
//! `waiting for host lock … held by pid N`. The kernel drops the `flock`
//! when the holder exits, even on `SIGKILL`, so a crashed run never leaves
//! a stale lock.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use anyhow::{Context, Result};

/// File name of the lock inside the runtime dir.
pub const LOCK_FILE_NAME: &str = "firestream-e2e.lock";

/// `${XDG_RUNTIME_DIR:-/tmp}/firestream-e2e.lock`.
pub fn lock_path() -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    dir.join(LOCK_FILE_NAME)
}

#[derive(Debug)]
struct Held {
    file: File,
    path: PathBuf,
}

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
    }
}

/// Proof that this process holds the host-wide e2e lock. Cheap to clone;
/// the `flock` is released when the last clone drops.
#[derive(Debug, Clone)]
pub struct HostLock(Arc<Held>);

static HELD: Mutex<Weak<Held>> = Mutex::new(Weak::new());

impl HostLock {
    /// Block until this process holds the lock at [`lock_path`].
    pub fn acquire() -> Result<HostLock> {
        Self::acquire_at(&lock_path())
    }

    /// Block until this process holds the lock at `path`. Prints one
    /// "waiting" line to stderr if another process holds it.
    pub fn acquire_at(path: &Path) -> Result<HostLock> {
        let lock = Self::take(path, true)?;
        Ok(lock.expect("a blocking take always returns the lock"))
    }

    /// Take the lock at `path` without blocking. `Ok(None)` means another
    /// process holds it.
    pub fn try_acquire_at(path: &Path) -> Result<Option<HostLock>> {
        Self::take(path, false)
    }

    fn take(path: &Path, block: bool) -> Result<Option<HostLock>> {
        let mut held = HELD.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(existing) = held.upgrade()
            && existing.path == path
        {
            return Ok(Some(HostLock(existing)));
        }
        let file = open(path)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(TryLockError::WouldBlock) if !block => return Ok(None),
            Err(TryLockError::WouldBlock) => {
                let holder = read_holder(path)
                    .map(|pid| format!("pid {pid}"))
                    .unwrap_or_else(|| "an unknown process".to_string());
                eprintln!(
                    "[e2e] waiting for host lock {} held by {holder}",
                    path.display()
                );
                file.lock()
                    .with_context(|| format!("flock {}", path.display()))?;
            }
            Err(TryLockError::Error(e)) => {
                return Err(e).with_context(|| format!("flock {}", path.display()));
            }
        }
        let lock = HostLock(Arc::new(stamp(file, path)?));
        *held = Arc::downgrade(&lock.0);
        Ok(Some(lock))
    }

    pub fn path(&self) -> &Path {
        &self.0.path
    }
}

/// Pid recorded in the lock file at `path`, if any. The value is advisory:
/// it names the last holder, which may have exited.
pub fn read_holder(path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
}

fn open(path: &Path) -> Result<File> {
    // WARNING: never open with `truncate(true)`. Truncation happens before
    // the flock, so a waiter would erase the holder's pid.
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("open lock file {}", path.display()))
}

fn stamp(mut file: File, path: &Path) -> Result<Held> {
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| writeln!(file, "{}", std::process::id()))
        .and_then(|()| file.flush())
        .with_context(|| format!("write pid to {}", path.display()))?;
    Ok(Held {
        file,
        path: path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_child;
    use std::io::BufRead;
    use std::time::{Duration, Instant};

    const HOLD_ENV: &str = "FIRESTREAM_E2E_TEST_HOLD_LOCK";

    /// Child half of `lock_is_exclusive_between_processes`: takes the lock,
    /// prints `LOCKED`, then holds it until stdin closes.
    #[test]
    fn child_hold_lock() {
        let Some(path) = std::env::var_os(HOLD_ENV) else {
            return;
        };
        let _lock = HostLock::acquire_at(Path::new(&path)).unwrap();
        println!("LOCKED");
        let _ = std::io::stdin().read_line(&mut String::new());
    }

    #[test]
    fn lock_is_exclusive_between_processes() {
        let dir = tempdir();
        let path = dir.join(LOCK_FILE_NAME);
        let mut child = test_child::spawn(
            "lock::tests::child_hold_lock",
            &[(HOLD_ENV, path.to_str().unwrap())],
        );
        let stdout = child.stdout.take().unwrap();
        let mut lines = std::io::BufReader::new(stdout).lines();
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let line = lines.next().expect("child exited before LOCKED").unwrap();
            if line.contains("LOCKED") {
                break;
            }
            assert!(Instant::now() < deadline, "child never took the lock");
        }

        assert_eq!(read_holder(&path), Some(child.id()));
        assert!(HostLock::try_acquire_at(&path).unwrap().is_none());

        drop(child.stdin.take());
        child.wait().unwrap();

        let lock = HostLock::try_acquire_at(&path).unwrap().expect("lock free after child exit");
        assert_eq!(read_holder(&path), Some(std::process::id()));
        let again = HostLock::acquire_at(&path).unwrap();
        assert_eq!(again.path(), lock.path());
        drop(lock);
        drop(again);
        assert_eq!(read_holder(&path), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn lock_path_honours_xdg_runtime_dir() {
        let p = lock_path();
        assert!(p.ends_with(LOCK_FILE_NAME));
    }

    fn tempdir() -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fs-e2e-core-lock-test-{}-{}",
            std::process::id(),
            crate::run_id::RunId::random()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }
}
