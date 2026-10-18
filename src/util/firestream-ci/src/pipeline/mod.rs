//! Pattern #11 from the plan: Phase DAG with `Tier::{Required, Advisory}`,
//! parallel-within-phase, verdict aggregation. Deliberate upgrade over the
//! sequential numbered phases in the bash. Tier classifier is injectable.
//!
//! ## Topology
//!
//! A pipeline is a DAG of phases. Each phase carries a [`Tier`] (Required or
//! Advisory) and may declare `depends_on` edges to other phases by name.
//! Phases run in topological order; within a phase the caller-supplied
//! closure returns a `Vec<Task>` that runs in parallel via
//! `futures::future::join_all`.
//!
//! ## Verdict
//!
//! After every phase has run (or been skipped because an upstream Required
//! failed), a [`Verdict`] aggregates outcomes. Exit code is:
//!
//! - `0` Passed — every Required + Advisory task succeeded.
//! - `1` Failed — at least one Required task failed.
//! - `2` PartiallyPassed — every Required passed but at least one Advisory
//!   failed.
//!
//! The verdict can be rendered as markdown tables (`phase_table`,
//! `build_table`) or a full summary file via `write_markdown_summary`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

use comfy_table::{ContentArrangement, Table, presets};
use futures::future::BoxFuture;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("pipeline: cycle detected involving phases: {0:?}")]
    Cycle(Vec<String>),

    #[error("pipeline: phase `{phase}` depends on unknown phase `{missing}`")]
    UnknownDep { phase: String, missing: String },

    #[error("pipeline: duplicate phase name `{0}`")]
    DuplicatePhase(String),

    #[error("pipeline: no phases declared")]
    Empty,
}

/// Required (counts toward Failed) vs Advisory (counts toward PartiallyPassed
/// only, never escalates to Failed).
///
/// Serde-enabled (lowercase) because the Phase 4 CI profile
/// (`ci-manifest.json`, see `crate::profile`) carries phase tiers and
/// tier-classification rules as data: `"tier": "required"` / `"advisory"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    Required,
    Advisory,
}

impl Tier {
    /// Lowercase label, matching the profile's JSON spelling.
    pub fn label(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Advisory => "advisory",
        }
    }
}

/// A leaf task in a phase. Created via `Task::new`; the closure returns
/// `Ok(())` on success or `Err(message)` on failure.
pub struct Task {
    name: String,
    fut: BoxFuture<'static, Result<(), String>>,
}

impl Task {
    /// Build a task from any future returning `Result<(), String>`.
    pub fn new<F>(name: impl Into<String>, fut: F) -> Self
    where
        F: std::future::Future<Output = Result<(), String>> + Send + 'static,
    {
        Self {
            name: name.into(),
            fut: Box::pin(fut),
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}

/// One phase in the DAG.
pub struct Phase {
    name: String,
    tier: Tier,
    depends_on: Vec<String>,
    // Boxed FnOnce that materializes the parallel task set when the phase
    // runs. The closure captures whatever context the caller needs (config,
    // tracer, etc.).
    builder: Option<Box<dyn FnOnce() -> Vec<Task> + Send>>,
}

impl Phase {
    pub fn new(name: impl Into<String>, tier: Tier) -> Self {
        Self {
            name: name.into(),
            tier,
            depends_on: Vec::new(),
            builder: None,
        }
    }

    pub fn depends_on(mut self, other: impl Into<String>) -> Self {
        self.depends_on.push(other.into());
        self
    }

    /// Provide a closure that, when called, materializes the parallel tasks
    /// for this phase. The closure runs at phase-start time.
    pub fn parallel<F>(mut self, build: F) -> Self
    where
        F: FnOnce() -> Vec<Task> + Send + 'static,
    {
        self.builder = Some(Box::new(build));
        self
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tier(&self) -> Tier {
        self.tier
    }
}

/// Lifecycle callbacks emitted during a [`Pipeline::run`]. Lets callers
/// surface per-phase / per-task progress as the run unfolds instead of
/// waiting for the post-run [`Verdict`] tables. Default reporter is a
/// no-op; opt in via [`PipelineBuilder::reporter`].
pub trait Reporter: Send + Sync {
    /// Called once per phase, before any task runs. `task_names` is the
    /// full list of task names that will execute in this phase, in the
    /// order they were declared. Implementations should use this to
    /// pre-size live state (placeholder rows, column widths) so the
    /// rendered area stays geometrically stable across the phase — once
    /// established, `task_start`/`task_finish` only flip state and never
    /// change row count or column width. This is the contract that keeps
    /// the dashboard's `superconsole` redraws from leaking frames into
    /// scrollback when wider task names register mid-phase.
    fn phase_start(&self, phase: &str, tier: Tier, task_names: &[&str]);
    fn task_start(&self, phase: &str, task: &str);
    fn task_finish(
        &self,
        phase: &str,
        task: &str,
        ok: bool,
        duration: Duration,
        error: Option<&str>,
    );
    fn phase_finish(&self, phase: &str, ok: bool, duration: Duration);
    /// Called when a phase is skipped because an upstream Required phase
    /// failed. Implementations may print a one-line notice.
    fn phase_skipped(&self, phase: &str, tier: Tier) {
        let _ = (phase, tier);
    }
}

/// No-op reporter installed by default so existing tests and callers that
/// don't opt in keep their original silent behavior.
struct NoopReporter;

impl Reporter for NoopReporter {
    fn phase_start(&self, _phase: &str, _tier: Tier, _task_names: &[&str]) {}
    fn task_start(&self, _phase: &str, _task: &str) {}
    fn task_finish(
        &self,
        _phase: &str,
        _task: &str,
        _ok: bool,
        _duration: Duration,
        _error: Option<&str>,
    ) {
    }
    fn phase_finish(&self, _phase: &str, _ok: bool, _duration: Duration) {}
}

/// Per-task outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOutcome {
    pub name: String,
    pub ok: bool,
    pub error: Option<String>,
    pub duration: Duration,
}

/// Per-phase outcome.
#[derive(Debug, Clone)]
pub struct PhaseOutcome {
    pub name: String,
    pub tier: Tier,
    pub tasks: Vec<TaskOutcome>,
    pub duration: Duration,
    /// True when the phase was skipped because an upstream Required failed.
    pub skipped: bool,
}

impl PhaseOutcome {
    pub fn ok(&self) -> bool {
        !self.skipped && self.tasks.iter().all(|t| t.ok)
    }
}

/// Aggregate outcome of a [`Pipeline::run`].
#[derive(Debug, Clone)]
pub struct Verdict {
    pub overall: VerdictOutcome,
    pub phases: Vec<PhaseOutcome>,
    /// Span ids the pipeline emitted. Reserved for future wiring with the
    /// `trace` module; left empty by default for now so callers can plumb
    /// real ids without breaking the type.
    pub spans: Vec<String>,
}

/// Verdict outcome — see [`Verdict::exit_code`] for the mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerdictOutcome {
    Passed,
    Failed,
    PartiallyPassed,
}

impl Verdict {
    /// `0` Passed, `1` Failed (any Required failed), `2` PartiallyPassed
    /// (only Advisory failed).
    pub fn exit_code(&self) -> i32 {
        match self.overall {
            VerdictOutcome::Passed => 0,
            VerdictOutcome::Failed => 1,
            VerdictOutcome::PartiallyPassed => 2,
        }
    }

    /// Unified, colorless summary. Equivalent to
    /// [`summary_block_styled(false)`](Self::summary_block_styled) — used by
    /// the markdown writer and anywhere ANSI would be noise (log files).
    pub fn summary_block(&self) -> String {
        self.summary_block_styled(false)
    }

    /// The verdict block, in four parts:
    ///
    /// 1. A one-line **banner** that states the verdict up front and names the
    ///    failing phases — the one thing the user is looking for, so it leads
    ///    rather than hides at the bottom.
    /// 2. A tight **table** of phases (with indented tasks) carrying only
    ///    short, alignable values: a status glyph + word and a duration. Runs
    ///    of skipped phases collapse to a single row so a cascade of upstream
    ///    skips doesn't bury the failure.
    /// 3. The **`total:`** line (wall-clock, task-sum, achieved parallelism).
    /// 4. A full-width **detail block** per failing task. This is where error
    ///    text lives — never inside a table cell, where `comfy-table` would
    ///    collapse a multi-line stderr dump into a 1-char column.
    ///
    /// `color` injects ANSI only into the banner, the verdict word, and the
    /// detail-block headers — never inside the table (escape codes would
    /// break `comfy-table`'s width math). Callers pass `false` for any
    /// non-terminal sink.
    pub fn summary_block_styled(&self, color: bool) -> String {
        let mut out = String::new();

        // ---- 1. Banner ----------------------------------------------------
        let failed_phases: Vec<&str> = self
            .phases
            .iter()
            .filter(|p| !p.skipped && !p.ok())
            .map(|p| p.name.as_str())
            .collect();
        let n_phases = self.phases.len();
        let banner = match self.overall {
            VerdictOutcome::Failed => paint(
                &format!(
                    "━━━ CI FAILED ━━━  {} ({} of {} phase{} failed)",
                    failed_phases.join(", "),
                    failed_phases.len(),
                    n_phases,
                    if n_phases == 1 { "" } else { "s" },
                ),
                "1;31",
                color,
            ),
            VerdictOutcome::PartiallyPassed => paint(
                &format!(
                    "━━━ CI PARTIAL ━━━  advisory failure{} ({})",
                    if failed_phases.len() == 1 { "" } else { "s" },
                    failed_phases.join(", "),
                ),
                "1;33",
                color,
            ),
            VerdictOutcome::Passed => paint(
                &format!("━━━ CI PASSED ━━━  all {n_phases} phases green"),
                "1;32",
                color,
            ),
        };
        out.push_str(&banner);
        out.push_str("\n\n");

        // ---- 2. Table -----------------------------------------------------
        let mut t = Table::new();
        t.load_preset(presets::UTF8_BORDERS_ONLY);
        t.set_content_arrangement(ContentArrangement::Dynamic);
        t.set_header(vec!["Phase / Task", "Status", "Duration"]);

        let mut i = 0;
        while i < self.phases.len() {
            let p = &self.phases[i];
            if p.skipped {
                // Collapse a run of consecutive skipped phases into one row;
                // they carry no tasks and no signal individually.
                let mut j = i;
                while j < self.phases.len() && self.phases[j].skipped {
                    j += 1;
                }
                let run = &self.phases[i..j];
                if run.len() == 1 {
                    t.add_row(vec![run[0].name.clone(), glyph_label(Status::Skip), dash()]);
                } else {
                    let names: Vec<&str> = run.iter().map(|p| p.name.as_str()).collect();
                    t.add_row(vec![
                        format!("{} phases skipped ({})", run.len(), names.join(", ")),
                        glyph_label(Status::Skip),
                        dash(),
                    ]);
                }
                i = j;
                continue;
            }

            let status = if p.ok() { Status::Pass } else { Status::Fail };
            t.add_row(vec![
                p.name.clone(),
                glyph_label(status),
                format_duration(p.duration),
            ]);
            for task in &p.tasks {
                let status = if task.ok { Status::Pass } else { Status::Fail };
                t.add_row(vec![
                    format!("  {}", display_task_name(&task.name)),
                    glyph_label(status),
                    format_duration(task.duration),
                ]);
            }
            i += 1;
        }
        out.push_str(&format!("{t}"));
        out.push('\n');

        // ---- 3. total line ------------------------------------------------
        // Wall = sum of phase durations (phases run sequentially in the DAG;
        // tasks within a phase run in parallel). Task-sum is the serial cost.
        // Ratio is the achieved parallelism — one honest number telling the
        // user whether the fan-out paid off.
        let wall: Duration = self.phases.iter().map(|p| p.duration).sum();
        let task_sum: Duration = self
            .phases
            .iter()
            .flat_map(|p| p.tasks.iter())
            .map(|t| t.duration)
            .sum();
        let speedup = if wall.as_secs_f64() > 0.0 {
            task_sum.as_secs_f64() / wall.as_secs_f64()
        } else {
            0.0
        };
        let (verb, sgr) = match self.overall {
            VerdictOutcome::Passed => ("passed", "32"),
            VerdictOutcome::Failed => ("failed", "31"),
            VerdictOutcome::PartiallyPassed => ("partial", "33"),
        };
        out.push_str(&format!(
            "total: {} · {} wall · {} task-sum · {:.1}× parallel",
            paint(verb, sgr, color),
            format_duration(wall),
            format_duration(task_sum),
            speedup
        ));

        // ---- 4. Failure detail blocks ------------------------------------
        // Full-width, free-flowing — the natural home for multi-line stderr.
        for p in &self.phases {
            for task in &p.tasks {
                let Some(err) = task.error.as_deref() else {
                    continue;
                };
                out.push_str("\n\n");
                out.push_str(&paint(
                    &format!(
                        "✗ {} › {}  ({})",
                        p.name,
                        display_task_name(&task.name),
                        format_duration(task.duration),
                    ),
                    "1;31",
                    color,
                ));
                for line in err.lines() {
                    out.push_str("\n  ");
                    out.push_str(line);
                }
            }
        }

        out
    }

    /// Write a full markdown summary to `path`. Mirrors the unified live
    /// summary so the on-disk file and the terminal verdict carry the
    /// same shape.
    pub fn write_markdown_summary(&self, path: &Path) -> std::io::Result<()> {
        let header = match self.overall {
            VerdictOutcome::Passed => "# Pipeline Verdict: PASSED\n\n",
            VerdictOutcome::Failed => "# Pipeline Verdict: FAILED\n\n",
            VerdictOutcome::PartiallyPassed => "# Pipeline Verdict: PARTIAL\n\n",
        };
        let mut out = String::new();
        out.push_str(header);
        out.push_str(&self.summary_block());
        out.push('\n');
        std::fs::write(path, out)
    }
}

/// Row status, rendered as a glyph + short word. The glyphs match the live
/// dashboard (`✓`/`✗`/`·`) so the scrollback summary reads the same as the
/// view the user was just watching.
#[derive(Clone, Copy)]
enum Status {
    Pass,
    Fail,
    Skip,
}

/// `✓ pass` / `✗ FAIL` / `· skip`. The failing label is upper-cased so the
/// one row that matters is legible at a glance even without color (the table
/// is deliberately colorless — see `summary_block_styled`).
fn glyph_label(s: Status) -> String {
    match s {
        Status::Pass => "✓ pass".into(),
        Status::Fail => "✗ FAIL".into(),
        Status::Skip => "· skip".into(),
    }
}

/// An em-dash stands in for a zero/elided duration so skipped rows don't show
/// a misleading `0.00s`.
fn dash() -> String {
    "—".into()
}

/// Wrap `s` in an ANSI SGR sequence when `on`; otherwise return it unchanged.
/// Used only outside the table (banner / verdict word / detail headers) so it
/// never interferes with `comfy-table`'s display-width accounting.
fn paint(s: &str, sgr: &str, on: bool) -> String {
    if on {
        format!("\x1b[{sgr}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

/// Strip the redundant `required-` / `advisory-` prefix from a task's
/// display name. The on-disk leaf (used by per-task log files) keeps the
/// full string; only rendered cells shorten so the table doesn't waste
/// 9–10 columns repeating tier info that's implicit in the phase.
pub fn display_task_name(full: &str) -> &str {
    full.strip_prefix("required-")
        .or_else(|| full.strip_prefix("advisory-"))
        .unwrap_or(full)
}

/// Compact human-friendly duration: `4.21s`, `38.14s`, `12m04s`, `1h02m`.
/// Sub-minute durations carry two decimals so a fleet of fast tasks (e.g.
/// three WASM builds finishing within 100ms of each other) don't collapse
/// onto the same rounded reading.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let millis = d.subsec_millis();
    if secs < 60 {
        format!("{}.{:02}s", secs, millis / 10)
    } else if secs < 3600 {
        format!("{}m{:02}s", secs / 60, secs % 60)
    } else {
        format!("{}h{:02}m", secs / 3600, (secs % 3600) / 60)
    }
}

/// Pipeline builder. Phases are added via `.phase(...)`; `.run()` executes
/// the DAG and returns a [`Verdict`].
pub struct Pipeline {
    phases: Vec<Phase>,
    tier_classifier: Option<Arc<dyn Fn(&str) -> Tier + Send + Sync>>,
    reporter: Arc<dyn Reporter>,
}

impl Pipeline {
    pub fn builder() -> PipelineBuilder {
        PipelineBuilder::default()
    }

    /// Execute the pipeline. Returns a [`Verdict`] regardless of phase
    /// outcomes; only DAG-validation failures produce an `Err`.
    pub async fn run(self) -> Result<Verdict, Error> {
        if self.phases.is_empty() {
            return Err(Error::Empty);
        }

        let order = topological_sort(&self.phases)?;

        // Map phase-name → outcome so dependents can check upstream status.
        let mut outcomes: HashMap<String, PhaseOutcome> = HashMap::new();
        let mut by_name: BTreeMap<String, Phase> = self
            .phases
            .into_iter()
            .map(|p| (p.name.clone(), p))
            .collect();

        for name in &order {
            let mut phase = by_name.remove(name).expect("name in order");

            // Skip if any upstream Required failed.
            let upstream_required_failed = phase.depends_on.iter().any(|dep| {
                outcomes
                    .get(dep)
                    .map(|o| matches!(o.tier, Tier::Required) && !o.ok())
                    .unwrap_or(false)
            });

            if upstream_required_failed {
                self.reporter.phase_skipped(name, phase.tier);
                outcomes.insert(
                    name.clone(),
                    PhaseOutcome {
                        name: name.clone(),
                        tier: phase.tier,
                        tasks: Vec::new(),
                        duration: Duration::ZERO,
                        skipped: true,
                    },
                );
                continue;
            }

            let tasks = phase.builder.take().map(|b| b()).unwrap_or_default();

            let task_names: Vec<&str> = tasks.iter().map(|t| t.name.as_str()).collect();
            self.reporter.phase_start(name, phase.tier, &task_names);

            let phase_start = Instant::now();
            let mut task_outcomes: Vec<TaskOutcome> = Vec::with_capacity(tasks.len());

            // Run tasks in parallel.
            let phase_for_tasks: Arc<str> = Arc::from(name.as_str());
            let task_handles: Vec<_> = tasks
                .into_iter()
                .map(|t| {
                    let task_name = t.name.clone();
                    let fut: Pin<Box<_>> = t.fut;
                    let reporter = Arc::clone(&self.reporter);
                    let phase_name = Arc::clone(&phase_for_tasks);
                    async move {
                        reporter.task_start(&phase_name, &task_name);
                        let start = Instant::now();
                        let res = fut.await;
                        let duration = start.elapsed();
                        let ok = res.is_ok();
                        let err_msg = res.as_ref().err().cloned();
                        reporter.task_finish(
                            &phase_name,
                            &task_name,
                            ok,
                            duration,
                            err_msg.as_deref(),
                        );
                        TaskOutcome {
                            name: task_name,
                            ok,
                            error: res.err(),
                            duration,
                        }
                    }
                })
                .collect();
            let results = futures::future::join_all(task_handles).await;
            task_outcomes.extend(results);

            let phase_duration = phase_start.elapsed();
            let phase_ok = task_outcomes.iter().all(|t| t.ok);
            self.reporter.phase_finish(name, phase_ok, phase_duration);

            outcomes.insert(
                name.clone(),
                PhaseOutcome {
                    name: name.clone(),
                    tier: phase.tier,
                    tasks: task_outcomes,
                    duration: phase_duration,
                    skipped: false,
                },
            );
        }

        // Assemble final verdict in deterministic order.
        let phases: Vec<PhaseOutcome> = order
            .iter()
            .map(|n| outcomes.remove(n).expect("outcome recorded"))
            .collect();

        let mut required_failed = false;
        let mut advisory_failed = false;
        for p in &phases {
            if p.skipped {
                continue;
            }
            let failed = !p.ok();
            if failed {
                match p.tier {
                    Tier::Required => required_failed = true,
                    Tier::Advisory => advisory_failed = true,
                }
            }
        }

        let overall = if required_failed {
            VerdictOutcome::Failed
        } else if advisory_failed {
            VerdictOutcome::PartiallyPassed
        } else {
            VerdictOutcome::Passed
        };

        Ok(Verdict {
            overall,
            phases,
            spans: Vec::new(),
        })
    }

    /// Access the optional tier classifier. Callers building [`Phase`] from
    /// an attribute name use this to default the tier.
    pub fn tier_for(&self, attr: &str) -> Option<Tier> {
        self.tier_classifier.as_ref().map(|f| f(attr))
    }
}

#[derive(Default)]
pub struct PipelineBuilder {
    phases: Vec<Phase>,
    tier_classifier: Option<Arc<dyn Fn(&str) -> Tier + Send + Sync>>,
    reporter: Option<Arc<dyn Reporter>>,
}

impl PipelineBuilder {
    pub fn phase(mut self, p: Phase) -> Self {
        self.phases.push(p);
        self
    }

    /// Inject a callback that classifies attribute names into a [`Tier`].
    /// The CI profile provides one; external consumers may
    /// supply their own.
    pub fn tier_classifier<F>(mut self, f: F) -> Self
    where
        F: Fn(&str) -> Tier + Send + Sync + 'static,
    {
        self.tier_classifier = Some(Arc::new(f));
        self
    }

    /// Install a [`Reporter`] for per-phase / per-task lifecycle events.
    /// Default is a no-op; pass [`Arc::new(BannerReporter::new())`] (from
    /// the binary crate) to get the ASCII progress output.
    pub fn reporter(mut self, r: Arc<dyn Reporter>) -> Self {
        self.reporter = Some(r);
        self
    }

    pub fn build(self) -> Result<Pipeline, Error> {
        // Detect duplicates up-front so cycle-detection doesn't have to.
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for p in &self.phases {
            if !seen.insert(p.name.as_str()) {
                return Err(Error::DuplicatePhase(p.name.clone()));
            }
        }
        Ok(Pipeline {
            phases: self.phases,
            tier_classifier: self.tier_classifier,
            reporter: self.reporter.unwrap_or_else(|| Arc::new(NoopReporter)),
        })
    }

    /// Shorthand for `build().run().await`. Returns the [`Verdict`].
    pub async fn run(self) -> Result<Verdict, Error> {
        self.build()?.run().await
    }
}

/// Kahn's algorithm: returns phases in dependency-respecting order, or
/// errors on cycles / unknown deps.
fn topological_sort(phases: &[Phase]) -> Result<Vec<String>, Error> {
    use std::collections::VecDeque;

    let names: BTreeSet<&str> = phases.iter().map(|p| p.name.as_str()).collect();

    // in_degree counts unresolved deps per phase.
    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut adj: HashMap<String, Vec<String>> = HashMap::new();
    for p in phases {
        in_degree.entry(p.name.clone()).or_insert(0);
        for dep in &p.depends_on {
            if !names.contains(dep.as_str()) {
                return Err(Error::UnknownDep {
                    phase: p.name.clone(),
                    missing: dep.clone(),
                });
            }
            *in_degree.entry(p.name.clone()).or_insert(0) += 1;
            adj.entry(dep.clone()).or_default().push(p.name.clone());
        }
    }

    // Stable seed: iterate phases in declaration order so the topo output
    // is deterministic for sibling phases.
    let mut queue: VecDeque<String> = phases
        .iter()
        .filter(|p| in_degree.get(&p.name).copied().unwrap_or(0) == 0)
        .map(|p| p.name.clone())
        .collect();

    let mut out = Vec::with_capacity(phases.len());
    while let Some(n) = queue.pop_front() {
        out.push(n.clone());
        if let Some(children) = adj.get(&n) {
            for c in children {
                let entry = in_degree.entry(c.clone()).or_insert(0);
                *entry = entry.saturating_sub(1);
                if *entry == 0 {
                    queue.push_back(c.clone());
                }
            }
        }
    }

    if out.len() != phases.len() {
        // Anything still with in_degree > 0 is part of a cycle.
        let stuck: Vec<String> = in_degree
            .iter()
            .filter_map(|(n, d)| if *d > 0 { Some(n.clone()) } else { None })
            .collect();
        return Err(Error::Cycle(stuck));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok_task(name: &str) -> Task {
        Task::new(name, async { Ok(()) })
    }
    fn fail_task(name: &str, err: &str) -> Task {
        let err = err.to_string();
        Task::new(name, async move { Err(err) })
    }

    #[tokio::test]
    async fn empty_pipeline_errors() {
        let r = Pipeline::builder().build().unwrap().run().await;
        assert!(matches!(r, Err(Error::Empty)));
    }

    #[tokio::test]
    async fn single_phase_all_ok_passes() {
        let v = Pipeline::builder()
            .phase(Phase::new("p1", Tier::Required).parallel(|| vec![ok_task("t1"), ok_task("t2")]))
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();
        assert_eq!(v.overall, VerdictOutcome::Passed);
        assert_eq!(v.exit_code(), 0);
        assert_eq!(v.phases.len(), 1);
        assert_eq!(v.phases[0].tasks.len(), 2);
    }

    #[tokio::test]
    async fn required_failure_yields_failed() {
        let v = Pipeline::builder()
            .phase(
                Phase::new("p1", Tier::Required)
                    .parallel(|| vec![ok_task("ok"), fail_task("bad", "boom")]),
            )
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();
        assert_eq!(v.overall, VerdictOutcome::Failed);
        assert_eq!(v.exit_code(), 1);
    }

    #[tokio::test]
    async fn only_advisory_failure_yields_partial() {
        let v = Pipeline::builder()
            .phase(Phase::new("req", Tier::Required).parallel(|| vec![ok_task("ok")]))
            .phase(
                Phase::new("adv", Tier::Advisory)
                    .depends_on("req")
                    .parallel(|| vec![fail_task("warn", "soft")]),
            )
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();
        assert_eq!(v.overall, VerdictOutcome::PartiallyPassed);
        assert_eq!(v.exit_code(), 2);
    }

    #[tokio::test]
    async fn downstream_skipped_when_required_upstream_fails() {
        let v = Pipeline::builder()
            .phase(Phase::new("a", Tier::Required).parallel(|| vec![fail_task("x", "no")]))
            .phase(
                Phase::new("b", Tier::Required)
                    .depends_on("a")
                    .parallel(|| vec![ok_task("y")]),
            )
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();
        let b = v.phases.iter().find(|p| p.name == "b").unwrap();
        assert!(b.skipped, "phase b must be skipped");
        assert_eq!(v.overall, VerdictOutcome::Failed);
    }

    #[tokio::test]
    async fn cycle_detected() {
        let r = Pipeline::builder()
            .phase(Phase::new("a", Tier::Required).depends_on("b"))
            .phase(Phase::new("b", Tier::Required).depends_on("a"))
            .build()
            .unwrap()
            .run()
            .await;
        assert!(matches!(r, Err(Error::Cycle(_))));
    }

    #[tokio::test]
    async fn unknown_dep_errors() {
        let r = Pipeline::builder()
            .phase(Phase::new("a", Tier::Required).depends_on("ghost"))
            .build()
            .unwrap()
            .run()
            .await;
        assert!(matches!(r, Err(Error::UnknownDep { .. })));
    }

    #[tokio::test]
    async fn duplicate_phase_rejected() {
        let r = Pipeline::builder()
            .phase(Phase::new("a", Tier::Required))
            .phase(Phase::new("a", Tier::Advisory))
            .build();
        assert!(matches!(r, Err(Error::DuplicatePhase(_))));
    }

    #[tokio::test]
    async fn summary_block_contains_phase_and_task_names() {
        let v = Pipeline::builder()
            .phase(Phase::new("p1", Tier::Required).parallel(|| vec![ok_task("t")]))
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();
        let s = v.summary_block();
        assert!(s.contains("p1"), "phase name in summary: {s}");
        assert!(s.contains("t"), "task name in summary: {s}");
        assert!(s.contains("passed"), "outcome verb in summary: {s}");
        assert!(
            s.starts_with("┌") || s.contains("│"),
            "UTF8 borders preset: {s}"
        );
        assert!(s.contains("total:"), "footer line in summary: {s}");
    }

    #[test]
    fn tier_classifier_callback_works() {
        let p = Pipeline::builder()
            .tier_classifier(|s: &str| {
                if s.starts_with("required-") {
                    Tier::Required
                } else {
                    Tier::Advisory
                }
            })
            .phase(Phase::new("seed", Tier::Required))
            .build()
            .unwrap();
        assert_eq!(p.tier_for("required-rust-fmt"), Some(Tier::Required));
        assert_eq!(p.tier_for("advisory-something"), Some(Tier::Advisory));
    }

    /// Reporter trait fires phase_start/task_start/task_finish/phase_finish
    /// for a normal phase and phase_skipped for a phase blocked by an
    /// upstream Required failure.
    #[tokio::test]
    async fn reporter_lifecycle_events_fire() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct CaptureReporter {
            events: Mutex<Vec<String>>,
        }
        impl Reporter for CaptureReporter {
            fn phase_start(&self, phase: &str, _tier: Tier, names: &[&str]) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("phase_start:{phase}:{}", names.len()));
            }
            fn task_start(&self, phase: &str, task: &str) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("task_start:{phase}:{task}"));
            }
            fn task_finish(
                &self,
                phase: &str,
                task: &str,
                ok: bool,
                _duration: Duration,
                _error: Option<&str>,
            ) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("task_finish:{phase}:{task}:{ok}"));
            }
            fn phase_finish(&self, phase: &str, ok: bool, _duration: Duration) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("phase_finish:{phase}:{ok}"));
            }
            fn phase_skipped(&self, phase: &str, _tier: Tier) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("phase_skipped:{phase}"));
            }
        }

        let reporter = Arc::new(CaptureReporter::default());
        let _ = Pipeline::builder()
            .reporter(reporter.clone() as Arc<dyn Reporter>)
            .phase(
                Phase::new("a", Tier::Required)
                    .parallel(|| vec![ok_task("aok"), fail_task("abad", "boom")]),
            )
            .phase(
                Phase::new("b", Tier::Required)
                    .depends_on("a")
                    .parallel(|| vec![ok_task("by")]),
            )
            .build()
            .unwrap()
            .run()
            .await
            .unwrap();

        let ev = reporter.events.lock().unwrap().clone();
        assert!(
            ev.contains(&"phase_start:a:2".to_string()),
            "events: {ev:?}"
        );
        assert!(
            ev.contains(&"task_start:a:aok".to_string()),
            "events: {ev:?}"
        );
        assert!(
            ev.contains(&"task_finish:a:aok:true".to_string()),
            "events: {ev:?}"
        );
        assert!(
            ev.contains(&"task_finish:a:abad:false".to_string()),
            "events: {ev:?}"
        );
        assert!(
            ev.contains(&"phase_finish:a:false".to_string()),
            "events: {ev:?}"
        );
        assert!(
            ev.contains(&"phase_skipped:b".to_string()),
            "events: {ev:?}"
        );
        // phase b never starts (it was skipped).
        assert!(!ev.iter().any(|e| e == "phase_start:b:1"), "events: {ev:?}");
    }
}
