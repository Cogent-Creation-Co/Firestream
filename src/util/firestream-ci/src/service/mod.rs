//! Pattern #22 — cross-platform service unit installer. Templates via
//! `include_str!` + `format!`; no external templating engine. Mirrors the
//! shape the replay-daemon scripts (`bin/install_scripts/replay-daemon-*.sh`,
//! forward-referenced from the plan — they're written by this PR family).
//!
//! ## Cross-platform shape
//!
//! On macOS, install a launchd plist into `~/Library/LaunchAgents/<name>.plist`
//! and `launchctl load -w` it. On Linux, install a `.service` + `.timer`
//! pair into `~/.config/systemd/user/` and `systemctl --user enable --now
//! <name>.timer` it. The `.install()` dispatcher picks the right backend
//! by `cfg!(target_os)`.
//!
//! ## Schedule
//!
//! `ScheduleKind` is the lowest-common-denominator schedule shape:
//!   * `Interval(d)` → launchd `StartInterval`, systemd `OnUnitActiveSec=<d>`
//!   * `Cron(s)`     → launchd `StartCalendarInterval` (parsed best-effort),
//!     systemd `OnCalendar=<s>` (passthrough; systemd-time(7))
//!   * `OnDemand`    → no schedule key; user invokes via `launchctl start` /
//!     `systemctl --user start`.
//!
//! Cron→launchd is intentionally minimal: we support `"@hourly"`, `"@daily"`,
//! `"@weekly"`, and `"<min> <hour> * * *"`. Anything richer falls back to
//! `OnDemand` with a `tracing::warn!` — the bash install scripts only ever
//! used one of those four anyway.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use thiserror::Error;
use tokio::fs;

#[derive(Debug, Error)]
pub enum Error {
    #[error("service: I/O error on `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("service: install command failed for `{kind}` ({code}): {stderr}")]
    InstallCommand {
        kind: &'static str,
        code: i32,
        stderr: String,
    },
    #[error("service: invalid unit name `{0}` (must be [A-Za-z0-9_-]+, no path separators)")]
    InvalidName(String),
}

/// Recurrence shape.
#[derive(Debug, Clone)]
pub enum ScheduleKind {
    Interval(Duration),
    /// Raw cron string. Systemd uses it verbatim; launchd parses a small
    /// subset (`@hourly`, `@daily`, `@weekly`, `M H * * *`).
    Cron(String),
    OnDemand,
}

/// A self-contained service description. The same struct drives both
/// launchd plist and systemd unit generation.
#[derive(Debug, Clone)]
pub struct ServiceUnit {
    pub name: String,
    pub description: String,
    pub command: PathBuf,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub schedule: ScheduleKind,
    /// Override the install target (mostly for tests). When unset:
    ///   * launchd: `~/Library/LaunchAgents/<name>.plist`
    ///   * systemd: `~/.config/systemd/user/{<name>.service, <name>.timer}`
    pub install_dir: Option<PathBuf>,
    /// Where launchd should redirect stdout / stderr. Defaults to
    /// `/tmp/<name>.{out,err}.log`. Ignored on systemd (journal handles it).
    pub stdout_path: Option<PathBuf>,
    pub stderr_path: Option<PathBuf>,
    /// Extra raw lines for the systemd `[Service]` section (e.g. `Nice=19`,
    /// `IOSchedulingClass=idle`). Ignored by the launchd renderer.
    pub service_extra: Vec<String>,
    /// Extra raw lines for the systemd `[Timer]` section (e.g.
    /// `RandomizedDelaySec=30m`). Ignored by the launchd renderer.
    pub timer_extra: Vec<String>,
}

impl ServiceUnit {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        command: impl Into<PathBuf>,
    ) -> Result<Self, Error> {
        let name = name.into();
        validate_name(&name)?;
        Ok(Self {
            name,
            description: description.into(),
            command: command.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            schedule: ScheduleKind::OnDemand,
            install_dir: None,
            stdout_path: None,
            stderr_path: None,
            service_extra: Vec::new(),
            timer_extra: Vec::new(),
        })
    }

    pub fn with_arg(mut self, a: impl Into<String>) -> Self {
        self.args.push(a.into());
        self
    }

    pub fn with_args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    pub fn with_env(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.env.insert(k.into(), v.into());
        self
    }

    pub fn with_schedule(mut self, s: ScheduleKind) -> Self {
        self.schedule = s;
        self
    }

    pub fn with_install_dir(mut self, d: impl Into<PathBuf>) -> Self {
        self.install_dir = Some(d.into());
        self
    }

    pub fn with_stdout_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.stdout_path = Some(p.into());
        self
    }

    pub fn with_stderr_path(mut self, p: impl Into<PathBuf>) -> Self {
        self.stderr_path = Some(p.into());
        self
    }

    pub fn with_service_extra(mut self, line: impl Into<String>) -> Self {
        self.service_extra.push(line.into());
        self
    }

    pub fn with_timer_extra(mut self, line: impl Into<String>) -> Self {
        self.timer_extra.push(line.into());
        self
    }

    /// Render the launchd plist string for inspection / persistence.
    /// Exposed so callers can write to a custom path without invoking
    /// `launchctl`.
    pub fn render_launchd_plist(&self) -> String {
        const TPL: &str = include_str!("templates/launchd.plist.template");
        let label = self.name.clone();
        let mut argv_lines = String::new();
        argv_lines.push_str(&format!(
            "        <string>{}</string>\n",
            xml_escape(&self.command.to_string_lossy())
        ));
        for a in &self.args {
            argv_lines.push_str(&format!("        <string>{}</string>\n", xml_escape(a)));
        }
        let schedule_block = match &self.schedule {
            ScheduleKind::Interval(d) => format!(
                "    <key>StartInterval</key>\n    <integer>{}</integer>",
                d.as_secs().max(1)
            ),
            ScheduleKind::Cron(c) => cron_to_launchd_block(c).unwrap_or_default(),
            ScheduleKind::OnDemand => String::new(),
        };
        let env_block = if self.env.is_empty() {
            String::new()
        } else {
            let mut s = String::from("    <key>EnvironmentVariables</key>\n    <dict>\n");
            for (k, v) in &self.env {
                s.push_str(&format!(
                    "        <key>{}</key>\n        <string>{}</string>\n",
                    xml_escape(k),
                    xml_escape(v)
                ));
            }
            s.push_str("    </dict>");
            s
        };
        let stdout = self
            .stdout_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/{}.out.log", self.name)));
        let stderr = self
            .stderr_path
            .clone()
            .unwrap_or_else(|| PathBuf::from(format!("/tmp/{}.err.log", self.name)));

        TPL.replace("{LABEL}", &xml_escape(&label))
            .replace("{PROGRAM_ARGUMENTS}", argv_lines.trim_end())
            .replace("{SCHEDULE_BLOCK}", &schedule_block)
            .replace("{ENV_BLOCK}", &env_block)
            .replace("{STDOUT_PATH}", &xml_escape(&stdout.to_string_lossy()))
            .replace("{STDERR_PATH}", &xml_escape(&stderr.to_string_lossy()))
    }

    /// Render the systemd `.service` unit body.
    pub fn render_systemd_service(&self) -> String {
        const TPL: &str = include_str!("templates/systemd.service.template");
        let mut exec = self.command.to_string_lossy().to_string();
        for a in &self.args {
            // Defensive quoting — bare spaces would break the systemd parse.
            if a.chars().any(char::is_whitespace) {
                exec.push_str(&format!(" \"{}\"", a.replace('"', "\\\"")));
            } else {
                exec.push(' ');
                exec.push_str(a);
            }
        }
        // service_extra rides the same placeholder as env — both land in
        // [Service], and folding them keeps the template stable.
        let env_block = self
            .env
            .iter()
            .map(|(k, v)| format!("Environment=\"{k}={v}\""))
            .chain(self.service_extra.iter().cloned())
            .collect::<Vec<_>>()
            .join("\n");
        TPL.replace("{DESCRIPTION}", &self.description)
            .replace("{EXEC_START}", &exec)
            .replace("{ENV_BLOCK}", &env_block)
    }

    /// Render the systemd `.timer` unit body, or `None` if `OnDemand`.
    pub fn render_systemd_timer(&self) -> Option<String> {
        const TPL: &str = include_str!("templates/systemd.timer.template");
        let mut timer_schedule = match &self.schedule {
            ScheduleKind::Interval(d) => format!("OnUnitActiveSec={}s", d.as_secs().max(1)),
            ScheduleKind::Cron(c) => format!("OnCalendar={c}"),
            ScheduleKind::OnDemand => return None,
        };
        for line in &self.timer_extra {
            timer_schedule.push('\n');
            timer_schedule.push_str(line);
        }
        Some(
            TPL.replace("{DESCRIPTION}", &self.description)
                .replace("{TIMER_SCHEDULE}", &timer_schedule),
        )
    }

    /// Install on the host this binary is running on. Dispatches by
    /// `cfg!(target_os)`. macOS → launchd; everything else → systemd.
    pub async fn install(&self) -> Result<InstallReport, Error> {
        #[cfg(target_os = "macos")]
        {
            self.install_launchd().await
        }
        #[cfg(not(target_os = "macos"))]
        {
            self.install_systemd_user().await
        }
    }

    /// Write the plist + `launchctl load -w`. Caller is responsible for the
    /// `launchctl unload` lifecycle on upgrade — the bash equivalent is
    /// idempotent by writing the file first, then `load -w` (which is a
    /// no-op on already-loaded units of the same label).
    pub async fn install_launchd(&self) -> Result<InstallReport, Error> {
        let plist = self.render_launchd_plist();
        let dir = self
            .install_dir
            .clone()
            .unwrap_or_else(|| home_dir().join("Library").join("LaunchAgents"));
        fs::create_dir_all(&dir).await.map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        let path = dir.join(format!("{}.plist", self.name));
        fs::write(&path, plist.as_bytes())
            .await
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        // launchctl load may not be available in tests; surface its result
        // through the report so callers can decide.
        let load_result = run_command(
            "launchctl",
            &["load", "-w", path.to_string_lossy().as_ref()],
        )
        .await;

        Ok(InstallReport {
            kind: InstallKind::Launchd,
            files: vec![path],
            backend_invoked: matches!(load_result, Ok(true)),
            backend_error: load_result.err(),
        })
    }

    /// Write `.service` + `.timer` (if scheduled) and
    /// `systemctl --user enable --now <name>.timer`. On `OnDemand`, only
    /// the `.service` is written and the unit must be invoked manually.
    pub async fn install_systemd_user(&self) -> Result<InstallReport, Error> {
        let dir = self
            .install_dir
            .clone()
            .unwrap_or_else(|| home_dir().join(".config").join("systemd").join("user"));
        fs::create_dir_all(&dir).await.map_err(|source| Error::Io {
            path: dir.clone(),
            source,
        })?;
        let service_path = dir.join(format!("{}.service", self.name));
        fs::write(&service_path, self.render_systemd_service().as_bytes())
            .await
            .map_err(|source| Error::Io {
                path: service_path.clone(),
                source,
            })?;
        let mut files = vec![service_path];

        let unit_to_enable;
        if let Some(timer) = self.render_systemd_timer() {
            let timer_path = dir.join(format!("{}.timer", self.name));
            fs::write(&timer_path, timer.as_bytes())
                .await
                .map_err(|source| Error::Io {
                    path: timer_path.clone(),
                    source,
                })?;
            unit_to_enable = format!("{}.timer", self.name);
            files.push(timer_path);
        } else {
            unit_to_enable = format!("{}.service", self.name);
        }

        let enable = run_command(
            "systemctl",
            &["--user", "enable", "--now", unit_to_enable.as_str()],
        )
        .await;

        Ok(InstallReport {
            kind: InstallKind::SystemdUser,
            files,
            backend_invoked: matches!(enable, Ok(true)),
            backend_error: enable.err(),
        })
    }
}

/// Outcome of an install — what was written + whether the backend
/// (launchctl / systemctl) accepted the load. Callers handle the
/// backend-failed-but-files-written case by deciding whether to roll back.
#[derive(Debug)]
pub struct InstallReport {
    pub kind: InstallKind,
    pub files: Vec<PathBuf>,
    pub backend_invoked: bool,
    pub backend_error: Option<Error>,
}

#[derive(Debug, Clone, Copy)]
pub enum InstallKind {
    Launchd,
    SystemdUser,
}

fn validate_name(name: &str) -> Result<(), Error> {
    if name.is_empty() {
        return Err(Error::InvalidName(name.into()));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(Error::InvalidName(name.into()));
    }
    if name.contains('/') || name.contains("..") {
        return Err(Error::InvalidName(name.into()));
    }
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Minimal cron→launchd mapper. Returns `None` for shapes we don't model.
fn cron_to_launchd_block(cron: &str) -> Option<String> {
    let trimmed = cron.trim();
    match trimmed {
        "@hourly" => Some(format_calendar_block(&[("Minute", 0)])),
        "@daily" | "@midnight" => Some(format_calendar_block(&[("Hour", 0), ("Minute", 0)])),
        "@weekly" => {
            // Weekday 0 = Sunday in launchd
            Some(format_calendar_block(&[
                ("Weekday", 0),
                ("Hour", 0),
                ("Minute", 0),
            ]))
        }
        _ => {
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            // "<min> <hour> * * *"
            if parts.len() == 5 && parts[2] == "*" && parts[3] == "*" && parts[4] == "*" {
                if let (Ok(min), Ok(hr)) = (parts[0].parse::<i32>(), parts[1].parse::<i32>()) {
                    return Some(format_calendar_block(&[("Hour", hr), ("Minute", min)]));
                }
            }
            tracing::warn!(
                target: "firestream_ci::service",
                cron = %trimmed,
                "cron→launchd: unsupported shape; install will skip schedule"
            );
            None
        }
    }
}

fn format_calendar_block(entries: &[(&str, i32)]) -> String {
    let mut s = String::from("    <key>StartCalendarInterval</key>\n    <dict>\n");
    for (k, v) in entries {
        s.push_str(&format!(
            "        <key>{k}</key>\n        <integer>{v}</integer>\n"
        ));
    }
    s.push_str("    </dict>");
    s
}

async fn run_command(program: &str, args: &[&str]) -> Result<bool, Error> {
    let result = tokio::process::Command::new(program)
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    let output = match result {
        Ok(o) => o,
        Err(_) => return Ok(false), // backend not present; caller decides
    };
    if output.status.success() {
        Ok(true)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        let code = output.status.code().unwrap_or(-1);
        Err(Error::InstallCommand {
            kind: if program == "launchctl" {
                "launchd"
            } else {
                "systemd"
            },
            code,
            stderr,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample_unit() -> ServiceUnit {
        ServiceUnit::new("firestream-replay", "firestream-ci span replay", "/usr/local/bin/firestream-ci")
            .unwrap()
            .with_arg("spans")
            .with_arg("replay")
            .with_env("OTEL_EXPORTER_OTLP_ENDPOINT", "http://localhost:4318")
    }

    #[test]
    fn rejects_invalid_names() {
        assert!(ServiceUnit::new("..", "x", "/bin/true").is_err());
        assert!(ServiceUnit::new("a/b", "x", "/bin/true").is_err());
        assert!(ServiceUnit::new("", "x", "/bin/true").is_err());
        assert!(ServiceUnit::new("ok-name_1.2", "x", "/bin/true").is_ok());
    }

    #[test]
    fn renders_launchd_plist_with_interval() {
        let u = sample_unit().with_schedule(ScheduleKind::Interval(Duration::from_secs(300)));
        let xml = u.render_launchd_plist();
        assert!(xml.contains("<key>Label</key>"));
        assert!(xml.contains("<string>firestream-replay</string>"));
        assert!(xml.contains("<string>/usr/local/bin/firestream-ci</string>"));
        assert!(xml.contains("<string>spans</string>"));
        assert!(xml.contains("<key>StartInterval</key>"));
        assert!(xml.contains("<integer>300</integer>"));
        assert!(xml.contains("OTEL_EXPORTER_OTLP_ENDPOINT"));
    }

    #[test]
    fn renders_launchd_with_cron_hourly() {
        let u = sample_unit().with_schedule(ScheduleKind::Cron("@hourly".into()));
        let xml = u.render_launchd_plist();
        assert!(xml.contains("<key>StartCalendarInterval</key>"));
        assert!(xml.contains("<key>Minute</key>"));
        assert!(xml.contains("<integer>0</integer>"));
    }

    #[test]
    fn renders_launchd_with_cron_m_h_pattern() {
        let u = sample_unit().with_schedule(ScheduleKind::Cron("15 3 * * *".into()));
        let xml = u.render_launchd_plist();
        assert!(xml.contains("<key>Hour</key>"));
        assert!(xml.contains("<integer>3</integer>"));
        assert!(xml.contains("<integer>15</integer>"));
    }

    #[test]
    fn renders_systemd_service_and_timer() {
        let u = sample_unit().with_schedule(ScheduleKind::Interval(Duration::from_secs(600)));
        let svc = u.render_systemd_service();
        assert!(svc.contains("[Service]"));
        assert!(svc.contains("ExecStart=/usr/local/bin/firestream-ci spans replay"));
        assert!(svc.contains("Environment=\"OTEL_EXPORTER_OTLP_ENDPOINT=http://localhost:4318\""));
        let timer = u.render_systemd_timer().unwrap();
        assert!(timer.contains("[Timer]"));
        assert!(timer.contains("OnUnitActiveSec=600s"));
    }

    #[test]
    fn ondemand_omits_timer() {
        let u = sample_unit();
        assert!(u.render_systemd_timer().is_none());
    }

    #[tokio::test]
    async fn install_launchd_writes_file_at_install_dir() {
        let tmp = tempdir().unwrap();
        let u = sample_unit()
            .with_install_dir(tmp.path())
            .with_schedule(ScheduleKind::Interval(Duration::from_secs(60)));
        let report = u.install_launchd().await.unwrap();
        assert!(matches!(report.kind, InstallKind::Launchd));
        assert_eq!(report.files.len(), 1);
        let body = std::fs::read_to_string(&report.files[0]).unwrap();
        assert!(body.contains("firestream-replay"));
    }

    #[tokio::test]
    async fn install_systemd_writes_service_and_timer() {
        let tmp = tempdir().unwrap();
        let u = sample_unit()
            .with_install_dir(tmp.path())
            .with_schedule(ScheduleKind::Interval(Duration::from_secs(60)));
        let report = u.install_systemd_user().await.unwrap();
        assert!(matches!(report.kind, InstallKind::SystemdUser));
        // Two files: .service + .timer
        assert_eq!(report.files.len(), 2);
        let service = std::fs::read_to_string(tmp.path().join("firestream-replay.service")).unwrap();
        assert!(service.contains("ExecStart"));
        let timer = std::fs::read_to_string(tmp.path().join("firestream-replay.timer")).unwrap();
        assert!(timer.contains("OnUnitActiveSec=60s"));
    }

    #[tokio::test]
    async fn install_systemd_ondemand_writes_service_only() {
        let tmp = tempdir().unwrap();
        let u = sample_unit().with_install_dir(tmp.path());
        let report = u.install_systemd_user().await.unwrap();
        assert_eq!(report.files.len(), 1);
        assert!(tmp.path().join("firestream-replay.service").exists());
        assert!(!tmp.path().join("firestream-replay.timer").exists());
    }

    #[test]
    fn xml_escape_handles_special_chars() {
        let u = ServiceUnit::new("svc", "<x & y>", "/bin/true").unwrap();
        let xml = u.render_launchd_plist();
        // Description doesn't go into the plist, but Label and command paths must
        // be escaped. Use a command path with a quote.
        let u2 = ServiceUnit::new("svc", "d", "/bin/with\"quote").unwrap();
        let xml2 = u2.render_launchd_plist();
        assert!(xml2.contains("/bin/with&quot;quote"));
        assert!(!xml.contains("<x & y>")); // not in template
    }
}
