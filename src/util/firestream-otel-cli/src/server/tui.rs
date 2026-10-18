//! `server tui` — ratatui-based rolling span table.
//!
//! Go reference: `otelcli/server_tui.go`. The Go version uses pterm; here we
//! use ratatui + crossterm and keep the v1 behaviour minimal: a scrolling
//! `Table` of the most recent N spans, where N is bounded by terminal height.

use std::io::{self, Stdout};
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyModifiers},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use opentelemetry_proto::tonic::trace::v1::ResourceSpans;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Constraint;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Cell, Row, Table};
use ratatui::Terminal;
use tokio::sync::mpsc;

/// A flattened span row for rendering.
struct SpanRow {
    trace_id: String,
    span_id: String,
    parent: String,
    name: String,
    kind: String,
    start: u64,
    elapsed_ms: i64,
}

fn span_kind_str(k: i32) -> &'static str {
    // Mirrors otlpclient.SpanKindIntToString.
    match k {
        1 => "internal",
        2 => "server",
        3 => "client",
        4 => "producer",
        5 => "consumer",
        _ => "unspecified",
    }
}

fn rows_from_rs(rs: &ResourceSpans) -> Vec<SpanRow> {
    let mut out = Vec::new();
    for ss in &rs.scope_spans {
        for s in &ss.spans {
            let start_ms = (s.start_time_unix_nano / 1_000_000) as i64;
            let end_ms = (s.end_time_unix_nano / 1_000_000) as i64;
            out.push(SpanRow {
                trace_id: hex::encode(&s.trace_id),
                span_id: hex::encode(&s.span_id),
                parent: hex::encode(&s.parent_span_id),
                name: s.name.clone(),
                kind: span_kind_str(s.kind).to_string(),
                start: s.start_time_unix_nano,
                elapsed_ms: (end_ms - start_ms).max(0),
            });
        }
    }
    out
}

/// Run the TUI event loop. Returns when the user presses `q` or `Ctrl+C`, or
/// when the input channel closes (sender dropped on shutdown).
pub async fn run_tui(mut rx: mpsc::Receiver<ResourceSpans>) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = event_loop(&mut terminal, &mut rx).await;

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    result
}

async fn event_loop(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rx: &mut mpsc::Receiver<ResourceSpans>,
) -> Result<()> {
    let mut rows: Vec<SpanRow> = Vec::new();
    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));

    loop {
        draw(terminal, &rows)?;

        tokio::select! {
            _ = tick.tick() => {}
            maybe_rs = rx.recv() => {
                match maybe_rs {
                    Some(rs) => {
                        rows.extend(rows_from_rs(&rs));
                        // sort by start time, keep newest tail
                        rows.sort_by_key(|r| r.start);
                        // keep up to terminal_height-3 rows
                        let h = terminal.size().map(|s| s.height as usize).unwrap_or(24);
                        let cap = h.saturating_sub(3).max(1);
                        if rows.len() > cap {
                            let drop = rows.len() - cap;
                            rows.drain(..drop);
                        }
                    }
                    None => {
                        // channel closed — sender dropped; exit cleanly.
                        return Ok(());
                    }
                }
            }
            maybe_evt = events.next() => {
                match maybe_evt {
                    Some(Ok(Event::Key(k))) => {
                        if k.code == KeyCode::Char('q')
                            || (k.code == KeyCode::Char('c')
                                && k.modifiers.contains(KeyModifiers::CONTROL))
                            || k.code == KeyCode::Esc
                        {
                            return Ok(());
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        tracing::warn!(error = %e, "tui: event stream error");
                    }
                    None => return Ok(()),
                }
            }
        }
    }
}

fn draw(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    rows: &[SpanRow],
) -> Result<()> {
    terminal.draw(|f| {
        let area = f.area();
        let header = Row::new(vec![
            Cell::from("Trace ID"),
            Cell::from("Span ID"),
            Cell::from("Parent"),
            Cell::from("Name"),
            Cell::from("Kind"),
            Cell::from("Start"),
            Cell::from("Elapsed"),
        ])
        .style(Style::default().add_modifier(Modifier::BOLD));

        let body: Vec<Row> = rows
            .iter()
            .map(|r| {
                Row::new(vec![
                    Cell::from(r.trace_id.clone()),
                    Cell::from(r.span_id.clone()),
                    Cell::from(r.parent.clone()),
                    Cell::from(r.name.clone()),
                    Cell::from(r.kind.clone()),
                    Cell::from(r.start.to_string()),
                    Cell::from(format!("{}ms", r.elapsed_ms)),
                ])
            })
            .collect();

        let widths = [
            Constraint::Length(34),
            Constraint::Length(18),
            Constraint::Length(18),
            Constraint::Length(24),
            Constraint::Length(10),
            Constraint::Length(20),
            Constraint::Length(10),
        ];

        let title = format!(
            "otel-cli server tui — {} span(s) — press 'q' to quit",
            rows.len()
        );
        let table = Table::new(body, widths).header(header).block(
            Block::default()
                .borders(Borders::ALL)
                .title(title),
        );

        f.render_widget(table, area);
    })?;
    Ok(())
}
