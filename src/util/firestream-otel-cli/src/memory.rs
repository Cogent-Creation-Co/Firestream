//! Per-span memory tracking.
//!
//! Two halves:
//! - [`aggregate_rss_bytes`] — sum resident set size (RSS) across a PID and
//!   all of its descendants. Linux reads `/proc/<pid>/status`; macOS calls
//!   `proc_pidinfo` via libc; other targets return 0.
//! - [`query_window`] — open an NDJSON samples file written by the
//!   `memory-sampler` subcommand and compute [`WindowStats`] for the time
//!   window `[start_ns, end_ns]`. Used by `otel-cli span` to attach
//!   `memory.rss.*` attributes at emit time.
//!
//! Sampling races mean a process can disappear mid-walk. Every reader treats
//! a missing/unreadable PID as 0 bytes rather than an error, matching the
//! shell watchdog's "transient failures must not kill the sampler"
//! philosophy.
//!
//! Sample line format (one JSON object per line, append-only):
//! ```text
//! {"ts":1700000000000000000,"rss_kb":12345}
//! ```
//! Pressure events written by the sampler share the file but carry an
//! `"event"` field; the window query skips any line missing `rss_kb`.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

/// Stats computed from samples whose timestamps fall in `[start_ns, end_ns]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowStats {
    pub samples: u64,
    /// First in-window sample (earliest `ts`).
    pub start_bytes: u64,
    /// Last in-window sample (latest `ts`).
    pub end_bytes: u64,
    /// `end_bytes` − `start_bytes`. Signed because RSS can drop.
    pub delta_bytes: i64,
    /// Maximum sample in the window.
    pub peak_bytes: u64,
    pub p50_bytes: u64,
    pub p95_bytes: u64,
}

impl WindowStats {
    /// Seven `(key, val)` pairs ready to merge into `Config::attributes`.
    /// Values are decimal integers; the consumer's `string_to_any_value`
    /// coerces them to OTLP `intValue`.
    pub fn to_attrs(&self) -> Vec<(String, String)> {
        vec![
            ("memory.rss.samples".to_string(), self.samples.to_string()),
            (
                "memory.rss.start_bytes".to_string(),
                self.start_bytes.to_string(),
            ),
            (
                "memory.rss.end_bytes".to_string(),
                self.end_bytes.to_string(),
            ),
            (
                "memory.rss.delta_bytes".to_string(),
                self.delta_bytes.to_string(),
            ),
            (
                "memory.rss.peak_bytes".to_string(),
                self.peak_bytes.to_string(),
            ),
            (
                "memory.rss.p50_bytes".to_string(),
                self.p50_bytes.to_string(),
            ),
            (
                "memory.rss.p95_bytes".to_string(),
                self.p95_bytes.to_string(),
            ),
        ]
    }
}

/// Sum RSS in bytes across `root_pid` and all descendants. Returns 0 when the
/// PID is dead or unreadable. Never panics on transient I/O errors — a
/// missing process mid-walk is normal during 1 Hz polling.
pub fn aggregate_rss_bytes(root_pid: i32) -> u64 {
    platform::aggregate_rss_bytes(root_pid)
}

/// Scan an NDJSON samples file once, return stats for the in-window subset.
/// Returns `Ok(None)` when no samples land in the window (caller should emit
/// no attrs rather than zeros). Parse errors on individual lines are
/// silently skipped so a single malformed line cannot poison the result.
pub fn query_window(
    path: &Path,
    start_ns: u64,
    end_ns: u64,
) -> std::io::Result<Option<WindowStats>> {
    if start_ns > end_ns {
        return Ok(None);
    }
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let reader = BufReader::new(file);

    let mut first: Option<(u64, u64)> = None; // (ts, rss_bytes)
    let mut last: Option<(u64, u64)> = None;
    let mut peak: u64 = 0;
    let mut samples: Vec<u64> = Vec::new();

    for line in reader.lines() {
        let line = match line {
            Ok(s) => s,
            Err(_) => continue,
        };
        let (ts, rss_kb) = match parse_sample_line(&line) {
            Some(v) => v,
            None => continue,
        };
        if ts < start_ns || ts > end_ns {
            continue;
        }
        let rss = rss_kb.saturating_mul(1024);
        if first.is_none() || ts < first.unwrap().0 {
            first = Some((ts, rss));
        }
        if last.is_none() || ts > last.unwrap().0 {
            last = Some((ts, rss));
        }
        if rss > peak {
            peak = rss;
        }
        samples.push(rss);
    }

    if samples.is_empty() {
        return Ok(None);
    }

    samples.sort_unstable();
    let p50 = quantile(&samples, 50);
    let p95 = quantile(&samples, 95);
    let start_bytes = first.map(|(_, b)| b).unwrap_or(0);
    let end_bytes = last.map(|(_, b)| b).unwrap_or(0);
    let delta_bytes = (end_bytes as i64) - (start_bytes as i64);

    Ok(Some(WindowStats {
        samples: samples.len() as u64,
        start_bytes,
        end_bytes,
        delta_bytes,
        peak_bytes: peak,
        p50_bytes: p50,
        p95_bytes: p95,
    }))
}

/// Pick the smallest value `v` such that at least `q` percent of samples are
/// ≤ `v`. Operates on an already-sorted slice. Empty → 0.
fn quantile(sorted: &[u64], q: u8) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    // Nearest-rank: index = ceil(q/100 * n) - 1, clamped to [0, n-1].
    let n = sorted.len();
    let idx = ((q as usize * n).div_ceil(100))
        .saturating_sub(1)
        .min(n - 1);
    sorted[idx]
}

/// Parse one NDJSON line. Returns `Some((ts_ns, rss_kb))` for sample rows;
/// `None` for pressure/peak events or malformed input. We avoid pulling in
/// serde_json here because each line is a tiny flat object and we only need
/// two fields — `extract_u64_field` walks the bytes once.
fn parse_sample_line(line: &str) -> Option<(u64, u64)> {
    let ts = extract_u64_field(line, "ts")?;
    let rss_kb = extract_u64_field(line, "rss_kb")?;
    Some((ts, rss_kb))
}

/// Find `"<name>":<digits>` in a flat one-line JSON object and return the
/// integer. Returns `None` if the field is missing or non-numeric. Tolerant
/// of whitespace between the colon and the digits.
fn extract_u64_field(line: &str, name: &str) -> Option<u64> {
    let key = format!("\"{name}\":");
    let i = line.find(&key)?;
    let rest = &line[i + key.len()..];
    let rest = rest.trim_start();
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if end == 0 {
        return None;
    }
    rest[..end].parse::<u64>().ok()
}

// ============================================================================
// Platform-specific RSS readers
// ============================================================================

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::VecDeque;
    use std::fs;

    /// Read `VmRSS:` from `/proc/<pid>/status` and convert KiB → bytes.
    /// Returns 0 when the file is missing/unreadable.
    fn rss_bytes(pid: i32) -> u64 {
        let path = format!("/proc/{pid}/status");
        let txt = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return 0,
        };
        for line in txt.lines() {
            // The "VmRSS:" line looks like: `VmRSS:\t   12345 kB`
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kb = rest
                    .trim_start()
                    .split_ascii_whitespace()
                    .next()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                return kb.saturating_mul(1024);
            }
        }
        0
    }

    /// Children of `pid` from `/proc/<pid>/task/<pid>/children` (space-
    /// separated PIDs). Returns empty Vec on missing/empty/unreadable.
    fn children_of(pid: i32) -> Vec<i32> {
        let path = format!("/proc/{pid}/task/{pid}/children");
        let txt = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        txt.split_ascii_whitespace()
            .filter_map(|s| s.parse::<i32>().ok())
            .collect()
    }

    pub fn aggregate_rss_bytes(root_pid: i32) -> u64 {
        let mut total: u64 = 0;
        let mut queue: VecDeque<i32> = VecDeque::new();
        queue.push_back(root_pid);
        while let Some(pid) = queue.pop_front() {
            total = total.saturating_add(rss_bytes(pid));
            for child in children_of(pid) {
                queue.push_back(child);
            }
        }
        total
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::collections::HashMap;

    use libc::{
        PROC_PIDTASKINFO, PROC_PIDTBSDINFO, c_int, c_void, pid_t, proc_bsdinfo, proc_listallpids,
        proc_pidinfo, proc_taskinfo,
    };

    /// RSS in bytes for one PID via PROC_PIDTASKINFO. Returns 0 on failure
    /// (dead process, EPERM, etc).
    fn rss_bytes(pid: pid_t) -> u64 {
        let mut info: proc_taskinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<proc_taskinfo>() as c_int;
        let n = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTASKINFO,
                0,
                &mut info as *mut _ as *mut c_void,
                size,
            )
        };
        if n != size {
            return 0;
        }
        info.pti_resident_size
    }

    /// PPID for one PID via PROC_PIDTBSDINFO. Returns 0 on failure.
    fn ppid_of(pid: pid_t) -> pid_t {
        let mut info: proc_bsdinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<proc_bsdinfo>() as c_int;
        let n = unsafe {
            proc_pidinfo(
                pid,
                PROC_PIDTBSDINFO,
                0,
                &mut info as *mut _ as *mut c_void,
                size,
            )
        };
        if n != size {
            return 0;
        }
        info.pbi_ppid as pid_t
    }

    /// Enumerate every PID on the system. proc_listallpids takes a sized
    /// buffer; we call it once with `null` to learn how many PIDs to expect,
    /// then again to fill. Returns an empty Vec on error.
    fn all_pids() -> Vec<pid_t> {
        // Unlike the raw proc_listpids, proc_listallpids counts in PIDs rather
        // than bytes — on both the sizing call and the fill call.
        let needed = unsafe { proc_listallpids(std::ptr::null_mut(), 0) };
        if needed <= 0 {
            return Vec::new();
        }
        // The count is a moving target, so over-allocate to absorb processes
        // spawning between the two calls. 8k PIDs covers anything a CI builder
        // will see.
        let cap = (needed as usize).saturating_mul(2).max(8 * 1024);
        let mut buf: Vec<pid_t> = vec![0; cap];
        let got = unsafe {
            proc_listallpids(
                buf.as_mut_ptr() as *mut c_void,
                (buf.len() * std::mem::size_of::<pid_t>()) as c_int,
            )
        };
        if got <= 0 {
            return Vec::new();
        }
        buf.truncate((got as usize).min(buf.len()));
        // The kernel can include 0s as padding; filter them.
        buf.retain(|&p| p > 0);
        buf
    }

    pub fn aggregate_rss_bytes(root_pid: i32) -> u64 {
        // Build PPID → [PIDs] index from a single sweep, then BFS from root.
        // Doing it the other way (per-PID children lookup) would mean
        // O(N * tree-size) proc_pidinfo calls; this is O(N).
        let pids = all_pids();
        if pids.is_empty() {
            return 0;
        }
        let mut children: HashMap<pid_t, Vec<pid_t>> = HashMap::with_capacity(pids.len());
        let mut rss: HashMap<pid_t, u64> = HashMap::with_capacity(pids.len());
        for &pid in &pids {
            let ppid = ppid_of(pid);
            if ppid != 0 {
                children.entry(ppid).or_default().push(pid);
            }
            rss.insert(pid, rss_bytes(pid));
        }

        let mut total: u64 = 0;
        let mut queue: std::collections::VecDeque<pid_t> = std::collections::VecDeque::new();
        queue.push_back(root_pid as pid_t);
        while let Some(pid) = queue.pop_front() {
            if let Some(&b) = rss.get(&pid) {
                total = total.saturating_add(b);
            }
            if let Some(kids) = children.get(&pid) {
                for &child in kids {
                    queue.push_back(child);
                }
            }
        }
        total
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    pub fn aggregate_rss_bytes(_root_pid: i32) -> u64 {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_samples(lines: &[&str]) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
        f.flush().unwrap();
        f
    }

    #[test]
    fn parses_sample_lines() {
        assert_eq!(
            parse_sample_line(r#"{"ts":100,"rss_kb":2048}"#),
            Some((100, 2048))
        );
        // Order-independent, whitespace-tolerant.
        assert_eq!(
            parse_sample_line(r#"{"rss_kb": 1024,"ts": 50}"#),
            Some((50, 1024))
        );
        // Pressure event: missing rss_kb → ignored.
        assert_eq!(
            parse_sample_line(r#"{"ts":1,"event":"memory.pressure.soft","rss_mb":12000}"#),
            None
        );
        assert_eq!(parse_sample_line("garbage"), None);
    }

    #[test]
    fn missing_file_yields_none() {
        let path = std::path::PathBuf::from("/tmp/does-not-exist-otel-cli-test.jsonl");
        let _ = std::fs::remove_file(&path);
        let res = query_window(&path, 0, u64::MAX).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn empty_window_yields_none() {
        let f = write_samples(&[r#"{"ts":10,"rss_kb":1024}"#, r#"{"ts":20,"rss_kb":2048}"#]);
        let res = query_window(f.path(), 30, 40).unwrap();
        assert!(res.is_none());
    }

    #[test]
    fn stats_match_window() {
        // Samples at ts=10..50 with RSS 1, 2, 3, 4, 5 KiB. Query [20, 40]
        // should pick samples 2, 3, 4 KiB.
        let f = write_samples(&[
            r#"{"ts":10,"rss_kb":1024}"#,
            r#"{"ts":20,"rss_kb":2048}"#,
            r#"{"ts":30,"rss_kb":3072}"#,
            r#"{"ts":40,"rss_kb":4096}"#,
            r#"{"ts":50,"rss_kb":5120}"#,
        ]);
        let s = query_window(f.path(), 20, 40).unwrap().expect("some");
        assert_eq!(s.samples, 3);
        assert_eq!(s.start_bytes, 2048 * 1024);
        assert_eq!(s.end_bytes, 4096 * 1024);
        assert_eq!(s.delta_bytes, (4096 - 2048) * 1024);
        assert_eq!(s.peak_bytes, 4096 * 1024);
        // p50 of [2,3,4] (sorted) at q=50 → ceil(0.5*3)=2 → index 1 → 3072.
        assert_eq!(s.p50_bytes, 3072 * 1024);
        // p95 → ceil(0.95*3)=3 → index 2 → 4096.
        assert_eq!(s.p95_bytes, 4096 * 1024);
    }

    #[test]
    fn skips_malformed_and_event_lines() {
        let f = write_samples(&[
            r#"{"ts":10,"rss_kb":1024}"#,
            r#"garbage line"#,
            r#"{"ts":15,"event":"memory.pressure.soft","rss_mb":12000}"#,
            r#"{"ts":20,"rss_kb":2048}"#,
        ]);
        let s = query_window(f.path(), 0, 100).unwrap().expect("some");
        assert_eq!(s.samples, 2);
        assert_eq!(s.peak_bytes, 2048 * 1024);
    }

    #[test]
    fn delta_is_signed() {
        // RSS drops over the window — delta should be negative.
        let f = write_samples(&[r#"{"ts":10,"rss_kb":4096}"#, r#"{"ts":20,"rss_kb":1024}"#]);
        let s = query_window(f.path(), 0, 100).unwrap().expect("some");
        assert_eq!(s.delta_bytes, -(3072 * 1024));
    }

    #[test]
    fn to_attrs_emits_seven_keys() {
        let s = WindowStats {
            samples: 1,
            start_bytes: 100,
            end_bytes: 200,
            delta_bytes: 100,
            peak_bytes: 200,
            p50_bytes: 150,
            p95_bytes: 200,
        };
        let attrs = s.to_attrs();
        assert_eq!(attrs.len(), 7);
        let keys: Vec<&str> = attrs.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"memory.rss.samples"));
        assert!(keys.contains(&"memory.rss.delta_bytes"));
    }

    /// Reading the current process must produce a non-zero RSS on the two
    /// supported platforms. Other targets return 0 by design.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn current_process_rss_is_nonzero() {
        let me = std::process::id() as i32;
        let bytes = aggregate_rss_bytes(me);
        assert!(bytes > 0, "expected non-zero RSS for self");
    }

    /// A spawned child should be reachable from the parent via the
    /// process-tree walk, and querying its own PID should yield non-zero
    /// RSS. We don't compare before/after totals on the parent because
    /// parallel tests perturb the parent's RSS by far more than a `sleep`
    /// child contributes.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn child_process_is_summed() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("sleep")
            .arg("5")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn sleep");
        let child_pid = child.id() as i32;
        // Brief settle so the child is in /proc / procinfo.
        std::thread::sleep(std::time::Duration::from_millis(150));

        let child_rss = aggregate_rss_bytes(child_pid);

        let _ = child.kill();
        let _ = child.wait();

        assert!(
            child_rss > 0,
            "expected non-zero RSS for child pid {child_pid}, got {child_rss}"
        );
    }
}
