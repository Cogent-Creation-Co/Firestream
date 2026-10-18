//! Pattern #17 — typed env-var allowlist. Mechanism only: callers (or the
//! CI profile) supplies the var list. Mirrors ConceptDB's
//! `bin/_lib.sh::CI_PASSTHROUGH_VARS` / `build_docker_env_args` (lines
//! 343-363) — but only the loop shape, never the var names.
//!
//! Why a separate module: the same allowlist is consumed by three carriers
//! that each need a different render — Docker (`--env KEY=val`), systemd
//! (`Environment=KEY=val`), and process exec (k/v tuples). Splitting the
//! capture (`snapshot`) from the rendering (`to_*`) lets a single allowlist
//! drive all three without leaking carrier specifics back to the caller.

use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("passthrough: variable name `{0}` is invalid (must match [A-Za-z_][A-Za-z0-9_]*)")]
    InvalidName(String),
}

/// Caller-built allowlist of environment variable names. Order is preserved
/// (declaration order = render order), and duplicates are deduplicated.
#[derive(Debug, Clone, Default)]
pub struct EnvAllowlist {
    vars: Vec<String>,
}

impl EnvAllowlist {
    pub fn builder() -> EnvAllowlistBuilder {
        EnvAllowlistBuilder::default()
    }

    /// Names in allowlist order (declaration, deduplicated).
    pub fn names(&self) -> &[String] {
        &self.vars
    }

    /// Capture current values from a getenv-like callable. Missing or empty
    /// variables are skipped (matches the bash, which uses `[[ -n "${!var:-}" ]]`).
    pub fn snapshot<F>(&self, mut getenv: F) -> EnvSnapshot
    where
        F: FnMut(&str) -> Option<String>,
    {
        // BTreeMap rather than HashMap so the rendered output is deterministic
        // for tests + diffability in `docker create` invocations.
        let mut values = BTreeMap::new();
        for name in &self.vars {
            if let Some(v) = getenv(name) {
                if !v.is_empty() {
                    values.insert(name.clone(), v);
                }
            }
        }
        EnvSnapshot { values }
    }

    /// Convenience: snapshot from the process's own environment.
    pub fn snapshot_from_process(&self) -> EnvSnapshot {
        self.snapshot(|name| std::env::var(name).ok())
    }
}

/// Captured (name, value) pairs from an allowlist. Distinct type from
/// [`EnvAllowlist`] so it's impossible to confuse "what we want to pass"
/// with "what we actually captured".
#[derive(Debug, Clone, Default)]
pub struct EnvSnapshot {
    values: BTreeMap<String, String>,
}

impl EnvSnapshot {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.values.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }

    /// Render as `--env KEY=val` argument pairs for `docker create / run`.
    pub fn to_docker_args(&self) -> Vec<String> {
        let mut out = Vec::with_capacity(self.values.len() * 2);
        for (k, v) in &self.values {
            out.push("--env".to_string());
            out.push(format!("{k}={v}"));
        }
        out
    }

    /// Render as `Environment=KEY=val` lines for a systemd unit's
    /// `[Service]` block.
    pub fn to_systemd_env(&self) -> Vec<String> {
        self.values
            .iter()
            .map(|(k, v)| format!("Environment={k}={v}"))
            .collect()
    }

    /// Render as (KEY, VAL) tuples — the format expected by
    /// `Command::envs`.
    pub fn to_env_tuples(&self) -> Vec<(String, String)> {
        self.values
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }
}

#[derive(Debug, Default, Clone)]
pub struct EnvAllowlistBuilder {
    vars: Vec<String>,
    seen: std::collections::HashSet<String>,
}

impl EnvAllowlistBuilder {
    /// Append a name. Silently dedups (the bash declares its allowlist as a
    /// flat array with no dedup; we tighten this without breaking the spec).
    #[allow(clippy::should_implement_trait)]
    pub fn add(mut self, name: impl Into<String>) -> Result<Self, Error> {
        let n = name.into();
        validate_name(&n)?;
        if self.seen.insert(n.clone()) {
            self.vars.push(n);
        }
        Ok(self)
    }

    /// Convenience: append many at once.
    pub fn add_all<I, S>(mut self, names: I) -> Result<Self, Error>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        for n in names {
            self = self.add(n)?;
        }
        Ok(self)
    }

    pub fn build(self) -> EnvAllowlist {
        EnvAllowlist { vars: self.vars }
    }
}

/// Reject names that POSIX shells reject. The bash trusts its hardcoded
/// list; once the list comes from defaults/an external caller we have to
/// validate.
fn validate_name(name: &str) -> Result<(), Error> {
    let mut chars = name.chars();
    let first = chars
        .next()
        .ok_or_else(|| Error::InvalidName(name.into()))?;
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(Error::InvalidName(name.into()));
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(Error::InvalidName(name.into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn builder_dedups_preserving_first_position() {
        let a = EnvAllowlist::builder()
            .add("A")
            .unwrap()
            .add("B")
            .unwrap()
            .add("A")
            .unwrap()
            .build();
        assert_eq!(a.names(), &["A".to_string(), "B".to_string()]);
    }

    #[test]
    fn rejects_invalid_names() {
        assert!(EnvAllowlist::builder().add("1FOO").is_err());
        assert!(EnvAllowlist::builder().add("FOO-BAR").is_err());
        assert!(EnvAllowlist::builder().add("").is_err());
        assert!(EnvAllowlist::builder().add("FOO BAR").is_err());
        assert!(EnvAllowlist::builder().add("_OK").is_ok());
        assert!(EnvAllowlist::builder().add("OK_2").is_ok());
    }

    #[test]
    fn snapshot_skips_missing_and_empty() {
        let a = EnvAllowlist::builder()
            .add_all(["PRESENT", "MISSING", "EMPTY"])
            .unwrap()
            .build();
        let mut env: HashMap<&str, String> = HashMap::new();
        env.insert("PRESENT", "hello".into());
        env.insert("EMPTY", "".into());
        let snap = a.snapshot(|k| env.get(k).cloned());
        assert_eq!(snap.len(), 1);
        let pairs: Vec<_> = snap.iter().collect();
        assert_eq!(pairs, vec![("PRESENT", "hello")]);
    }

    #[test]
    fn renders_docker_args() {
        let a = EnvAllowlist::builder()
            .add_all(["FOO", "BAR"])
            .unwrap()
            .build();
        let snap = a.snapshot(|k| match k {
            "FOO" => Some("1".into()),
            "BAR" => Some("two".into()),
            _ => None,
        });
        // BTreeMap ordering → BAR before FOO alphabetically.
        assert_eq!(
            snap.to_docker_args(),
            vec![
                "--env".to_string(),
                "BAR=two".to_string(),
                "--env".to_string(),
                "FOO=1".to_string(),
            ]
        );
    }

    #[test]
    fn renders_systemd_env() {
        let a = EnvAllowlist::builder().add("FOO").unwrap().build();
        let snap = a.snapshot(|k| (k == "FOO").then(|| "bar".to_string()));
        assert_eq!(
            snap.to_systemd_env(),
            vec!["Environment=FOO=bar".to_string()]
        );
    }

    #[test]
    fn renders_env_tuples() {
        let a = EnvAllowlist::builder().add_all(["A", "B"]).unwrap().build();
        let snap = a.snapshot(|k| Some(format!("v_{k}")));
        let tuples = snap.to_env_tuples();
        assert!(tuples.contains(&("A".to_string(), "v_A".to_string())));
        assert!(tuples.contains(&("B".to_string(), "v_B".to_string())));
    }
}
