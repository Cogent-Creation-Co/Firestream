//! Live CI dashboard built on `superconsole`. Implements the `Reporter`
//! trait from `pipeline::mod`, so it slots in alongside the plain-text
//! `BannerReporter` and is selected at startup based on `std::io::IsTerminal`
//! and the `FIRESTREAM_CI_UI` env var.
//!
//! Scratch area (redrawn each tick): one line per phase header + one line
//! per task with a braille spinner, name, and elapsed timer. Tasks that
//! have finished stay rendered with `✓` / `✗` until the phase finishes,
//! at which point the phase summary is emitted to the scroll history.
//!
//! Emitted area (scrolls): on task failure, the last ~80 lines of the
//! per-task stderr log file are emitted as a labeled block so failure
//! context is durably in scroll history. Phase finish summaries are
//! emitted too.
//!
//! Tail-N preview of running tasks' live output is wired through the
//! `LineRing` type in `firestream-nix-build`: each task builder asks the
//! dashboard for a ring via `ring_for(phase, task)` and plumbs it into
//! `FastBuild::builder().ring_sink(...)`. The build's stderr tee loop
//! pushes lines into the ring; this module's `RootComponent` renders
//! the last K lines under each running task with a box-draw rule.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use firestream_nix_build::ring::LineRing;
use superconsole::style::{Color, StyledContent, Stylize};
use superconsole::{Component, DrawMode, Line, Lines, Span, SuperConsole};

use crate::pipeline::{Reporter, Tier, format_duration};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

const TICK_INTERVAL: Duration = Duration::from_millis(100);

/// Tail length emitted as a failure block when a task fails.
const FAILURE_TAIL_LINES: usize = 80;

/// How many tail lines to show under each running task in the live
/// preview. `LineRing` capacity is set one higher so a write can race
/// the render without losing the visible window.
const TAIL_PREVIEW_LINES: usize = 5;
const RING_CAPACITY: usize = TAIL_PREVIEW_LINES + 1;

/// Suppress per-task tail preview for phases with more than this many
/// total tasks. Each preview costs ~6 lines; with 9 verify checks in
/// flight the viewport clips and only ~3 tasks stay visible. Headers
/// alone are 1 line, so all tasks fit.
///
/// Earlier versions gated this on the *live* `running_count`, but that
/// caused the live area's height to oscillate as tasks finished: when
/// `running_count` crossed the threshold the previews abruptly turned
/// on/off and the prior frame's top lines leaked into scrollback (the
/// "stair-step of duplicate phase headers" bug). Gating on a STATIC
/// per-phase total (`PhaseEntry::task_count`, set once at `phase_start`)
/// keeps the live-area height monotonic and eliminates the leak. Phases
/// with one or two tasks (tidy, attest) still get tails; verify/build
/// don't.
const TAIL_PREVIEW_MAX_PHASE_TASKS: usize = 2;

// ──────────────────────────────────────────────────────────────────────
// Layout. One source of truth for column widths so the live block and
// the scrollback summary line up; previously three formatters each
// picked their own padding and the columns drifted depending on which
// formatter rendered the row.
// ──────────────────────────────────────────────────────────────────────
const PHASE_NAME_W: usize = 8;
const STATUS_W: usize = 6;
const TASK_NAME_W_MIN: usize = 12;
const DUR_W: usize = 8;

/// Tier-driven base color for a phase header. Tier is no longer rendered
/// as text in the live row — color carries the channel instead, so
/// required phases catch the eye while advisory ones recede.
fn phase_color(t: Tier) -> Color {
    match t {
        Tier::Required => Color::Cyan,
        Tier::Advisory => Color::DarkGrey,
    }
}

use crate::pipeline::display_task_name;

/// Render a phase header line. Shared by live (`RootComponent`) and
/// scrollback (`phase_finish`) so the row the user watched at the top
/// of the live block is byte-for-byte the row in scrollback when the
/// phase completes — modulo glyph/status.
fn fmt_phase_row(
    glyph: &str,
    phase: &str,
    status: &str,
    dur: &str,
    detail: Option<&str>,
) -> String {
    let base = format!(
        "{glyph} {phase:<phase_w$} {status:<status_w$} {dur:>dur_w$}",
        phase_w = PHASE_NAME_W,
        status_w = STATUS_W,
        dur_w = DUR_W,
    );
    match detail {
        Some(d) if !d.is_empty() => format!("{base}  ({d})"),
        _ => base,
    }
}

/// Render a task row. `name_w` is the active global task-name column
/// width; expands monotonically as wider task names register. `name` is
/// passed through `display_task_name` first so the rendered cell strips
/// the tier prefix when present.
fn fmt_task_row(glyph: &str, name: &str, dur: &str, suffix: &str, name_w: usize) -> String {
    let shown = display_task_name(name);
    format!(
        "  {glyph} {shown:<name_w$} {dur:>dur_w$}{suffix}",
        dur_w = DUR_W,
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskState {
    /// Placeholder row reserved at `phase_start` with the task's final
    /// name already populated. Two invariants keep the live scratch area
    /// from leaking into scrollback: (1) row count is fixed for the
    /// phase's lifetime — `task_start` flips state in-place rather than
    /// growing the vec; (2) `task_name_w` is pre-expanded at
    /// `phase_start` to cover every task in the phase, so per-row width
    /// also stays fixed. Together they make superconsole's
    /// `MoveUp(last_lines)` math exact across the phase, even on
    /// terminals where individual rows would otherwise cross a wrap
    /// boundary mid-run.
    Queued,
    Running,
    Passed,
    Failed,
}

/// Selects how `RootComponent` renders. `Live` is the per-tick scratch-area
/// view — animated spinner, per-task tail previews. `Final` is what gets
/// promoted into scroll history at shutdown: no spinner glyphs, no tail
/// previews, and finished phases are suppressed entirely so the persisted
/// last frame stays useful instead of freezing a mid-run snapshot.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Live,
    Final,
}

struct TaskEntry {
    name: String,
    started: Instant,
    state: TaskState,
    duration: Option<Duration>,
    error: Option<String>,
    /// Live tail of the task's stderr (last N lines). `None` for tasks
    /// whose builder didn't request one (e.g., shell-only tasks).
    ring: Option<Arc<LineRing>>,
}

struct PhaseEntry {
    name: String,
    tier: Tier,
    task_count: usize,
    started: Instant,
    tasks: Vec<TaskEntry>,
    finished: bool,
    /// Optional phase-specific detail (e.g. `tidy` gc stats) rendered in
    /// parens at the end of the phase header. Written by
    /// `DashboardReporter::set_phase_detail`.
    detail: Option<String>,
}

impl PhaseEntry {
    fn done_count(&self) -> usize {
        self.tasks
            .iter()
            .filter(|t| matches!(t.state, TaskState::Passed | TaskState::Failed))
            .count()
    }
}

struct DashboardState {
    phases: Vec<PhaseEntry>,
    frame: usize,
    /// Rings registered by task builders before the task fires (the builder
    /// closure runs at phase-start while `task_start` fires only when the
    /// task actually begins). Drained into `TaskEntry.ring` at `task_start`.
    pending_rings: HashMap<(String, String), Arc<LineRing>>,
    /// Global task-name column width. Starts at `TASK_NAME_W_MIN` and
    /// expands monotonically as wider task names register via
    /// `task_start`, so columns stay aligned across the whole run.
    task_name_w: usize,
}

impl DashboardState {
    fn new() -> Self {
        Self {
            phases: Vec::new(),
            frame: 0,
            pending_rings: HashMap::new(),
            task_name_w: TASK_NAME_W_MIN,
        }
    }

    fn phase_mut(&mut self, name: &str) -> Option<&mut PhaseEntry> {
        self.phases.iter_mut().find(|p| p.name == name)
    }
}

/// Component rendered each tick. Borrows a snapshot of state cheaply by
/// holding the lock during draw — draws are bounded by terminal height so
/// the critical section is short.
struct RootComponent<'a> {
    state: &'a Mutex<DashboardState>,
    mode: RenderMode,
}

impl<'a> Component for RootComponent<'a> {
    fn draw_unchecked(
        &self,
        _dimensions: superconsole::Dimensions,
        _mode: DrawMode,
    ) -> anyhow::Result<Lines> {
        let state = self.state.lock().expect("dashboard state poisoned");
        let frame = state.frame;
        let name_w = state.task_name_w;
        let mut out = Lines::new();

        for phase in &state.phases {
            // Skip phases that have been emitted to history already.
            if phase.finished {
                continue;
            }
            // In Final mode, also skip phases whose tasks have all finished:
            // their per-task PASS/FAIL summary will be (or already was) emitted
            // by `phase_finish`, and leaving them in the final frame would
            // duplicate that block.
            if self.mode == RenderMode::Final && phase.done_count() == phase.task_count {
                continue;
            }

            let glyph = match self.mode {
                RenderMode::Live => SPINNER_FRAMES[frame % SPINNER_FRAMES.len()],
                // Frozen marker so the persisted frame doesn't lie about
                // still-spinning work.
                RenderMode::Final => "…",
            };
            let status = format!("{}/{}", phase.done_count(), phase.task_count);
            let elapsed = format_duration(phase.started.elapsed());
            let header = fmt_phase_row(
                glyph,
                &phase.name,
                &status,
                &elapsed,
                phase.detail.as_deref(),
            );
            out.push(line_styled(header.with(phase_color(phase.tier)).bold()));

            // Tail previews are noise in the final frame — they'd freeze a
            // partial log snippet under a non-spinning row. Otherwise show
            // them only for low-task-count phases: the gate is static per
            // phase to avoid the height-oscillation scrollback leak.
            let show_tails =
                self.mode == RenderMode::Live && phase.task_count <= TAIL_PREVIEW_MAX_PHASE_TASKS;

            for task in &phase.tasks {
                let (task_glyph, color) = match (task.state, self.mode) {
                    (TaskState::Queued, _) => ("·", Color::DarkGrey),
                    (TaskState::Running, RenderMode::Live) => {
                        (SPINNER_FRAMES[frame % SPINNER_FRAMES.len()], Color::Cyan)
                    }
                    (TaskState::Running, RenderMode::Final) => ("…", Color::DarkGrey),
                    (TaskState::Passed, _) => ("✓", Color::Green),
                    (TaskState::Failed, _) => ("✗", Color::Red),
                };
                let dur = match task.state {
                    TaskState::Queued => String::new(),
                    TaskState::Running => format_duration(task.started.elapsed()),
                    _ => task
                        .duration
                        .map(format_duration)
                        .unwrap_or_else(|| "?".into()),
                };
                let suffix = if self.mode == RenderMode::Final && task.state == TaskState::Running {
                    " (interrupted)"
                } else {
                    ""
                };
                // `phase_start` always pre-populates names, so anonymous
                // placeholders shouldn't reach the renderer. Keep a
                // visual stand-in for the defensive case (e.g. a reporter
                // somehow used the old signature).
                let display_name = if task.name.is_empty() {
                    "(queued)"
                } else {
                    &task.name
                };
                let label = fmt_task_row(task_glyph, display_name, &dur, suffix, name_w);
                out.push(line_styled(label.with(color)));

                // Tail-N preview: only for running tasks with a non-empty
                // ring. Finished tasks would show a stale tail next to the
                // ✓ / ✗ glyph, which is misleading — skip them.
                if show_tails && task.state == TaskState::Running {
                    if let Some(ring) = task.ring.as_ref() {
                        let mut snap = ring.snapshot();
                        let drop_from_front = snap.len().saturating_sub(TAIL_PREVIEW_LINES);
                        if drop_from_front > 0 {
                            snap.drain(0..drop_from_front);
                        }
                        if !snap.is_empty() {
                            for tl in &snap {
                                let trimmed = truncate_for_display(tl, 76);
                                let row = format!("     │  {trimmed}");
                                out.push(line_styled(row.with(Color::DarkGrey)));
                            }
                            out.push(line_styled(
                                "     └────────────────────────────────────"
                                    .to_string()
                                    .with(Color::DarkGrey),
                            ));
                        }
                    }
                }
            }
        }

        Ok(out)
    }
}

fn line_text<S: Into<String>>(s: S) -> Line {
    Line::from_iter([Span::new_unstyled_lossy(s.into())])
}

fn line_styled(s: superconsole::style::StyledContent<String>) -> Line {
    Line::from_iter([Span::new_styled_lossy(s)])
}

/// Truncate a line to at most `max_chars` characters (counting by UTF-8
/// scalars, conservative for terminal-cell width) and append an ellipsis
/// if shortened. Used so a stray long line in the tail preview doesn't
/// wrap the dashboard.
fn truncate_for_display(s: &str, max_chars: usize) -> String {
    let mut count = 0;
    let mut end = s.len();
    for (i, _) in s.char_indices() {
        if count == max_chars {
            end = i;
            break;
        }
        count += 1;
    }
    if end < s.len() {
        let mut out: String = s[..end].into();
        out.push('…');
        out
    } else {
        s.into()
    }
}

/// Dashboard `Reporter` implementation. Spawns a tick thread that
/// re-renders the scratch area every 100 ms; phase/task lifecycle
/// callbacks update shared state.
pub struct DashboardReporter {
    state: Arc<Mutex<DashboardState>>,
    console: Arc<Mutex<Option<SuperConsole>>>,
    log_dir: PathBuf,
    shutdown: Arc<AtomicBool>,
    tick_handle: Mutex<Option<JoinHandle<()>>>,
    /// Set once `finalize_now` has run. After this, `emit_text` /
    /// `emit_styled` fall through to direct stderr writes — the
    /// `SuperConsole` has been taken and any further `ci_emit!` must
    /// not get lost.
    finalized: AtomicBool,
}

impl DashboardReporter {
    /// Try to construct a dashboard reporter. Returns `None` when stdout
    /// is not a TTY (callers should fall back to `BannerReporter`).
    pub fn try_new(log_dir: PathBuf) -> Option<Self> {
        let sc = SuperConsole::new()?;
        // Tell any TeeTerminal sink in this process to stop mirroring to
        // stderr while we own the screen. Restored by `Drop` below.
        crate::exec::set_dashboard_owns_terminal(true);
        let state = Arc::new(Mutex::new(DashboardState::new()));
        let console = Arc::new(Mutex::new(Some(sc)));
        let shutdown = Arc::new(AtomicBool::new(false));

        let state_t = Arc::clone(&state);
        let console_t = Arc::clone(&console);
        let shutdown_t = Arc::clone(&shutdown);
        let tick_handle = std::thread::spawn(move || {
            while !shutdown_t.load(Ordering::Acquire) {
                std::thread::sleep(TICK_INTERVAL);
                let mut sc_guard = console_t.lock().expect("console poisoned");
                let Some(sc) = sc_guard.as_mut() else { break };
                {
                    let mut s = state_t.lock().expect("state poisoned");
                    s.frame = s.frame.wrapping_add(1);
                }
                let root = RootComponent {
                    state: &state_t,
                    mode: RenderMode::Live,
                };
                let _ = sc.render(&root);
            }
        });

        Some(Self {
            state,
            console,
            log_dir,
            shutdown,
            tick_handle: Mutex::new(Some(tick_handle)),
            finalized: AtomicBool::new(false),
        })
    }

    /// Tear down the dashboard deterministically: stop the tick thread,
    /// render the `Final` frame, print the failure footer, release the
    /// terminal-owner flag. Idempotent — repeated calls (including the
    /// Drop safety-net call) no-op.
    ///
    /// Callers in CI-runner code should invoke this immediately after
    /// `pipeline::run()` returns and before any subsequent stderr output,
    /// so the persisted last frame is the clean `RenderMode::Final` view
    /// (no spinners, no Cyan, frozen `…` for interrupted tasks) rather
    /// than whatever the tick thread happened to be rendering when the
    /// process began winding down.
    pub fn finalize_now(&self) {
        if self.finalized.swap(true, Ordering::AcqRel) {
            return;
        }
        // Stop the tick thread first so it can't race the final render.
        self.shutdown.store(true, Ordering::Release);
        if let Some(h) = self.tick_handle.lock().ok().and_then(|mut g| g.take()) {
            let _ = h.join();
        }
        // Snapshot failures before consuming the console.
        let failures: Vec<(String, Tier, String, Duration)> = {
            let s = self.state.lock().expect("dashboard state poisoned");
            s.phases
                .iter()
                .flat_map(|p| {
                    let phase_name = p.name.clone();
                    let tier = p.tier;
                    p.tasks.iter().filter_map(move |t| {
                        if t.state == TaskState::Failed {
                            Some((
                                phase_name.clone(),
                                tier,
                                t.name.clone(),
                                t.duration.unwrap_or_default(),
                            ))
                        } else {
                            None
                        }
                    })
                })
                .collect()
        };
        if let Ok(mut g) = self.console.lock() {
            if let Some(sc) = g.take() {
                let root = RootComponent {
                    state: &self.state,
                    mode: RenderMode::Final,
                };
                let _ = sc.finalize(&root);
            }
        }
        if !failures.is_empty() {
            eprintln!();
            for (phase, tier, task, dur) in &failures {
                let log_path =
                    crate::util::log_paths::stderr_log_path(&self.log_dir, phase, *tier, task);
                eprintln!(
                    "FAILED: {task} in phase {phase} ({dur}). Log: {log}",
                    dur = format_duration(*dur),
                    log = log_path.display(),
                );
            }
        }
        crate::exec::set_dashboard_owns_terminal(false);
    }

    /// Write a phase-specific detail string that renders in parens at the
    /// end of the phase header line. Used by `run_tidy` to surface gc
    /// stats in the phase summary instead of emitting them as a separate
    /// line that duplicates the phase header.
    pub fn set_phase_detail(&self, phase: &str, detail: String) {
        if let Ok(mut s) = self.state.lock() {
            if let Some(p) = s.phase_mut(phase) {
                p.detail = Some(detail);
            }
        }
    }

    /// Create and register a tail-N ring for the given (phase, task).
    /// Called from a task builder BEFORE `task_start` fires; the ring is
    /// claimed and moved into the matching `TaskEntry` when the task
    /// actually starts. Returns an `Arc` the caller plumbs through
    /// `FastBuild::builder().ring_sink(...)`.
    pub fn ring_for(&self, phase: &str, task: &str) -> Arc<LineRing> {
        let ring = Arc::new(LineRing::with_capacity(RING_CAPACITY));
        if let Ok(mut s) = self.state.lock() {
            s.pending_rings
                .insert((phase.to_string(), task.to_string()), Arc::clone(&ring));
        }
        ring
    }

    /// Emit one or more text lines into the scroll history (above the
    /// scratch area). Used for phase summaries and failure dumps.
    ///
    /// Once `finalize_now` has run the `SuperConsole` is gone; fall
    /// through to direct stderr so subsequent `ci_emit!` calls (e.g.
    /// the post-pipeline `verdict.phase_table` lines) are not silently
    /// dropped.
    fn emit_text<I, S>(&self, lines: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        if self.finalized.load(Ordering::Acquire) {
            for s in lines {
                eprintln!("{}", s.into());
            }
            return;
        }
        let mut out = Lines::new();
        for s in lines {
            out.push(line_text(s.into()));
        }
        if let Ok(mut g) = self.console.lock() {
            if let Some(sc) = g.as_mut() {
                sc.emit(out);
            }
        }
    }

    /// Styled variant of `emit_text`. Used by `phase_finish` so the
    /// scrollback summary keeps the same Green/Red coloring the user
    /// saw on the live row.
    fn emit_styled<I>(&self, lines: I)
    where
        I: IntoIterator<Item = StyledContent<String>>,
    {
        if self.finalized.load(Ordering::Acquire) {
            for s in lines {
                eprintln!("{s}");
            }
            return;
        }
        let mut out = Lines::new();
        for s in lines {
            out.push(line_styled(s));
        }
        if let Ok(mut g) = self.console.lock() {
            if let Some(sc) = g.as_mut() {
                sc.emit(out);
            }
        }
    }

    /// Best-effort tail of the per-task stderr log produced by
    /// `nix-fast-build`. The path is constructed via the shared
    /// `crate::util::log_paths::stderr_log_path` helper so writer
    /// (`build_nix_attr_task` in `bin/firestream-ci.rs`) and reader stay in
    /// lockstep. `task` is the full `attr_leaf` (e.g. `required-rust-fmt`,
    /// `advisory-rust-audit`) — no prefix-stripping happens at display
    /// time, so the helper sees the same string the writer used.
    fn read_failure_tail(&self, phase: &str, task: &str) -> Vec<String> {
        let tier = {
            let s = self.state.lock().expect("dashboard state poisoned");
            s.phases
                .iter()
                .find(|p| p.name == phase)
                .map(|p| p.tier)
                .unwrap_or(Tier::Required)
        };
        let path = crate::util::log_paths::stderr_log_path(&self.log_dir, phase, tier, task);
        let Ok(mut f) = std::fs::File::open(&path) else {
            return Vec::new();
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        // Read at most last ~64 KB and split by line, then keep last N.
        let window: u64 = 64 * 1024;
        let start = len.saturating_sub(window);
        if f.seek(SeekFrom::Start(start)).is_err() {
            return Vec::new();
        }
        let reader = BufReader::new(f);
        let mut lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
        if lines.len() > FAILURE_TAIL_LINES {
            let drop = lines.len() - FAILURE_TAIL_LINES;
            lines.drain(0..drop);
        }
        lines
    }
}

// Hand-rolled Debug: `SuperConsole` from upstream doesn't derive Debug, so
// we cannot blanket-derive on the reporter. The struct only ever appears
// in a Debug context via `CiLinuxCtx`'s Debug for tracing — a placeholder
// is fine.
impl std::fmt::Debug for DashboardReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DashboardReporter")
            .field("log_dir", &self.log_dir)
            .finish_non_exhaustive()
    }
}

impl Drop for DashboardReporter {
    fn drop(&mut self) {
        // Panic safety net. On a normal pipeline return the CI-runner
        // already called `finalize_now` and this is a no-op; the early
        // call is what guarantees the Final frame lands before any
        // post-pipeline stderr output.
        self.finalize_now();
    }
}

impl Reporter for DashboardReporter {
    fn phase_start(&self, phase: &str, tier: Tier, task_names: &[&str]) {
        let now = Instant::now();
        let mut s = self.state.lock().expect("state poisoned");
        // Pre-expand the global task-name column to fit every task that
        // will run in this phase, BEFORE any rendering happens. Doing it
        // here (rather than in `task_start`) keeps frame width stable
        // across the whole phase — otherwise the longest name's arrival
        // mid-phase widens every row and superconsole's MoveUp(last_lines)
        // undershoots on terminals where the new width crosses a wrap
        // boundary, leaking prior frames into scrollback.
        for n in task_names {
            let w = n.chars().count();
            if w > s.task_name_w {
                s.task_name_w = w;
            }
        }
        // Pre-allocate placeholder rows WITH their final names so the
        // user sees `· demo-server-…` immediately instead of a
        // generic `(queued)` row that gets backfilled later. `task_start`
        // only needs to flip the state of the matching row.
        let placeholders: Vec<TaskEntry> = task_names
            .iter()
            .map(|n| TaskEntry {
                name: (*n).to_string(),
                started: now,
                state: TaskState::Queued,
                duration: None,
                error: None,
                ring: None,
            })
            .collect();
        s.phases.push(PhaseEntry {
            name: phase.to_string(),
            tier,
            task_count: task_names.len(),
            started: now,
            tasks: placeholders,
            finished: false,
            detail: None,
        });
    }

    fn task_start(&self, phase: &str, task: &str) {
        let mut s = self.state.lock().expect("state poisoned");
        let ring = s
            .pending_rings
            .remove(&(phase.to_string(), task.to_string()));
        // Try to flip the pre-allocated placeholder. The placeholder was
        // created in `phase_start` with the right name — width and row
        // count don't change. The bool tracks whether we hit the happy
        // path; the fallback below recovers if a task slipped through.
        let mut promoted = false;
        if let Some(p) = s.phase_mut(phase) {
            if let Some(slot) = p
                .tasks
                .iter_mut()
                .find(|t| t.name == task && t.state == TaskState::Queued)
            {
                slot.started = Instant::now();
                slot.state = TaskState::Running;
                slot.ring = ring.clone();
                promoted = true;
            }
        }
        if !promoted {
            // Task wasn't pre-declared (defensive — shouldn't happen for
            // any phase that goes through `phase_start` with the task
            // list). Push a fresh row; mid-phase width expansion is the
            // price of recovery.
            let w = task.chars().count();
            if w > s.task_name_w {
                s.task_name_w = w;
            }
            if let Some(p) = s.phase_mut(phase) {
                p.tasks.push(TaskEntry {
                    name: task.to_string(),
                    started: Instant::now(),
                    state: TaskState::Running,
                    duration: None,
                    error: None,
                    ring,
                });
            }
        }
    }

    fn task_finish(
        &self,
        phase: &str,
        task: &str,
        ok: bool,
        duration: Duration,
        error: Option<&str>,
    ) {
        {
            let mut s = self.state.lock().expect("state poisoned");
            if let Some(p) = s.phase_mut(phase) {
                if let Some(t) = p.tasks.iter_mut().find(|t| t.name == task) {
                    t.state = if ok {
                        TaskState::Passed
                    } else {
                        TaskState::Failed
                    };
                    t.duration = Some(duration);
                    t.error = error.map(str::to_string);
                }
            }
        }
        if !ok {
            // Emit a labeled failure block so the failure context lands
            // in scroll history (scratch can clear; this can't).
            let header = format!(
                "─── {task} failed in {dur} ───",
                task = task,
                dur = format_duration(duration),
            );
            let reason = error.unwrap_or("(no error message)").to_string();
            let mut block: Vec<String> = vec![String::new(), header, format!("  reason: {reason}")];
            let tail = self.read_failure_tail(phase, task);
            if !tail.is_empty() {
                block.push(String::from("  ─── last lines of stderr log ───"));
                block.extend(tail.into_iter().map(|l| format!("  {l}")));
            }
            block.push(String::from("─────────────────────────────────────"));
            block.push(String::new());
            self.emit_text(block);
        }
    }

    fn phase_finish(&self, phase: &str, ok: bool, duration: Duration) {
        // Pull what we need from state in one lock: the optional phase
        // detail, the names of failed tasks (only failures earn a per-task
        // row — the user already watched every PASS go by live), and the
        // current global task-name column width so the scrollback row
        // lines up with the live rows.
        let (detail, failed_tasks, name_w) = {
            let mut s = self.state.lock().expect("state poisoned");
            let name_w = s.task_name_w;
            if let Some(p) = s.phase_mut(phase) {
                p.finished = true;
                let detail = p.detail.clone();
                let failed: Vec<(String, Option<Duration>)> = p
                    .tasks
                    .iter()
                    .filter(|t| t.state == TaskState::Failed)
                    .map(|t| (t.name.clone(), t.duration))
                    .collect();
                (detail, failed, name_w)
            } else {
                (None, Vec::new(), name_w)
            }
        };

        // Pass/fail color overrides tier on the finish row — it's the
        // verdict-bearing line, so green/red wins over cyan/dim grey.
        let (glyph, status, color) = if ok {
            ("✓", "passed", Color::Green)
        } else {
            ("✗", "failed", Color::Red)
        };
        let dur_str = format_duration(duration);
        let header = fmt_phase_row(glyph, phase, status, &dur_str, detail.as_deref());

        let mut block: Vec<StyledContent<String>> = vec![header.with(color)];
        // Per-task rows only on failure. Successful phases get the
        // header line and nothing else — the live block already showed
        // every PASS row.
        for (name, dur) in &failed_tasks {
            let d = dur.map(format_duration).unwrap_or_else(|| "?".into());
            let row = fmt_task_row("✗", name, &d, "", name_w);
            block.push(row.with(Color::Red));
        }
        self.emit_styled(block);
    }

    fn phase_skipped(&self, phase: &str, _tier: Tier) {
        let line = fmt_phase_row("·", phase, "skipped", "", Some("upstream required failed"));
        self.emit_styled([line.with(Color::DarkGrey)]);
    }
}

/// Decide which `Reporter` impl to install based on TTY presence and the
/// `FIRESTREAM_CI_UI` env var. Returns `(reporter, kind_label)` so the caller
/// can log which mode was chosen.
///
/// `FIRESTREAM_CI_UI` values:
///   - `auto` (default): dashboard when stderr is a TTY and not in CI;
///     banner otherwise.
///   - `dashboard`: force dashboard, even when piped (useful for screenshots).
///   - `banner`:    force the legacy line-oriented reporter.
pub fn select_reporter(
    log_dir: PathBuf,
    banner: Arc<dyn Reporter>,
) -> (
    Arc<dyn Reporter>,
    Option<Arc<DashboardReporter>>,
    &'static str,
) {
    use std::io::IsTerminal;

    let pref = std::env::var("FIRESTREAM_CI_UI").unwrap_or_else(|_| "auto".into());
    let want_dashboard = match pref.as_str() {
        "dashboard" => true,
        "banner" => false,
        _ => {
            let is_tty = std::io::stderr().is_terminal();
            let no_color = std::env::var_os("NO_COLOR").is_some();
            let dumb = std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false);
            let in_ci = std::env::var_os("CI").is_some();
            is_tty && !no_color && !dumb && !in_ci
        }
    };

    if want_dashboard {
        if let Some(r) = DashboardReporter::try_new(log_dir) {
            let arc = Arc::new(r);
            set_active_dashboard(Arc::clone(&arc));
            return (
                Arc::clone(&arc) as Arc<dyn Reporter>,
                Some(arc),
                "dashboard",
            );
        }
    }
    (banner, None, "banner")
}

// ---------------------------------------------------------------------------
// Active-reporter sidecar.
//
// In dashboard mode, superconsole is the *only* writer to stderr — anything
// else writing in parallel mangles the redraw. Direct `eprintln!` sites in
// the CI runtime path use the `emit!` macro below, which routes through
// this sidecar when set and falls through to `eprintln!` when not.
//
// Written exactly once during reporter selection; read-only thereafter.
// `OnceLock` so reads are lock-free.
// ---------------------------------------------------------------------------

static ACTIVE_DASHBOARD: OnceLock<Arc<DashboardReporter>> = OnceLock::new();

fn set_active_dashboard(d: Arc<DashboardReporter>) {
    // First wins; runs once per process per the OnceLock contract.
    let _ = ACTIVE_DASHBOARD.set(d);
}

/// Emit one line either into the dashboard's scroll history (when active)
/// or to stderr (banner / no-UI mode). Public so the `emit!` macro and
/// direct callers can use it without spelling out the sidecar.
pub fn emit_line(line: impl Into<String>) {
    if let Some(d) = ACTIVE_DASHBOARD.get() {
        d.emit_text([line.into()]);
    } else {
        eprintln!("{}", line.into());
    }
}

/// Finalize the active dashboard (if any) so the persisted final frame
/// is a clean `RenderMode::Final` snapshot. Safe to call when no
/// dashboard is registered (banner mode), and idempotent on the
/// reporter so repeated calls are no-ops.
///
/// CI-runner code calls this immediately after `Pipeline::run()`
/// returns so subsequent stderr output (phase tables, `manifest:
/// wrote …`, `CI summary: …`, the final `firestream-ci ci-linux: pipeline
/// passed/failed` line) lands cleanly below the finalized frame rather
/// than mixing with the live tick render.
pub fn finalize_active_dashboard() {
    if let Some(d) = ACTIVE_DASHBOARD.get() {
        d.finalize_now();
    }
}

/// `eprintln!`-shaped macro that routes through the active dashboard when
/// one is registered, and falls back to `eprintln!` otherwise. Use this at
/// every CI-runtime call site that previously wrote to stderr directly —
/// the manifest banner, phase advisory notices, the SBOM check, etc.
#[macro_export]
macro_rules! ci_emit {
    () => { $crate::dashboard::emit_line(String::new()) };
    ($($arg:tt)*) => { $crate::dashboard::emit_line(format!($($arg)*)) };
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountingReporter;
    impl Reporter for CountingReporter {
        fn phase_start(&self, _: &str, _: Tier, _: &[&str]) {}
        fn task_start(&self, _: &str, _: &str) {}
        fn task_finish(&self, _: &str, _: &str, _: bool, _: Duration, _: Option<&str>) {}
        fn phase_finish(&self, _: &str, _: bool, _: Duration) {}
    }

    #[test]
    fn select_reporter_falls_back_to_banner_when_ui_is_banner() {
        // SAFETY: tests in this binary are not parallelized at the env-var
        // level; the value is restored at end-of-test.
        unsafe {
            std::env::set_var("FIRESTREAM_CI_UI", "banner");
        }
        let banner: Arc<dyn Reporter> = Arc::new(CountingReporter);
        let (_r, _d, kind) = select_reporter(PathBuf::from("/tmp"), banner);
        assert_eq!(kind, "banner");
        unsafe {
            std::env::remove_var("FIRESTREAM_CI_UI");
        }
    }

    #[test]
    fn select_reporter_falls_back_to_banner_when_no_tty() {
        // Test runner stdio is not a TTY → auto mode picks banner.
        unsafe {
            std::env::set_var("FIRESTREAM_CI_UI", "auto");
        }
        let banner: Arc<dyn Reporter> = Arc::new(CountingReporter);
        let (_r, _d, kind) = select_reporter(PathBuf::from("/tmp"), banner);
        assert_eq!(kind, "banner");
        unsafe {
            std::env::remove_var("FIRESTREAM_CI_UI");
        }
    }

    // ────────────────────────────────────────────────────────────────────
    // Rendering tests. Driven through `RootComponent::draw_unchecked` on
    // a synthetic `DashboardState` — keeps the tests TTY-free (the real
    // `DashboardReporter::try_new` needs a terminal) while still exercising
    // the formatter + layout code paths the user sees.
    // ────────────────────────────────────────────────────────────────────

    fn make_state(task_name_w: usize, phases: Vec<PhaseEntry>) -> Mutex<DashboardState> {
        Mutex::new(DashboardState {
            phases,
            frame: 0,
            pending_rings: HashMap::new(),
            task_name_w,
        })
    }

    fn task(name: &str, state: TaskState, dur_s: f64) -> TaskEntry {
        TaskEntry {
            name: name.to_string(),
            started: Instant::now() - Duration::from_secs_f64(dur_s),
            state,
            duration: matches!(state, TaskState::Passed | TaskState::Failed)
                .then(|| Duration::from_secs_f64(dur_s)),
            error: None,
            ring: None,
        }
    }

    fn render(state: &Mutex<DashboardState>, mode: RenderMode) -> Vec<String> {
        use superconsole::Dimensions;
        let root = RootComponent { state, mode };
        let dims = Dimensions {
            width: 200,
            height: 100,
        };
        let lines = root
            .draw_unchecked(dims, DrawMode::Normal)
            .expect("render did not fail");
        // Stringify each rendered line by concatenating its spans' raw text.
        // We deliberately drop ANSI styling so tests can assert on column
        // layout without coupling to terminfo bytes.
        lines
            .iter()
            .map(|line| {
                line.iter()
                    .map(|span| span.content().to_string())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn fmt_phase_row_aligns_columns() {
        // Different phase names + status/duration values must land their
        // status and duration columns at the same character positions.
        let a = fmt_phase_row("⠸", "verify", "9/9", "9.3s", None);
        let b = fmt_phase_row("✓", "build", "passed", "120.5s", None);
        // Status column start: position of the first non-space char after
        // the phase-name pad.
        let status_start = |s: &str, status_first_char: char| -> usize {
            s.chars()
                .position(|c| c == status_first_char)
                .expect("status")
        };
        assert_eq!(
            status_start(&a, '9'),
            status_start(&b, 'p'),
            "status column drifts:\n  {a:?}\n  {b:?}"
        );
        // Duration column end: both rows end with the duration (no detail),
        // both durations should end at the same char index (right-aligned).
        assert_eq!(
            a.chars().count(),
            b.chars().count(),
            "row lengths drift, durations not right-aligned:\n  {a:?}\n  {b:?}"
        );
    }

    #[test]
    fn display_task_name_strips_tier_prefix() {
        assert_eq!(display_task_name("required-rust-fmt"), "rust-fmt");
        assert_eq!(display_task_name("advisory-rust-audit"), "rust-audit");
        // No prefix → unchanged. Stderr-log paths rely on this being the
        // full string everywhere except the display cell.
        assert_eq!(display_task_name("nix-gc"), "nix-gc");
        assert_eq!(
            display_task_name("demo-server-linux-x86_64-gpu"),
            "demo-server-linux-x86_64-gpu"
        );
    }

    #[test]
    fn fmt_task_row_strips_tier_prefix_for_display() {
        let row = fmt_task_row("✓", "required-rust-fmt", "1.9s", "", 12);
        assert!(
            row.contains("rust-fmt"),
            "stripped name missing in row: {row:?}"
        );
        assert!(
            !row.contains("required-"),
            "tier prefix leaked into display row: {row:?}"
        );
    }

    #[test]
    fn fmt_task_row_pads_to_name_w() {
        let short = fmt_task_row("✓", "ts-tsc", "9.3s", "", 20);
        let long = fmt_task_row("✓", "demo-server-linux-x86_64-gpu", "9.3s", "", 33);
        // Both rows place the duration after the padded name column;
        // with the correct name_w, durations land at the same offset.
        assert!(
            short.ends_with("9.3s"),
            "short row missing duration: {short:?}"
        );
        assert!(
            long.ends_with("9.3s"),
            "long row missing duration: {long:?}"
        );
    }

    #[test]
    fn fmt_phase_row_appends_detail() {
        let row = fmt_phase_row(
            "✓",
            "tidy",
            "passed",
            "2h20m",
            Some("41 roots ok, 2 failed, 0 deleted"),
        );
        assert!(row.contains("(41 roots ok, 2 failed, 0 deleted)"), "{row}");
        // Detail follows the duration column, never overlaps it.
        let dur_idx = row.find("2h20m").expect("duration in row");
        let detail_idx = row.find('(').expect("detail in row");
        assert!(detail_idx > dur_idx);
    }

    #[test]
    fn final_mode_has_no_spinner_or_cyan() {
        // A phase with one running task — Final mode must render `…`
        // and DarkGrey, not the live spinner / Cyan.
        let phase = PhaseEntry {
            name: "build".into(),
            tier: Tier::Required,
            task_count: 1,
            started: Instant::now() - Duration::from_secs(5),
            tasks: vec![task("demo-cli", TaskState::Running, 5.0)],
            finished: false,
            detail: None,
        };
        let state = make_state(20, vec![phase]);
        let rendered = render(&state, RenderMode::Final);
        let joined: String = rendered.join("\n");
        // No spinner glyphs.
        for frame in SPINNER_FRAMES {
            assert!(
                !joined.contains(frame),
                "Final frame contains spinner {frame:?}: {joined:?}"
            );
        }
        // The interrupted suffix marks where the user would have seen
        // a spinning task.
        assert!(joined.contains("(interrupted)"), "{joined}");
    }

    #[test]
    fn live_mode_skips_finished_phases() {
        // A finished phase should not render in either mode (the phase
        // summary is in scrollback; the live block would duplicate it).
        let phase = PhaseEntry {
            name: "verify".into(),
            tier: Tier::Required,
            task_count: 1,
            started: Instant::now() - Duration::from_secs(9),
            tasks: vec![task("rust-nextest", TaskState::Passed, 4.2)],
            finished: true,
            detail: None,
        };
        let state = make_state(20, vec![phase]);
        assert!(render(&state, RenderMode::Live).is_empty());
        assert!(render(&state, RenderMode::Final).is_empty());
    }

    #[test]
    fn detail_renders_in_live_phase_header() {
        let phase = PhaseEntry {
            name: "tidy".into(),
            tier: Tier::Advisory,
            task_count: 1,
            started: Instant::now() - Duration::from_secs(60),
            tasks: vec![task("nix-gc", TaskState::Passed, 60.0)],
            finished: false,
            detail: Some("41 roots ok".into()),
        };
        let state = make_state(20, vec![phase]);
        let rendered = render(&state, RenderMode::Live);
        let header = rendered.first().expect("header rendered");
        assert!(header.contains("(41 roots ok)"), "{header}");
    }

    /// The failure-tail path the dashboard reads must match the path
    /// `build_nix_attr_task` writes — for *both* required and advisory
    /// tasks. Before this refactor, the dashboard rebuilt
    /// `{phase}-{tier}-{task}.stderr.log` and only coincidentally lined
    /// up with the writer's `{phase}-{attr_leaf}.stderr.log` for required
    /// tasks (where the task name had `required-` stripped). Advisory
    /// tasks doubled their `advisory-` segment and the failure block
    /// rendered empty.
    #[test]
    fn failure_tail_path_matches_writer_for_advisory_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logs = tmp.path();
        // Writer side: same call build_nix_attr_task makes.
        let phase = "verify";
        let attr_leaf = "advisory-rust-audit";
        let writer_path =
            crate::util::log_paths::stderr_log_path(logs, phase, Tier::Advisory, attr_leaf);
        std::fs::write(&writer_path, b"line one\nline two\n").expect("write fixture");
        // Reader side: same call read_failure_tail makes (task ==
        // attr_leaf now that the strip-`required-` rule is gone).
        let reader_path =
            crate::util::log_paths::stderr_log_path(logs, phase, Tier::Advisory, attr_leaf);
        assert_eq!(writer_path, reader_path);
        assert!(reader_path.exists(), "writer + reader disagree on path");
        // And no double-`advisory-` regression in the filename itself.
        let fname = reader_path.file_name().unwrap().to_string_lossy();
        assert!(
            !fname.contains("advisory-advisory"),
            "double-advisory regression: {fname}"
        );
    }

    /// The leak this guards against: before pre-populating task names at
    /// `phase_start`, `task_name_w` expanded mid-phase as each wider task
    /// name registered via `task_start`. Each width bump changed the
    /// rendered character width of every row in the frame; on terminals
    /// where a row crossed the wrap threshold, superconsole's
    /// `MoveUp(last_lines)` undershot by the wrapped overage and the
    /// previous frame leaked into scrollback as a stair-stepped duplicate.
    ///
    /// The new contract: `phase_start` receives the full task-name list
    /// and pre-expands `task_name_w` AND pre-populates placeholders, so
    /// every render across the phase has identical row count and
    /// identical column width — regardless of how task lifecycle events
    /// interleave with tick renders.
    #[test]
    fn live_frame_geometry_is_stable_across_task_lifecycle() {
        // Synthesize the same state the dashboard reporter would build
        // for a `build` phase with 9 attrs, where the longest name
        // (`demo-server-linux-x86_64-gpu`, 33 chars) is the LAST
        // task to register. This is the path that historically expanded
        // `task_name_w` from ~21 to 33 mid-phase.
        let task_names = [
            "demo-server-linux-x86_64",
            "demo-web-linux-x86_64",
            "demo-docs-linux-x86_64",
            "demo-sbom",
            "demo-cli",
            "demo-portal-wasm",
            "demo-spreadsheet-editor-wasm",
            "demo-core-dataflow-wasm",
            "demo-server-linux-x86_64-gpu",
        ];
        let max_name_w = task_names.iter().map(|n| n.chars().count()).max().unwrap();
        let now = Instant::now();

        // What `phase_start` now produces: all names populated, all
        // states Queued, task_name_w pre-expanded.
        let phase = PhaseEntry {
            name: "build".into(),
            tier: Tier::Required,
            task_count: task_names.len(),
            started: now,
            tasks: task_names
                .iter()
                .map(|n| TaskEntry {
                    name: (*n).into(),
                    started: now,
                    state: TaskState::Queued,
                    duration: None,
                    error: None,
                    ring: None,
                })
                .collect(),
            finished: false,
            detail: None,
        };
        let state = make_state(max_name_w, vec![phase]);

        // Frame 0: all queued. The geometric baseline.
        let frame0 = render(&state, RenderMode::Live);
        let row_count_0 = frame0.len();
        let max_width_0 = frame0.iter().map(|s| s.chars().count()).max().unwrap_or(0);
        assert_eq!(row_count_0, 1 + task_names.len(), "header + 9 tasks");

        // Frame 1: server, web, docs flip to Running. (`task_start`
        // mutates state; row count and widths should not change.)
        {
            let mut s = state.lock().expect("state");
            let p = s.phase_mut("build").expect("phase");
            for slot in p.tasks.iter_mut() {
                if matches!(
                    slot.name.as_str(),
                    "demo-server-linux-x86_64"
                        | "demo-web-linux-x86_64"
                        | "demo-docs-linux-x86_64"
                ) {
                    slot.state = TaskState::Running;
                }
            }
        }
        let frame1 = render(&state, RenderMode::Live);
        let row_count_1 = frame1.len();
        let max_width_1 = frame1.iter().map(|s| s.chars().count()).max().unwrap_or(0);

        // Frame 2: now the GPU task (the longest name) flips to Running,
        // and server finishes. Historically the width bump from name 21→33
        // happened HERE — under the new contract it's a no-op for width.
        {
            let mut s = state.lock().expect("state");
            let p = s.phase_mut("build").expect("phase");
            for slot in p.tasks.iter_mut() {
                if slot.name == "demo-server-linux-x86_64" {
                    slot.state = TaskState::Passed;
                    slot.duration = Some(Duration::from_secs(10));
                }
                if slot.name == "demo-server-linux-x86_64-gpu" {
                    slot.state = TaskState::Running;
                }
            }
        }
        let frame2 = render(&state, RenderMode::Live);
        let row_count_2 = frame2.len();
        let max_width_2 = frame2.iter().map(|s| s.chars().count()).max().unwrap_or(0);

        assert_eq!(
            row_count_0, row_count_1,
            "row count must not change when tasks start running"
        );
        assert_eq!(
            row_count_1, row_count_2,
            "row count must not change when the widest task starts/finishes"
        );
        assert_eq!(
            max_width_0, max_width_1,
            "column width must not change between Queued and Running renders"
        );
        assert_eq!(
            max_width_1, max_width_2,
            "column width must not change when the widest task transitions",
        );
    }

    #[test]
    fn failure_tail_path_matches_writer_for_required_task() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let logs = tmp.path();
        let phase = "verify";
        let attr_leaf = "required-rust-fmt";
        let writer_path =
            crate::util::log_paths::stderr_log_path(logs, phase, Tier::Required, attr_leaf);
        std::fs::write(&writer_path, b"diff line\n").expect("write fixture");
        let reader_path =
            crate::util::log_paths::stderr_log_path(logs, phase, Tier::Required, attr_leaf);
        assert_eq!(writer_path, reader_path);
        assert!(reader_path.exists());
    }
}
