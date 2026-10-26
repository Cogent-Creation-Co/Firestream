//! Run identity: one id per harness process, and the resource names built
//! from it.
//!
//! Every object an e2e harness creates carries a name of the form
//! `firestream-e2e-<target>-<id>`, where `<target>` is the chart or stack
//! (`postgresql`, `pg-backup`, `supabase`, …) and `<id>` is the run id: six
//! lowercase alphanumeric characters. This is the shape the k3d harness has
//! always used, so a sweeper that understands it also cleans clusters left by
//! older runs.
//!
//! The k3d harness derives docker object names from the cluster name
//! (`k3d-<name>-server-0`, `k3d-<name>-images`, network `k3d-<name>`); the
//! compose harness uses the name as the compose project
//! (`com.docker.compose.project=<name>`). Dev stacks use `firestream-<stack>`
//! and never carry the `firestream-e2e-` prefix.
//!
//! Set `FIRESTREAM_E2E_RUN_ID=<id>` to pin the id (for example so a wrapper
//! script knows which `--run` to sweep after a crash). An invalid value is
//! ignored and a random id is used instead.

use std::fmt;
use std::sync::OnceLock;

use anyhow::{Result, bail};
use rand::Rng;
use rand::distributions::Alphanumeric;

/// Prefix shared by every harness-owned name.
pub const PREFIX: &str = "firestream-e2e-";

/// Prefix k3d adds to cluster names for containers, networks, volumes and
/// kubeconfig contexts.
pub const K3D_PREFIX: &str = "k3d-firestream-e2e-";

/// Env var that pins [`RunId::current`].
pub const RUN_ID_ENV: &str = "FIRESTREAM_E2E_RUN_ID";

/// Length of a run id.
pub const RUN_ID_LEN: usize = 6;

/// A validated run id: [`RUN_ID_LEN`] characters from `[a-z0-9]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RunId(String);

impl RunId {
    /// The id of this process. The first call fixes it (from
    /// [`RUN_ID_ENV`] if valid, else random); every later call returns the
    /// same value.
    pub fn current() -> &'static RunId {
        static CURRENT: OnceLock<RunId> = OnceLock::new();
        CURRENT.get_or_init(|| {
            std::env::var(RUN_ID_ENV)
                .ok()
                .and_then(|v| RunId::parse(&v).ok())
                .unwrap_or_else(RunId::random)
        })
    }

    /// A fresh random id. Use [`RunId::current`] in harness code.
    pub fn random() -> RunId {
        let mut rng = rand::thread_rng();
        let id = (0..RUN_ID_LEN)
            .map(|_| (rng.sample(Alphanumeric) as char).to_ascii_lowercase())
            .collect();
        RunId(id)
    }

    /// Validate `s` as a run id.
    pub fn parse(s: &str) -> Result<RunId> {
        if s.len() != RUN_ID_LEN || !s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()) {
            bail!("invalid run id {s:?}: expected {RUN_ID_LEN} characters from [a-z0-9]");
        }
        Ok(RunId(s.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `firestream-e2e-<target>-<id>`: the k3d cluster name or compose
    /// project name for `target` in this run.
    ///
    /// # Panics
    ///
    /// Panics if `target` is not a valid target (see [`is_valid_target`]).
    /// Targets are compile-time chart and stack names, so a bad one is a
    /// programming error.
    pub fn resource_name(&self, target: &str) -> String {
        assert!(
            is_valid_target(target),
            "invalid e2e target {target:?}: expected [a-z0-9-], not starting or ending with '-'"
        );
        format!("{PREFIX}{target}-{}", self.0)
    }
}

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// [`RunId::resource_name`] for the current process.
pub fn resource_name(target: &str) -> String {
    RunId::current().resource_name(target)
}

/// A target is non-empty `[a-z0-9-]` that does not start or end with `-`.
/// It may contain `-` (`pg-backup`).
pub fn is_valid_target(target: &str) -> bool {
    !target.is_empty()
        && !target.starts_with('-')
        && !target.ends_with('-')
        && target
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Split `firestream-e2e-<target>-<id>` into `(target, id)`. Returns `None`
/// for any name that does not have exactly this shape.
pub fn parse_resource_name(name: &str) -> Option<(&str, RunId)> {
    let rest = name.strip_prefix(PREFIX)?;
    let (target, id) = rest.rsplit_once('-')?;
    if !is_valid_target(target) {
        return None;
    }
    Some((target, RunId::parse(id).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_is_stable_and_valid() {
        let a = RunId::current().clone();
        let b = RunId::current().clone();
        assert_eq!(a, b);
        assert!(RunId::parse(a.as_str()).is_ok());
    }

    #[test]
    fn random_ids_are_valid() {
        for _ in 0..100 {
            let id = RunId::random();
            assert!(RunId::parse(id.as_str()).is_ok(), "{id}");
        }
    }

    #[test]
    fn parse_rejects_bad_ids() {
        for bad in ["", "abc", "ABCDEF", "abc-de", "abcdefg", "abc_de"] {
            assert!(RunId::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn resource_name_matches_k3d_shape() {
        let id = RunId::parse("rkpzmb").unwrap();
        assert_eq!(id.resource_name("supabase"), "firestream-e2e-supabase-rkpzmb");
        assert_eq!(id.resource_name("pg-backup"), "firestream-e2e-pg-backup-rkpzmb");
    }

    #[test]
    #[should_panic(expected = "invalid e2e target")]
    fn resource_name_rejects_bad_target() {
        RunId::parse("abc123").unwrap().resource_name("Bad_Target");
    }

    #[test]
    fn parse_resource_name_round_trips() {
        let (t, id) = parse_resource_name("firestream-e2e-pg-backup-5u750j").unwrap();
        assert_eq!(t, "pg-backup");
        assert_eq!(id.as_str(), "5u750j");
        assert!(parse_resource_name("firestream-postgresql").is_none());
        assert!(parse_resource_name("firestream-e2e-postgresql").is_none());
        assert!(parse_resource_name("firestream-e2e--abc123").is_none());
        assert!(parse_resource_name("conceptdb-odoo-e2e").is_none());
    }
}
