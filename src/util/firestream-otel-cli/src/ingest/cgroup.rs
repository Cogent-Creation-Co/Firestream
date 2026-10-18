//! Best-effort cgroup v2 `memory.peak` probe (PRD §9.5 / §10.1).
//!
//! When Nix builders land in their own delegated child cgroup (W8's
//! `use-cgroups = true` + the container running `--cgroupns=host`), each
//! builder's `memory.peak` is readable and gives us `build.memory.peak_bytes`.
//!
//! We almost never have the builder's pid from `internal-json`, so the common
//! path is the directory scan: find the most-recently-modified `memory.peak`
//! under a Nix builder slice. This is heuristic and frequently misses (cgroup
//! v1 host, namespace mismatch, no delegation). On any miss the caller leaves
//! `build.memory.peak_bytes` unset and bumps `nix.cgroup.probe_missing` — we
//! never fabricate a value (PRD §9.4).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// cgroup v2 mount point. Fixed by the kernel ABI.
const CGROUP_ROOT: &str = "/sys/fs/cgroup";

/// Read `memory.peak` for the cgroup that contains `pid`, resolved via
/// `/proc/<pid>/cgroup`. Returns `None` on any failure (no such pid, cgroup v1,
/// unreadable file). This is the precise path used when the wrapper owns the
/// child and knows its pid.
pub fn peak_bytes_for_pid(pid: u32) -> Option<u64> {
    let rel = cgroup_rel_path_for_pid(pid)?;
    let peak = PathBuf::from(CGROUP_ROOT).join(rel).join("memory.peak");
    read_peak_file(&peak)
}

/// Parse `/proc/<pid>/cgroup` and return the cgroup v2 relative path (the
/// `0::<path>` line, leading slash stripped). cgroup v1 lines (with a non-zero
/// hierarchy id and controller list) are ignored.
fn cgroup_rel_path_for_pid(pid: u32) -> Option<String> {
    let text = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    parse_proc_cgroup(&text)
}

/// Extract the unified (v2) cgroup path from `/proc/<pid>/cgroup` contents.
/// The v2 line is `0::/some/path`. Returns the path with its leading `/`
/// stripped so it can be joined under [`CGROUP_ROOT`].
fn parse_proc_cgroup(text: &str) -> Option<String> {
    for line in text.lines() {
        // Format: hierarchy-ID:controller-list:cgroup-path
        let mut parts = line.splitn(3, ':');
        let hid = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        // The unified hierarchy is `0::<path>`.
        if hid == "0" && controllers.is_empty() {
            return Some(path.trim_start_matches('/').to_string());
        }
    }
    None
}

/// Read and parse a `memory.peak` file. The file holds a single decimal byte
/// count. Returns `None` if absent/unreadable/unparseable.
fn read_peak_file(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    text.trim().parse::<u64>().ok()
}

/// Heuristic fallback: scan known Nix builder slice locations for the
/// most-recently-modified `memory.peak` and read it. Used when we have no pid.
///
/// Nix builders under `systemd-run --user --scope` (W8) appear at paths like
/// `…/user.slice/user-<uid>.slice/…/nix-build-*.scope/memory.peak`; under the
/// daemon they appear beside `nix-daemon`/`nix-build` cgroups. We match
/// directory names containing `nix` and pick the freshest, since the build that
/// just closed is the one most recently touched.
///
/// This is intentionally conservative — it returns `None` rather than reading
/// an unrelated cgroup if nothing under [`CGROUP_ROOT`] looks like a builder.
pub fn peak_bytes_scan() -> Option<u64> {
    let root = Path::new(CGROUP_ROOT);
    if !root.is_dir() {
        return None;
    }
    let mut best: Option<(SystemTime, PathBuf)> = None;
    scan_dir(root, 0, &mut best);
    let (_, path) = best?;
    read_peak_file(&path)
}

/// Recursively walk cgroup dirs whose path contains a Nix-builder marker,
/// tracking the freshest `memory.peak`. Bounded depth so a pathological tree
/// can't run away.
fn scan_dir(dir: &Path, depth: usize, best: &mut Option<(SystemTime, PathBuf)>) {
    const MAX_DEPTH: usize = 8;
    if depth > MAX_DEPTH {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A builder cgroup leaf: directory name mentions nix. Its memory.peak
        // is the candidate.
        if is_builder_cgroup(&name) {
            let peak = path.join("memory.peak");
            if let Ok(meta) = std::fs::metadata(&peak) {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                let take = match best {
                    Some((best_mtime, _)) => mtime > *best_mtime,
                    None => true,
                };
                if take {
                    *best = Some((mtime, peak));
                }
            }
        }
        scan_dir(&path, depth + 1, best);
    }
}

/// True for cgroup directory names that correspond to a Nix builder. Matches
/// the `nix-build`/`nix-daemon` scope names systemd and the daemon create.
fn is_builder_cgroup(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("nix-build") || n.contains("nix-daemon") || n.starts_with("nix-")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_proc_cgroup_v2_line() {
        let text = "0::/user.slice/user-1000.slice/session-3.scope\n";
        assert_eq!(
            parse_proc_cgroup(text),
            Some("user.slice/user-1000.slice/session-3.scope".to_string())
        );
    }

    #[test]
    fn parse_proc_cgroup_ignores_v1_lines() {
        // Hybrid hierarchy: v1 controller lines then the v2 line.
        let text = "\
12:memory:/some/v1/path
11:cpu,cpuacct:/another
0::/the/v2/path
";
        assert_eq!(parse_proc_cgroup(text), Some("the/v2/path".to_string()));
    }

    #[test]
    fn parse_proc_cgroup_none_when_no_v2() {
        let text = "12:memory:/only/v1\n";
        assert_eq!(parse_proc_cgroup(text), None);
    }

    #[test]
    fn read_peak_file_parses_decimal() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("memory.peak");
        std::fs::write(&p, "123456\n").unwrap();
        assert_eq!(read_peak_file(&p), Some(123456));
    }

    #[test]
    fn read_peak_file_missing_is_none() {
        assert_eq!(read_peak_file(Path::new("/nonexistent/memory.peak")), None);
    }

    #[test]
    fn builder_cgroup_matcher() {
        assert!(is_builder_cgroup("nix-build-foo.scope"));
        assert!(is_builder_cgroup("nix-daemon.service"));
        assert!(is_builder_cgroup("nix-12345"));
        assert!(!is_builder_cgroup("user.slice"));
        assert!(!is_builder_cgroup("system.slice"));
    }

    /// In this test environment the builder cgroup almost certainly doesn't
    /// exist; the scan must degrade to `None` without panicking. (If it
    /// happens to find one, a non-negative integer is also acceptable.)
    #[test]
    fn scan_degrades_cleanly() {
        let _ = peak_bytes_scan();
        // No assertion on the value — the contract is "doesn't panic".
    }
}
