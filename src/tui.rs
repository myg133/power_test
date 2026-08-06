//! Terminal UI for live progress display. Optional (`--tui`).
//!
//! Layout: top bar with run id / target / RPS / time remaining, middle
//! stacked panels for latency / TTFT / ITL / TPS percentiles, bottom
//! bar with progress + error count, footer with key hints.
//!
//! The TUI runs in a blocking thread (crossterm works fine in one) and
//! snapshots the [`MetricsAggregator`] every 250 ms. On `q` or `Esc`
//! it triggers a cancel and returns; the executor drains in-flight
//! requests and the CLI prints the normal summary afterwards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Widget, Wrap};
use ratatui::Terminal;

use crate::config::RunConfig;
use crate::error::Result;
use crate::runner::{HistKind, MetricsAggregator};

/// What the user wants the TUI to do based on the last key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuiAction {
    /// Cancel the run (default for `q` / `Esc` / `c`).
    Quit,
    /// Toggle pause / resume.
    Pause,
    /// Anything else.
    None,
}

/// Pure function: turn a key event into an action. Public for tests.
pub fn parse_key_event(key: KeyEvent) -> TuiAction {
    if key.kind != KeyEventKind::Press && key.kind != KeyEventKind::Repeat {
        return TuiAction::None;
    }
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => TuiAction::Quit,
        KeyCode::Char('c') | KeyCode::Char('C') => TuiAction::Quit,
        KeyCode::Esc => TuiAction::Quit,
        KeyCode::Char('p') | KeyCode::Char('P') => TuiAction::Pause,
        _ => TuiAction::None,
    }
}

/// Snapshot of the aggregator + derived fields used by the renderer.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub total: u64,
    pub success: u64,
    pub errors: u64,
    pub scheduled: u64,
    pub skipped: u64,
    /// Achieved RPS over the last 1 second (or full run so far if
    /// not enough samples).
    pub current_rps: f64,
    pub latency_p50_ms: Option<f64>,
    pub latency_p90_ms: Option<f64>,
    pub latency_p99_ms: Option<f64>,
    pub ttft_p50_ms: Option<f64>,
    pub ttft_p99_ms: Option<f64>,
    pub itl_mean_ms: Option<f64>,
    pub itl_p99_ms: Option<f64>,
    pub tps_mean: f64,
    pub tps_p99: f64,
    /// Seconds remaining (clamped to >= 0).
    pub seconds_remaining: f64,
}

impl Snapshot {
    pub fn from_aggregator(
        agg: &MetricsAggregator,
        run_started: Instant,
        duration_secs: u64,
    ) -> Self {
        let total = agg.total_requests();
        let success = agg.success_count();
        let errors = agg.error_count();
        let scheduled = agg.scheduled();
        let skipped = agg.skipped();
        let elapsed = run_started.elapsed().as_secs_f64();
        let achieved = if elapsed > 0.0 {
            total as f64 / elapsed
        } else {
            0.0
        };
        // 1s rolling RPS: count completions in the most recent
        // second-bucket of `per_second_completed`.
        let rolling = match agg.per_second_completed().iter().max_by_key(|(s, _)| *s) {
            Some((_, c)) => *c as f64,
            None => achieved,
        };
        let current_rps = if rolling > 0.0 { rolling } else { achieved };
        let seconds_remaining = (duration_secs as f64 - elapsed).max(0.0);
        Self {
            total,
            success,
            errors,
            scheduled,
            skipped,
            current_rps,
            latency_p50_ms: agg.percentile(HistKind::Latency, 50.0).map(|v| us_to_ms(v)),
            latency_p90_ms: agg.percentile(HistKind::Latency, 90.0).map(|v| us_to_ms(v)),
            latency_p99_ms: agg.percentile(HistKind::Latency, 99.0).map(|v| us_to_ms(v)),
            ttft_p50_ms: agg.percentile(HistKind::Ttft, 50.0).map(|v| us_to_ms(v)),
            ttft_p99_ms: agg.percentile(HistKind::Ttft, 99.0).map(|v| us_to_ms(v)),
            itl_mean_ms: agg.mean(HistKind::Itl).map(|v| v / 1000.0),
            itl_p99_ms: agg.percentile(HistKind::Itl, 99.0).map(|v| us_to_ms(v)),
            tps_mean: agg.tps_mean(),
            tps_p99: agg.tps_percentile(99.0),
            seconds_remaining,
        }
    }
}

fn us_to_ms(us: u64) -> f64 {
    us as f64 / 1000.0
}

/// Render a snapshot to a `Buffer` (used in tests, no real terminal).
/// Public for tests.
pub fn render_to_buffer(snap: &Snapshot, cfg: &RunConfig, area: Rect, buf: &mut Buffer) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // top bar
            Constraint::Length(7), // latency panel
            Constraint::Length(5), // ttft panel
            Constraint::Length(5), // itl panel
            Constraint::Length(5), // tps panel
            Constraint::Length(3), // bottom bar
            Constraint::Length(3), // footer
        ])
        .split(area);

    render_top_bar(snap, cfg, chunks[0], buf);
    render_latency(snap, chunks[1], buf);
    render_ttft(snap, chunks[2], buf);
    render_itl(snap, chunks[3], buf);
    render_tps(snap, chunks[4], buf);
    render_bottom_bar(snap, cfg, chunks[5], buf);
    render_footer(chunks[6], buf);
}

fn bordered_block(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)))
}

fn bordered_box(title: &str) -> Block<'_> {
    Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)))
}

fn render_top_bar(snap: &Snapshot, cfg: &RunConfig, area: Rect, buf: &mut Buffer) {
    let short_id: String = cfg.run_id.chars().take(8).collect();
    let target_short = if cfg.target.chars().count() > 50 {
        let s: String = cfg.target.chars().take(47).collect();
        format!("{s}…")
    } else {
        cfg.target.clone()
    };
    let line = Line::from(vec![
        Span::styled(
            format!(" run {} ", short_id),
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" target={} ", target_short)),
        Span::raw(format!(" model={} ", cfg.model)),
        Span::raw(format!(
            " target_rps={:.2} current_rps={:.2} remaining={:.0}s ",
            cfg.target_rps, snap.current_rps, snap.seconds_remaining
        )),
    ]);
    Paragraph::new(line)
        .block(bordered_block(" power_test "))
        .render(area, buf);
}

fn render_latency(snap: &Snapshot, area: Rect, buf: &mut Buffer) {
    let items: Vec<ListItem> = vec![
        ListItem::new(format!(
            " p50:  {}",
            fmt_ms(snap.latency_p50_ms)
        )),
        ListItem::new(format!(
            " p90:  {}",
            fmt_ms(snap.latency_p90_ms)
        )),
        ListItem::new(format!(
            " p99:  {}",
            fmt_ms(snap.latency_p99_ms)
        )),
    ];
    List::new(items).block(bordered_block(" Latency (ms) ")).render(area, buf);
}

fn render_ttft(snap: &Snapshot, area: Rect, buf: &mut Buffer) {
    let items: Vec<ListItem> = vec![
        ListItem::new(format!(" p50:  {}", fmt_ms(snap.ttft_p50_ms))),
        ListItem::new(format!(" p99:  {}", fmt_ms(snap.ttft_p99_ms))),
    ];
    List::new(items).block(bordered_block(" TTFT (ms) ")).render(area, buf);
}

fn render_itl(snap: &Snapshot, area: Rect, buf: &mut Buffer) {
    let items: Vec<ListItem> = vec![
        ListItem::new(format!(" mean: {}", fmt_ms(snap.itl_mean_ms))),
        ListItem::new(format!(" p99:  {}", fmt_ms(snap.itl_p99_ms))),
    ];
    List::new(items).block(bordered_box(" ITL (ms) ")).render(area, buf);
}

fn render_tps(snap: &Snapshot, area: Rect, buf: &mut Buffer) {
    let items: Vec<ListItem> = vec![
        ListItem::new(format!(" mean: {:.2}", snap.tps_mean)),
        ListItem::new(format!(" p99:  {:.2}", snap.tps_p99)),
    ];
    List::new(items)
        .block(bordered_box(" TPS "))
        .render(area, buf);
}

fn render_bottom_bar(snap: &Snapshot, cfg: &RunConfig, area: Rect, buf: &mut Buffer) {
    let text = format!(
        " {}/{} requests, {} errors, skipped {}, ETA {:.0}s ",
        snap.total,
        snap.scheduled.saturating_add(snap.skipped),
        snap.errors,
        snap.skipped,
        snap.seconds_remaining
    );
    let para = Paragraph::new(Line::from(Span::raw(text)));
    para.block(bordered_box(&format!(" {} ", cfg.api.as_str())))
        .wrap(Wrap { trim: true })
        .render(area, buf);
}

fn render_footer(area: Rect, buf: &mut Buffer) {
    let line = Line::from(vec![
        Span::styled(" [q]", Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)),
        Span::raw(" quit  "),
        Span::styled(
            " [p]",
            Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" pause  "),
        Span::styled(
            " [c]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(" cancel "),
    ]);
    Paragraph::new(line).render(area, buf);
}

fn fmt_ms(v: Option<f64>) -> String {
    match v {
        Some(x) => format!("{:.2}", x),
        None => "—".to_string(),
    }
}

/// Run the TUI until the user quits or the run finishes. The function
/// takes ownership of the terminal (raw mode + alternate screen) and
/// restores it on exit.
///
/// `agg` is cloned from the executor's `Arc<Mutex<MetricsAggregator>>`,
/// which means the TUI sees live updates without blocking the runner.
/// `duration_secs` controls the countdown shown in the top bar.
/// `cancel` is notified when the user quits via `q` / `Esc` / `c`.
///
/// `paused` is an optional shared flag; when set, the user pressing
/// `p` toggles it. The executor is expected to read this flag before
/// acquiring a permit, but the integration is owned by the runner.
/// When the caller passes `None`, the pause key is a no-op.
pub fn run_tui(
    agg: Arc<Mutex<MetricsAggregator>>,
    cfg: &RunConfig,
    duration_secs: u64,
    cancel: Arc<tokio::sync::Notify>,
    paused: Option<Arc<AtomicBool>>,
) -> Result<()> {
    use crossterm::event::{DisableMouseCapture, EnableMouseCapture};
    use crossterm::execute;
    use crossterm::terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
    };
    use std::io::stdout;

    let mut stdout = stdout();
    enable_raw_mode().map_err(|e| crate::error::Error::Other(format!("raw mode: {e}")))?;
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)
        .map_err(|e| crate::error::Error::Other(format!("alt screen: {e}")))?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)
        .map_err(|e| crate::error::Error::Other(format!("terminal: {e}")))?;

    let result = tui_loop(&mut terminal, agg, cfg, duration_secs, &cancel, paused.as_ref());

    // Always restore the terminal.
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();
    result
}

fn tui_loop<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    agg: Arc<Mutex<MetricsAggregator>>,
    cfg: &RunConfig,
    duration_secs: u64,
    cancel: &Arc<tokio::sync::Notify>,
    paused: Option<&Arc<AtomicBool>>,
) -> Result<()> {
    let run_started = Instant::now();
    let mut last_draw = Instant::now();
    let draw_interval = Duration::from_millis(250);
    loop {
        // Snapshot under the lock; release before drawing.
        let snap = {
            let g = agg.lock().unwrap();
            Snapshot::from_aggregator(&g, run_started, duration_secs)
        };
        if last_draw.elapsed() >= draw_interval {
            terminal
                .draw(|f| {
                    let area = f.size();
                    let snap2 = {
                        let g = agg.lock().unwrap();
                        Snapshot::from_aggregator(&g, run_started, duration_secs)
                    };
                    f.render_widget(Clear, area);
                    render_to_buffer(&snap2, cfg, area, f.buffer_mut());
                })
                .map_err(|e| crate::error::Error::Other(format!("draw: {e}")))?;
            last_draw = Instant::now();
        }
        // Drain input with a short timeout.
        if crossterm::event::poll(Duration::from_millis(50)).unwrap_or(false) {
            if let Ok(Event::Key(k)) = crossterm::event::read() {
                let action = parse_key_event(k);
                match action {
                    TuiAction::Quit => {
                        cancel.notify_waiters();
                        return Ok(());
                    }
                    TuiAction::Pause => {
                        if let Some(p) = paused {
                            let cur = p.load(Ordering::SeqCst);
                            p.store(!cur, Ordering::SeqCst);
                        }
                    }
                    TuiAction::None => {}
                }
            }
        }
        // Stop if the run has fully elapsed.
        if run_started.elapsed().as_secs() >= duration_secs {
            return Ok(());
        }
        // Avoid an unused warning for the outer snap.
        // Avoid an unused warning for the outer snap.
        let _ = snap;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
    };

    fn base_cfg() -> RunConfig {
        RunConfig {
            run_id: "abcd1234-5678-90ab-cdef-1234567890ab".into(),
            target: "https://api.example.com/v1/chat/completions".into(),
            api: ApiKind::Openai,
            model: "gpt-3.5-turbo".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 4.0 },
            max_tokens: 32,
            stream: true,
            target_rps: 4.0,
            duration_secs: 30,
            concurrency: 16,
            tag: Some("tui-test".into()),
            api_key: None,
            started_at: chrono::Local::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
        }
    }

    /// Render to a String by walking the buffer row-by-row.
    fn render_to_string(snap: &Snapshot, cfg: &RunConfig) -> String {
        let area = Rect::new(0, 0, 80, 28);
        let mut buf = Buffer::empty(area);
        render_to_buffer(snap, cfg, area, &mut buf);
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                let cell = buf.get(x, y);
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn render_produces_nonempty_output_with_empty_aggregator() {
        let cfg = base_cfg();
        let agg = MetricsAggregator::new();
        let snap = Snapshot::from_aggregator(&agg, Instant::now(), 10);
        let s = render_to_string(&snap, &cfg);
        assert!(!s.is_empty(), "rendered output should be non-empty");
        // The short run id (first 8 chars of the uuid) appears in the
        // top bar.
        assert!(s.contains("abcd1234"), "short run id should appear");
        // Counters should be 0.
        assert!(s.contains("0/0"), "zero counters expected");
    }

    #[test]
    fn render_includes_target_and_model() {
        let mut cfg = base_cfg();
        // Use a short target so the top bar doesn't truncate the
        // model name.
        cfg.target = "http://x/y".into();
        let agg = MetricsAggregator::new();
        let snap = Snapshot::from_aggregator(&agg, Instant::now(), 10);
        let s = render_to_string(&snap, &cfg);
        // The model name appears in the top bar.
        assert!(s.contains("gpt-3.5-turbo"), "model missing: {s}");
        // The full target must appear too.
        assert!(s.contains("http://x/y"), "target missing: {s}");
    }

    #[test]
    fn render_includes_percentile_labels() {
        let cfg = base_cfg();
        let agg = MetricsAggregator::new();
        let snap = Snapshot::from_aggregator(&agg, Instant::now(), 10);
        let s = render_to_string(&snap, &cfg);
        assert!(s.contains("p50"), "p50 label missing");
        assert!(s.contains("p99"), "p99 label missing");
        assert!(s.contains("TTFT"), "TTFT label missing");
        assert!(s.contains("ITL"), "ITL label missing");
        assert!(s.contains("TPS"), "TPS label missing");
        assert!(s.contains("Latency"), "Latency label missing");
    }

    #[test]
    fn render_handles_no_aggregator_data() {
        let cfg = base_cfg();
        // Empty aggregator: all percentiles are None; the renderer
        // should emit em-dashes and not panic.
        let agg = MetricsAggregator::new();
        let snap = Snapshot::from_aggregator(&agg, Instant::now(), 30);
        let s = render_to_string(&snap, &cfg);
        assert!(s.contains("—"), "missing em-dash for empty histograms");
    }

    #[test]
    fn parse_key_quit_returns_cancel() {
        let k = KeyEvent::new(KeyCode::Char('q'), crossterm::event::KeyModifiers::NONE);
        assert_eq!(parse_key_event(k), TuiAction::Quit);
        let k = KeyEvent::new(KeyCode::Esc, crossterm::event::KeyModifiers::NONE);
        assert_eq!(parse_key_event(k), TuiAction::Quit);
    }

    #[test]
    fn parse_key_pause_returns_pause() {
        let k = KeyEvent::new(KeyCode::Char('p'), crossterm::event::KeyModifiers::NONE);
        assert_eq!(parse_key_event(k), TuiAction::Pause);
    }

    #[test]
    fn parse_key_other_returns_none() {
        let k = KeyEvent::new(KeyCode::Char('x'), crossterm::event::KeyModifiers::NONE);
        assert_eq!(parse_key_event(k), TuiAction::None);
        let k = KeyEvent::new(KeyCode::Enter, crossterm::event::KeyModifiers::NONE);
        assert_eq!(parse_key_event(k), TuiAction::None);
        // Release events are ignored.
        let mut k = KeyEvent::new(KeyCode::Char('q'), crossterm::event::KeyModifiers::NONE);
        k.kind = KeyEventKind::Release;
        assert_eq!(parse_key_event(k), TuiAction::None);
    }

    #[test]
    fn snapshot_from_aggregator_computes_basics() {
        let cfg = base_cfg();
        let mut agg = MetricsAggregator::new();
        // Inject a single request record so percentiles are non-empty.
        let mut m = crate::client::RequestMetrics::default();
        m.status = 200;
        m.ttft = Some(Duration::from_millis(20));
        m.itl_samples = vec![Duration::from_millis(15)];
        m.completion_tokens = 5;
        m.total_duration = Duration::from_millis(100);
        m.started_at = chrono::Utc::now();
        m.finished_at = chrono::Utc::now();
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let snap = Snapshot::from_aggregator(&agg, Instant::now(), cfg.duration_secs);
        assert_eq!(snap.total, 1);
        assert_eq!(snap.success, 1);
        assert_eq!(snap.errors, 0);
        assert!(snap.latency_p50_ms.is_some());
    }
}
