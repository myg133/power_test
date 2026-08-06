//! Historical run comparison (M3).
//!
//! Given two saved runs, compute a side-by-side diff of the headline
//! metrics and render it as plain text (with ANSI color, terminal-aware)
//! and as a self-contained HTML page with bar charts.
//!
//! Conventions:
//! - `a` is the **baseline** (left column / left side). Deltas are
//!   `b - a`. Improvement direction is metric-specific (e.g. latency
//!   down is better, RPS up is better).
//! - When the two runs have incompatible shapes (different target,
//!   different model, or different load-pattern kind), we still
//!   produce the diff but emit a warning at the top.

use std::fmt::Write as _;
use std::io::IsTerminal;

use serde::Serialize;

use crate::config::{LoadPattern, RunConfig, RunStatus};
use crate::runner::{HistKind, MetricsAggregator, RequestRecord};

/// Inputs to [`compare`]. Both sides must be loaded from disk by the
/// caller; this module does no I/O.
#[derive(Debug, Clone)]
pub struct CompareInputs {
    pub cfg_a: RunConfig,
    pub records_a: Vec<RequestRecord>,
    pub status_a: RunStatus,
    pub cfg_b: RunConfig,
    pub records_b: Vec<RequestRecord>,
    pub status_b: RunStatus,
}

impl CompareInputs {
    pub fn run_id_a(&self) -> &str {
        &self.cfg_a.run_id
    }
    pub fn run_id_b(&self) -> &str {
        &self.cfg_b.run_id
    }
}

/// A single numeric diff with an absolute and a percent change.
/// `abs` is `b - a`; `pct` is `100 * (b - a) / a` (or `None` if a is 0).
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct Delta {
    pub abs: f64,
    pub pct: Option<f64>,
}

impl Delta {
    pub fn new(a: f64, b: f64) -> Self {
        let abs = b - a;
        let pct = if a.abs() > f64::EPSILON {
            Some(100.0 * abs / a)
        } else {
            None
        }
        .map(|p| {
            // Clamp silly tiny noise to zero for display; this avoids
            // "0.0001%" when the values are huge and equal-ish.
            if p.abs() < 1e-6 {
                0.0
            } else {
                p
            }
        });
        Self { abs, pct }
    }
}

/// The full diff of two runs. All fields are pre-computed in
/// [`compute`]; renderers are pure.
#[derive(Debug, Clone)]
pub struct MetricDiff {
    /// Achieved requests per second over the configured duration.
    pub achieved_rps: Delta,
    /// Total number of completed requests (success + error).
    pub total_requests: Delta,
    /// Successful requests as a percentage of total.
    pub success_rate: Delta,
    /// End-to-end latency p50 / p90 / p99 / p99.9 in milliseconds.
    pub latency: LatencyQuartiles,
    /// Time-to-first-token p50 / p99 in milliseconds.
    pub ttft: LatencyPair,
    /// Inter-token latency mean / p99 in milliseconds.
    pub itl: LatencyPair,
    /// Tokens-per-second mean / p99.
    pub tps: LatencyPair,
    /// Total completion tokens.
    pub total_tokens: Delta,
    /// Wall-clock duration of the run, in seconds (with fractional part).
    pub duration_secs: Delta,
}

#[derive(Debug, Clone, Copy)]
pub struct LatencyQuartiles {
    pub p50: Delta,
    pub p90: Delta,
    pub p99: Delta,
    pub p99_9: Delta,
}

#[derive(Debug, Clone, Copy)]
pub struct LatencyPair {
    pub first: Delta,
    pub second: Delta,
}

/// Compute the diff from two `(RunConfig, RequestRecord[])` pairs plus
/// any compatibility warnings.
pub fn compute(inputs: &CompareInputs) -> (MetricDiff, Vec<String>) {
    let agg_a = MetricsAggregator::from_records(&inputs.records_a);
    let agg_b = MetricsAggregator::from_records(&inputs.records_b);
    let stats_a = AggregatorSnapshot::from(&agg_a, &inputs.cfg_a);
    let stats_b = AggregatorSnapshot::from(&agg_b, &inputs.cfg_b);

    let duration_a = inputs.cfg_a.duration_secs as f64;
    let duration_b = inputs.cfg_b.duration_secs as f64;

    let diff = MetricDiff {
        achieved_rps: Delta::new(stats_a.achieved_rps, stats_b.achieved_rps),
        total_requests: Delta::new(
            stats_a.total_requests as f64,
            stats_b.total_requests as f64,
        ),
        success_rate: Delta::new(stats_a.success_rate, stats_b.success_rate),
        latency: LatencyQuartiles {
            p50: Delta::new(stats_a.latency_p50_ms, stats_b.latency_p50_ms),
            p90: Delta::new(stats_a.latency_p90_ms, stats_b.latency_p90_ms),
            p99: Delta::new(stats_a.latency_p99_ms, stats_b.latency_p99_ms),
            p99_9: Delta::new(stats_a.latency_p999_ms, stats_b.latency_p999_ms),
        },
        ttft: LatencyPair {
            first: Delta::new(stats_a.ttft_p50_ms, stats_b.ttft_p50_ms),
            second: Delta::new(stats_a.ttft_p99_ms, stats_b.ttft_p99_ms),
        },
        itl: LatencyPair {
            first: Delta::new(stats_a.itl_mean_ms, stats_b.itl_mean_ms),
            second: Delta::new(stats_a.itl_p99_ms, stats_b.itl_p99_ms),
        },
        tps: LatencyPair {
            first: Delta::new(stats_a.tps_mean, stats_b.tps_mean),
            second: Delta::new(stats_a.tps_p99, stats_b.tps_p99),
        },
        total_tokens: Delta::new(
            stats_a.total_completion_tokens as f64,
            stats_b.total_completion_tokens as f64,
        ),
        duration_secs: Delta::new(duration_a, duration_b),
    };

    let warnings = shape_warnings(&inputs.cfg_a, &inputs.cfg_b);
    (diff, warnings)
}

/// Warnings to print at the top of the diff when the two runs are not
/// directly comparable. We do NOT block; the diff is still produced.
fn shape_warnings(a: &RunConfig, b: &RunConfig) -> Vec<String> {
    let mut w = Vec::new();
    if a.target != b.target {
        w.push(format!(
            "different targets: '{}' vs '{}'",
            a.target, b.target
        ));
    }
    if a.model != b.model {
        w.push(format!("different models: '{}' vs '{}'", a.model, b.model));
    }
    // M6g: also warn when the alias (group key) differs —
    // different aliases imply these runs are in different
    // history subdirectories and were never meant to be
    // compared. This is the inverse of the compare-with
    // dropdown's filter: if list_runs_by_alias wouldn't
    // return the other run, the diff is probably
    // meaningless.
    let a_key = crate::storage::effective_group_key(&a.model, a.model_alias.as_deref());
    let b_key = crate::storage::effective_group_key(&b.model, b.model_alias.as_deref());
    if a_key != b_key {
        w.push(format!(
            "different model alias: '{}' vs '{}' (runs are in different history subdirectories)",
            a_key, b_key
        ));
    }
    if pattern_kind(&a.pattern) != pattern_kind(&b.pattern) {
        w.push(format!(
            "different load patterns: '{}' vs '{}'",
            pattern_kind(&a.pattern),
            pattern_kind(&b.pattern)
        ));
    }
    w
}

fn pattern_kind(p: &LoadPattern) -> &'static str {
    match p {
        LoadPattern::Constant { .. } => "constant",
        LoadPattern::Ramp { .. } => "ramp",
        LoadPattern::Spike { .. } => "spike",
        LoadPattern::Soak { .. } => "soak",
    }
}

/// Pre-aggregated values for one run, so the diff and renderers don't
/// have to call `MetricsAggregator` methods multiple times.
#[derive(Debug, Clone, Copy)]
struct AggregatorSnapshot {
    achieved_rps: f64,
    total_requests: u64,
    success_rate: f64,
    latency_p50_ms: f64,
    latency_p90_ms: f64,
    latency_p99_ms: f64,
    latency_p999_ms: f64,
    ttft_p50_ms: f64,
    ttft_p99_ms: f64,
    itl_mean_ms: f64,
    itl_p99_ms: f64,
    tps_mean: f64,
    tps_p99: f64,
    total_completion_tokens: u64,
}

impl AggregatorSnapshot {
    fn from(agg: &MetricsAggregator, cfg: &RunConfig) -> Self {
        let to_ms = |us: Option<u64>| us.map(|v| v as f64 / 1000.0).unwrap_or(0.0);
        let itl_mean_ms = agg.mean(HistKind::Itl).map(|v| v / 1000.0).unwrap_or(0.0);
        let achieved_rps = if cfg.duration_secs > 0 {
            agg.total_requests() as f64 / cfg.duration_secs as f64
        } else {
            0.0
        };
        let success_rate = if agg.total_requests() > 0 {
            100.0 * agg.success_count() as f64 / agg.total_requests() as f64
        } else {
            0.0
        };
        Self {
            achieved_rps,
            total_requests: agg.total_requests(),
            success_rate,
            latency_p50_ms: to_ms(agg.percentile(HistKind::Latency, 50.0)),
            latency_p90_ms: to_ms(agg.percentile(HistKind::Latency, 90.0)),
            latency_p99_ms: to_ms(agg.percentile(HistKind::Latency, 99.0)),
            latency_p999_ms: to_ms(agg.percentile(HistKind::Latency, 99.9)),
            ttft_p50_ms: to_ms(agg.percentile(HistKind::Ttft, 50.0)),
            ttft_p99_ms: to_ms(agg.percentile(HistKind::Ttft, 99.0)),
            itl_mean_ms,
            itl_p99_ms: to_ms(agg.percentile(HistKind::Itl, 99.0)),
            tps_mean: agg.tps_mean(),
            tps_p99: agg.tps_percentile(99.0),
            total_completion_tokens: agg.total_completion_tokens(),
        }
    }
}

/// Direction a metric should move for a "good" result. Used by the
/// renderer to color deltas green/red.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Higher is better (e.g. achieved RPS, success rate, TPS, total tokens).
    Up,
    /// Lower is better (e.g. latency, ITL, TTFT).
    Down,
    /// Neutral: just show the number, no color judgment.
    Neutral,
}

/// Decide whether `delta` is an improvement, regression, or no change
/// for a metric with the given `direction`. Tolerance avoids flagging
/// 0.001% rounding noise as a regression.
pub fn color_class(delta: Delta, direction: Direction) -> ColorClass {
    if direction == Direction::Neutral || delta.pct.is_none() {
        return ColorClass::Neutral;
    }
    // Sub-0.5% changes are noise; treat as neutral.
    let pct = delta.pct.unwrap_or(0.0);
    if pct.abs() < 0.5 {
        return ColorClass::Neutral;
    }
    let positive = pct > 0.0;
    match direction {
        Direction::Up => {
            if positive {
                ColorClass::Good
            } else {
                ColorClass::Bad
            }
        }
        Direction::Down => {
            if positive {
                ColorClass::Bad
            } else {
                ColorClass::Good
            }
        }
        Direction::Neutral => ColorClass::Neutral,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorClass {
    Good,
    Bad,
    Neutral,
}

// ---------------------------------------------------------------------------
// Text rendering
// ---------------------------------------------------------------------------

/// Render the diff as a plain-text table. The output is suitable for
/// `println!`. When `color` is `true`, deltas are ANSI-colored
/// (improvement = green, regression = red, neutral = grey).
pub fn render_text(inputs: &CompareInputs, color: bool) -> String {
    let (diff, warnings) = compute(inputs);
    let agg_a = MetricsAggregator::from_records(&inputs.records_a);
    let agg_b = MetricsAggregator::from_records(&inputs.records_b);
    let stats_a = AggregatorSnapshot::from(&agg_a, &inputs.cfg_a);
    let stats_b = AggregatorSnapshot::from(&agg_b, &inputs.cfg_b);

    let mut out = String::new();
    let _ = writeln!(out, "power_test compare — {} vs {}", inputs.run_id_a(), inputs.run_id_b());
    let _ = writeln!(out, "{}", "=".repeat(72));
    if !warnings.is_empty() {
        for w in &warnings {
            let _ = writeln!(out, "warning: {w}");
        }
        let _ = writeln!(out, "{}", "-".repeat(72));
    }
    let _ = writeln!(
        out,
        "  baseline (a): target={} model={} rps={:.2} duration={}s",
        inputs.cfg_a.target, inputs.cfg_a.model, inputs.cfg_a.target_rps, inputs.cfg_a.duration_secs
    );
    let _ = writeln!(
        out,
        "  candidate (b): target={} model={} rps={:.2} duration={}s",
        inputs.cfg_b.target, inputs.cfg_b.model, inputs.cfg_b.target_rps, inputs.cfg_b.duration_secs
    );
    let _ = writeln!(out);

    // Helper to render one row: name, a, b, delta (colored), pct.
    // `fmt_b` and `fmt_delta` are display formatters. We use a closure
    // style for `fmt_a` to keep latency-p99 etc. consistent.
    let helper = |out: &mut String,
                  name: &str,
                  a: String,
                  b: String,
                  delta: Delta,
                  direction: Direction,
                  pct: bool| {
        let cls = color_class(delta, direction);
        let delta_str = format_delta(delta, pct);
        let (a_col, b_col) = if color {
            (
                colorize(&a, ColorClass::Neutral),
                colorize(&b, ColorClass::Neutral),
            )
        } else {
            (a, b)
        };
        let _ = writeln!(
            out,
            "  {:<22}  {:>14}  {:>14}  {:>20}  {}",
            name, a_col, b_col, delta_str, color_label(cls, color)
        );
    };

    let _ = writeln!(
        out,
        "  {:<22}  {:>14}  {:>14}  {:>20}  {}",
        "metric", "a", "b", "delta", ""
    );
    let _ = writeln!(out, "  {}", "-".repeat(76));

    helper(
        &mut out,
        "achieved rps",
        format!("{:.2}", stats_a.achieved_rps),
        format!("{:.2}", stats_b.achieved_rps),
        diff.achieved_rps,
        Direction::Up,
        true,
    );
    helper(
        &mut out,
        "total requests",
        format!("{}", stats_a.total_requests),
        format!("{}", stats_b.total_requests),
        diff.total_requests,
        Direction::Up,
        false,
    );
    helper(
        &mut out,
        "success rate %",
        format!("{:.2}", stats_a.success_rate),
        format!("{:.2}", stats_b.success_rate),
        diff.success_rate,
        Direction::Up,
        true,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "  latency (ms)");
    helper(
        &mut out,
        "  p50",
        format!("{:.2}", stats_a.latency_p50_ms),
        format!("{:.2}", stats_b.latency_p50_ms),
        diff.latency.p50,
        Direction::Down,
        true,
    );
    helper(
        &mut out,
        "  p90",
        format!("{:.2}", stats_a.latency_p90_ms),
        format!("{:.2}", stats_b.latency_p90_ms),
        diff.latency.p90,
        Direction::Down,
        true,
    );
    helper(
        &mut out,
        "  p99",
        format!("{:.2}", stats_a.latency_p99_ms),
        format!("{:.2}", stats_b.latency_p99_ms),
        diff.latency.p99,
        Direction::Down,
        true,
    );
    helper(
        &mut out,
        "  p99.9",
        format!("{:.2}", stats_a.latency_p999_ms),
        format!("{:.2}", stats_b.latency_p999_ms),
        diff.latency.p99_9,
        Direction::Down,
        true,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "  ttft (ms)");
    helper(
        &mut out,
        "  p50",
        format!("{:.2}", stats_a.ttft_p50_ms),
        format!("{:.2}", stats_b.ttft_p50_ms),
        diff.ttft.first,
        Direction::Down,
        true,
    );
    helper(
        &mut out,
        "  p99",
        format!("{:.2}", stats_a.ttft_p99_ms),
        format!("{:.2}", stats_b.ttft_p99_ms),
        diff.ttft.second,
        Direction::Down,
        true,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "  itl (ms)");
    helper(
        &mut out,
        "  mean",
        format!("{:.2}", stats_a.itl_mean_ms),
        format!("{:.2}", stats_b.itl_mean_ms),
        diff.itl.first,
        Direction::Down,
        true,
    );
    helper(
        &mut out,
        "  p99",
        format!("{:.2}", stats_a.itl_p99_ms),
        format!("{:.2}", stats_b.itl_p99_ms),
        diff.itl.second,
        Direction::Down,
        true,
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "  tps (tokens/sec)");
    helper(
        &mut out,
        "  mean",
        format!("{:.2}", stats_a.tps_mean),
        format!("{:.2}", stats_b.tps_mean),
        diff.tps.first,
        Direction::Up,
        true,
    );
    helper(
        &mut out,
        "  p99",
        format!("{:.2}", stats_a.tps_p99),
        format!("{:.2}", stats_b.tps_p99),
        diff.tps.second,
        Direction::Up,
        true,
    );
    let _ = writeln!(out);
    helper(
        &mut out,
        "total tokens",
        format!("{}", stats_a.total_completion_tokens),
        format!("{}", stats_b.total_completion_tokens),
        diff.total_tokens,
        Direction::Up,
        false,
    );
    helper(
        &mut out,
        "duration (s)",
        format!("{:.2}", inputs.cfg_a.duration_secs as f64),
        format!("{:.2}", inputs.cfg_b.duration_secs as f64),
        diff.duration_secs,
        Direction::Neutral,
        true,
    );

    out
}

fn format_delta(d: Delta, show_pct: bool) -> String {
    let sign = if d.abs >= 0.0 { "+" } else { "" };
    if show_pct {
        match d.pct {
            Some(p) => format!("{}{:.2}  ({}{:.2}%)", sign, d.abs, sign, p),
            None => format!("{}{:.2}  (n/a)", sign, d.abs),
        }
    } else {
        format!("{}{:.0}", sign, d.abs)
    }
}

fn color_label(cls: ColorClass, color: bool) -> &'static str {
    if !color {
        ""
    } else {
        match cls {
            ColorClass::Good => "good",
            ColorClass::Bad => "bad",
            ColorClass::Neutral => "neutral",
        }
    }
}

// ---------------------------------------------------------------------------
// ANSI color helpers
// ---------------------------------------------------------------------------

const ANSI_RESET: &str = "\x1b[0m";
const ANSI_GREEN: &str = "\x1b[32m";
const ANSI_RED: &str = "\x1b[31m";
const ANSI_GREY: &str = "\x1b[90m";

/// Wrap `s` in ANSI color codes according to `cls`. Caller is expected
/// to have already decided whether stdout/stderr is a TTY.
pub fn colorize(s: &str, cls: ColorClass) -> String {
    let code = match cls {
        ColorClass::Good => ANSI_GREEN,
        ColorClass::Bad => ANSI_RED,
        ColorClass::Neutral => ANSI_GREY,
    };
    format!("{code}{s}{ANSI_RESET}")
}

/// Should we emit ANSI color codes? True only when stdout is a real
/// TTY (not piped, not redirected).
pub fn stdout_supports_color() -> bool {
    std::io::stdout().is_terminal()
}

// ---------------------------------------------------------------------------
// HTML rendering
// ---------------------------------------------------------------------------

/// Render a self-contained HTML compare page.
pub fn render_html(inputs: &CompareInputs) -> String {
    let (diff, warnings) = compute(inputs);
    let agg_a = MetricsAggregator::from_records(&inputs.records_a);
    let agg_b = MetricsAggregator::from_records(&inputs.records_b);
    let stats_a = AggregatorSnapshot::from(&agg_a, &inputs.cfg_a);
    let stats_b = AggregatorSnapshot::from(&agg_b, &inputs.cfg_b);

    let title = format!(
        "power_test compare — {} vs {}",
        inputs.run_id_a(),
        inputs.run_id_b()
    );

    let warning_html = if warnings.is_empty() {
        String::new()
    } else {
        let items: String = warnings
            .iter()
            .map(|w| format!(r#"<li>{}</li>"#, html_escape(w)))
            .collect();
        format!(
            r#"<div class="warnings"><strong>warnings:</strong><ul>{}</ul></div>"#,
            items
        )
    };

    let data_json = serde_json::json!({
        "a": {
            "run_id": inputs.cfg_a.run_id,
            "target": inputs.cfg_a.target,
            "model": inputs.cfg_a.model,
            "target_rps": inputs.cfg_a.target_rps,
            "duration_secs": inputs.cfg_a.duration_secs,
            "total_requests": stats_a.total_requests,
            "success_rate": stats_a.success_rate,
            "achieved_rps": stats_a.achieved_rps,
            "status": inputs.status_a.as_str(),
            "latency_ms": {
                "p50": stats_a.latency_p50_ms,
                "p90": stats_a.latency_p90_ms,
                "p99": stats_a.latency_p99_ms,
                "p99_9": stats_a.latency_p999_ms,
            },
        },
        "b": {
            "run_id": inputs.cfg_b.run_id,
            "target": inputs.cfg_b.target,
            "model": inputs.cfg_b.model,
            "target_rps": inputs.cfg_b.target_rps,
            "duration_secs": inputs.cfg_b.duration_secs,
            "total_requests": stats_b.total_requests,
            "success_rate": stats_b.success_rate,
            "achieved_rps": stats_b.achieved_rps,
            "status": inputs.status_b.as_str(),
            "latency_ms": {
                "p50": stats_b.latency_p50_ms,
                "p90": stats_b.latency_p90_ms,
                "p99": stats_b.latency_p99_ms,
                "p99_9": stats_b.latency_p999_ms,
            },
        },
        "diff": diff_to_json(&diff),
    })
    .to_string();

    let diff_rows = build_diff_rows(&diff, &stats_a, &stats_b, &inputs.cfg_a, &inputs.cfg_b);

    format!(
        r##"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <title>{title}</title>
  <script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
  <style>
    :root {{
      --bg: #0e1116;
      --fg: #e6edf3;
      --muted: #8b949e;
      --card: #161b22;
      --border: #30363d;
      --accent: #58a6ff;
      --good: #3fb950;
      --bad: #f85149;
    }}
    * {{ box-sizing: border-box; }}
    body {{
      margin: 0;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto,
                   Oxygen, Ubuntu, sans-serif;
      background: var(--bg);
      color: var(--fg);
      line-height: 1.5;
    }}
    .wrap {{ max-width: 1200px; margin: 0 auto; padding: 24px; }}
    h1 {{ margin: 0 0 8px 0; font-size: 24px; }}
    h2 {{ margin: 32px 0 12px 0; font-size: 18px; border-bottom: 1px solid var(--border); padding-bottom: 6px; }}
    .sub {{ color: var(--muted); margin-bottom: 16px; }}
    .warnings {{
      background: rgba(248,81,73,0.08);
      border: 1px solid rgba(248,81,73,0.4);
      color: var(--fg);
      padding: 10px 14px;
      border-radius: 8px;
      margin-bottom: 16px;
    }}
    .warnings ul {{ margin: 6px 0 0 0; padding-left: 20px; }}
    .header-cards {{
      display: grid;
      grid-template-columns: 1fr 1fr;
      gap: 16px;
      margin-bottom: 24px;
    }}
    .run-card {{
      background: var(--card);
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 16px;
    }}
    .run-card h3 {{
      margin: 0 0 10px 0;
      font-family: ui-monospace, SFMono-Regular, monospace;
      font-size: 14px;
      color: var(--accent);
    }}
    .run-card dl {{
      display: grid;
      grid-template-columns: max-content 1fr;
      gap: 4px 16px;
      margin: 0;
      font-size: 13px;
    }}
    .run-card dt {{ color: var(--muted); }}
    .run-card dd {{ margin: 0; font-family: ui-monospace, SFMono-Regular, monospace; }}
    table {{ width: 100%; border-collapse: collapse; background: var(--card); border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }}
    th, td {{ text-align: left; padding: 8px 12px; border-bottom: 1px solid var(--border); font-size: 13px; }}
    th {{ color: var(--muted); font-weight: 500; font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; background: rgba(255,255,255,0.02); }}
    tr:last-child td {{ border-bottom: none; }}
    td.num {{ font-family: ui-monospace, SFMono-Regular, monospace; text-align: right; }}
    .delta.good {{ color: var(--good); }}
    .delta.bad {{ color: var(--bad); }}
    .delta.neutral {{ color: var(--muted); }}
    .charts {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; }}
    .chart-card {{ background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 16px; min-height: 320px; }}
    .chart-card h3 {{ margin: 0 0 12px 0; font-size: 14px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }}
    .chart-card canvas {{ max-height: 280px; }}
    footer {{ margin-top: 48px; padding-top: 16px; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px; text-align: center; }}
  </style>
</head>
<body>
  <div class="wrap">
    <h1>power_test compare</h1>
    <div class="sub">{a_id} <span style="color:var(--muted)">vs</span> {b_id} · generated <code>{generated}</code></div>
    {warning_html}

    <div class="header-cards">
      <div class="run-card">
        <h3>a · {a_id}</h3>
        <dl>
          <dt>target</dt><dd>{a_target}</dd>
          <dt>model</dt><dd>{a_model}</dd>
          <dt>target rps</dt><dd>{a_rps:.2}</dd>
          <dt>duration</dt><dd>{a_dur}s</dd>
          <dt>total requests</dt><dd>{a_total}</dd>
          <dt>success rate</dt><dd>{a_sr:.1}%</dd>
          <dt>status</dt><dd>{a_status}</dd>
        </dl>
      </div>
      <div class="run-card">
        <h3>b · {b_id}</h3>
        <dl>
          <dt>target</dt><dd>{b_target}</dd>
          <dt>model</dt><dd>{b_model}</dd>
          <dt>target rps</dt><dd>{b_rps:.2}</dd>
          <dt>duration</dt><dd>{b_dur}s</dd>
          <dt>total requests</dt><dd>{b_total}</dd>
          <dt>success rate</dt><dd>{b_sr:.1}%</dd>
          <dt>status</dt><dd>{b_status}</dd>
        </dl>
      </div>
    </div>

    <h2>Diff</h2>
    <table>
      <thead><tr><th>metric</th><th style="text-align:right">a</th><th style="text-align:right">b</th><th style="text-align:right">delta</th><th style="text-align:right">%</th></tr></thead>
      <tbody>{diff_rows}</tbody>
    </table>

    <h2>Latency percentiles (ms)</h2>
    <div class="charts">
      <div class="chart-card"><h3>{a_id} latency (ms)</h3><canvas id="latA"></canvas></div>
      <div class="chart-card"><h3>{b_id} latency (ms)</h3><canvas id="latB"></canvas></div>
    </div>

    <footer>powered by power_test v{version}</footer>
  </div>

  <script id="compare-data" type="application/json">{data_json}</script>
  <script>
    const DATA = JSON.parse(document.getElementById('compare-data').textContent);
    const palette = ['#58a6ff', '#3fb950', '#d29922', '#a371f7', '#f85149', '#8b949e'];
    const chartFont = {{ size: 12 }};

    function makeBar(canvasId, values) {{
      new Chart(document.getElementById(canvasId), {{
        type: 'bar',
        data: {{
          labels: ['p50', 'p90', 'p99', 'p99.9'],
          datasets: [{{
            label: 'Latency (ms)',
            data: values,
            backgroundColor: palette[0],
          }}]
        }},
        options: {{
          responsive: true,
          maintainAspectRatio: false,
          plugins: {{ legend: {{ display: false }} }},
          scales: {{ y: {{ beginAtZero: true, ticks: {{ font: chartFont }} }}, x: {{ ticks: {{ font: chartFont }} }} }}
        }}
      }});
    }}

    makeBar('latA', [DATA.a.latency_ms.p50, DATA.a.latency_ms.p90, DATA.a.latency_ms.p99, DATA.a.latency_ms.p99_9]);
    makeBar('latB', [DATA.b.latency_ms.p50, DATA.b.latency_ms.p90, DATA.b.latency_ms.p99, DATA.b.latency_ms.p99_9]);
  </script>
</body>
</html>
"##,
        title = html_escape(&title),
        a_id = html_escape(inputs.run_id_a()),
        b_id = html_escape(inputs.run_id_b()),
        generated = chrono::Utc::now().to_rfc3339(),
        warning_html = warning_html,
        a_target = html_escape(&inputs.cfg_a.target),
        a_model = html_escape(&inputs.cfg_a.model),
        a_rps = inputs.cfg_a.target_rps,
        a_dur = inputs.cfg_a.duration_secs,
        a_total = stats_a.total_requests,
        a_sr = stats_a.success_rate,
        a_status = inputs.status_a.as_str(),
        b_target = html_escape(&inputs.cfg_b.target),
        b_model = html_escape(&inputs.cfg_b.model),
        b_rps = inputs.cfg_b.target_rps,
        b_dur = inputs.cfg_b.duration_secs,
        b_total = stats_b.total_requests,
        b_sr = stats_b.success_rate,
        b_status = inputs.status_b.as_str(),
        diff_rows = diff_rows,
        data_json = html_escape(&data_json),
        version = env!("CARGO_PKG_VERSION"),
    )
}

fn diff_to_json(d: &MetricDiff) -> serde_json::Value {
    serde_json::json!({
        "achieved_rps": d.achieved_rps,
        "total_requests": d.total_requests,
        "success_rate": d.success_rate,
        "latency": {
            "p50": d.latency.p50,
            "p90": d.latency.p90,
            "p99": d.latency.p99,
            "p99_9": d.latency.p99_9,
        },
        "ttft": { "p50": d.ttft.first, "p99": d.ttft.second },
        "itl":  { "mean": d.itl.first, "p99": d.itl.second },
        "tps":  { "mean": d.tps.first, "p99": d.tps.second },
        "total_tokens": d.total_tokens,
        "duration_secs": d.duration_secs,
    })
}

fn build_diff_rows(
    diff: &MetricDiff,
    a: &AggregatorSnapshot,
    b: &AggregatorSnapshot,
    cfg_a: &RunConfig,
    cfg_b: &RunConfig,
) -> String {
    let mut out = String::new();
    // (name, a, b, delta, direction, show_pct)
    let rows: Vec<(&str, String, String, Delta, Direction, bool)> = vec![
        (
            "achieved rps",
            format!("{:.2}", a.achieved_rps),
            format!("{:.2}", b.achieved_rps),
            diff.achieved_rps,
            Direction::Up,
            true,
        ),
        (
            "total requests",
            format!("{}", a.total_requests),
            format!("{}", b.total_requests),
            diff.total_requests,
            Direction::Up,
            false,
        ),
        (
            "success rate %",
            format!("{:.2}", a.success_rate),
            format!("{:.2}", b.success_rate),
            diff.success_rate,
            Direction::Up,
            true,
        ),
        (
            "latency p50 (ms)",
            format!("{:.2}", a.latency_p50_ms),
            format!("{:.2}", b.latency_p50_ms),
            diff.latency.p50,
            Direction::Down,
            true,
        ),
        (
            "latency p90 (ms)",
            format!("{:.2}", a.latency_p90_ms),
            format!("{:.2}", b.latency_p90_ms),
            diff.latency.p90,
            Direction::Down,
            true,
        ),
        (
            "latency p99 (ms)",
            format!("{:.2}", a.latency_p99_ms),
            format!("{:.2}", b.latency_p99_ms),
            diff.latency.p99,
            Direction::Down,
            true,
        ),
        (
            "latency p99.9 (ms)",
            format!("{:.2}", a.latency_p999_ms),
            format!("{:.2}", b.latency_p999_ms),
            diff.latency.p99_9,
            Direction::Down,
            true,
        ),
        (
            "ttft p50 (ms)",
            format!("{:.2}", a.ttft_p50_ms),
            format!("{:.2}", b.ttft_p50_ms),
            diff.ttft.first,
            Direction::Down,
            true,
        ),
        (
            "ttft p99 (ms)",
            format!("{:.2}", a.ttft_p99_ms),
            format!("{:.2}", b.ttft_p99_ms),
            diff.ttft.second,
            Direction::Down,
            true,
        ),
        (
            "itl mean (ms)",
            format!("{:.2}", a.itl_mean_ms),
            format!("{:.2}", b.itl_mean_ms),
            diff.itl.first,
            Direction::Down,
            true,
        ),
        (
            "itl p99 (ms)",
            format!("{:.2}", a.itl_p99_ms),
            format!("{:.2}", b.itl_p99_ms),
            diff.itl.second,
            Direction::Down,
            true,
        ),
        (
            "tps mean (tok/s)",
            format!("{:.2}", a.tps_mean),
            format!("{:.2}", b.tps_mean),
            diff.tps.first,
            Direction::Up,
            true,
        ),
        (
            "tps p99 (tok/s)",
            format!("{:.2}", a.tps_p99),
            format!("{:.2}", b.tps_p99),
            diff.tps.second,
            Direction::Up,
            true,
        ),
        (
            "total tokens",
            format!("{}", a.total_completion_tokens),
            format!("{}", b.total_completion_tokens),
            diff.total_tokens,
            Direction::Up,
            false,
        ),
        (
            "duration (s)",
            format!("{:.2}", cfg_a.duration_secs as f64),
            format!("{:.2}", cfg_b.duration_secs as f64),
            diff.duration_secs,
            Direction::Neutral,
            true,
        ),
    ];
    for (name, a_str, b_str, delta, direction, show_pct) in rows {
        let cls = color_class(delta, direction);
        let class = match cls {
            ColorClass::Good => "delta good",
            ColorClass::Bad => "delta bad",
            ColorClass::Neutral => "delta neutral",
        };
        let (delta_str, pct_str) = if show_pct {
            let sign = if delta.abs >= 0.0 { "+" } else { "" };
            let d = format!("{}{:.2}", sign, delta.abs);
            let p = match delta.pct {
                Some(p) => format!("{}{:.2}%", sign, p),
                None => "n/a".to_string(),
            };
            (d, p)
        } else {
            let sign = if delta.abs >= 0.0 { "+" } else { "" };
            (format!("{}{:.0}", sign, delta.abs), "—".to_string())
        };
        let _ = writeln!(
            out,
            "<tr><td>{name}</td><td class=\"num\">{a}</td><td class=\"num\">{b}</td><td class=\"num {cls}\">{d}</td><td class=\"num {cls}\">{p}</td></tr>",
            name = html_escape(name),
            a = a_str,
            b = b_str,
            cls = class,
            d = delta_str,
            p = pct_str,
        );
    }
    out
}

#[allow(dead_code)]
fn build_latency_chart_data(s: &AggregatorSnapshot) -> serde_json::Value {
    serde_json::json!({
        "labels": ["p50", "p90", "p99", "p99.9"],
        "values_ms": [
            s.latency_p50_ms, s.latency_p90_ms, s.latency_p99_ms, s.latency_p999_ms
        ],
    })
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
    };
    use std::time::Duration;

    fn cfg(run_id: &str, model: &str, target: &str, duration: u64, rps: f64) -> RunConfig {
        RunConfig {
            run_id: run_id.into(),
            target: target.into(),
            api: ApiKind::Openai,
            model: model.into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps },
            max_tokens: 16,
            stream: true,
            target_rps: rps,
            duration_secs: duration,
            concurrency: 8,
            tag: None,
            api_key: None,
            started_at: chrono::Local::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
        }
    }

    fn make_record(latency_ms: u64, ttft_ms: Option<u64>, itl: Vec<u64>, tokens: u32) -> RequestRecord {
        RequestRecord {
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            status: 200,
            error: None,
            total_duration_us: latency_ms * 1000,
            ttft_us: ttft_ms.map(|v| v * 1000),
            itl_samples_us: itl.into_iter().map(|v| v * 1000).collect(),
            completion_tokens: tokens,
            prompt_tokens: 5,
            estimated: false,
            session_id: None,
            session_turn: None,
            session_continuation: false,
            cache_creation_input_tokens: 0,
            cache_hit_input_tokens: 0,
        }
    }

    #[test]
    fn delta_basic_arithmetic() {
        let d = Delta::new(10.0, 12.0);
        assert!((d.abs - 2.0).abs() < 1e-9);
        assert!((d.pct.unwrap() - 20.0).abs() < 1e-9);

        let d = Delta::new(100.0, 95.0);
        assert!((d.abs + 5.0).abs() < 1e-9);
        assert!((d.pct.unwrap() + 5.0).abs() < 1e-9);

        let d = Delta::new(0.0, 5.0);
        assert_eq!(d.pct, None);
    }

    #[test]
    fn color_class_picks_good_bad_neutral() {
        // Big improvement (latency down): good.
        let d = Delta::new(100.0, 50.0);
        assert_eq!(color_class(d, Direction::Down), ColorClass::Good);

        // Big regression (latency up): bad.
        let d = Delta::new(100.0, 200.0);
        assert_eq!(color_class(d, Direction::Down), ColorClass::Bad);

        // RPS up: good.
        let d = Delta::new(10.0, 12.0);
        assert_eq!(color_class(d, Direction::Up), ColorClass::Good);

        // RPS down: bad.
        let d = Delta::new(10.0, 8.0);
        assert_eq!(color_class(d, Direction::Up), ColorClass::Bad);

        // Small change is neutral.
        let d = Delta::new(100.0, 100.4);
        assert_eq!(color_class(d, Direction::Up), ColorClass::Neutral);

        // Duration is always neutral.
        let d = Delta::new(60.0, 30.0);
        assert_eq!(color_class(d, Direction::Neutral), ColorClass::Neutral);
    }

    #[test]
    fn diff_is_correct_for_synthetic_runs() {
        // Build two runs with the same shape but different latencies.
        let cfg_a = cfg("a", "m", "http://x/v1/chat/completions", 2, 2.0);
        let cfg_b = cfg("b", "m", "http://x/v1/chat/completions", 2, 2.0);

        // a: 4 successful requests at 50ms.
        let records_a: Vec<RequestRecord> = (0..4)
            .map(|_| make_record(50, Some(20), vec![10, 10], 10))
            .collect();
        // b: 6 successful requests at 30ms.
        let records_b: Vec<RequestRecord> = (0..6)
            .map(|_| make_record(30, Some(15), vec![7, 7, 7], 10))
            .collect();

        let inputs = CompareInputs {
            cfg_a,
            records_a,
            status_a: RunStatus::Completed,
            cfg_b,
            records_b,
            status_b: RunStatus::Completed,
        };

        let (diff, warnings) = compute(&inputs);
        assert!(warnings.is_empty(), "expected no shape warnings, got {warnings:?}");

        // 4 / 2 = 2.0 vs 6 / 2 = 3.0; delta = 1.0
        assert!((diff.achieved_rps.abs - 1.0).abs() < 1e-6);
        assert!((diff.achieved_rps.pct.unwrap() - 50.0).abs() < 1e-6);

        // total requests: 4 -> 6
        assert!((diff.total_requests.abs - 2.0).abs() < 1e-6);

        // Both 100% success.
        assert!(diff.success_rate.abs.abs() < 1e-6);

        // Latency p50: 50 -> 30, delta -20.
        // HdrHistogram has ~0.1% precision, so allow a 1% tolerance.
        assert!((diff.latency.p50.abs + 20.0).abs() < 0.5);
        assert!((diff.latency.p50.pct.unwrap() + 40.0).abs() < 1.0);
    }

    #[test]
    fn shape_warnings_detect_incompatible_runs() {
        let cfg_a = cfg("a", "m1", "http://x/v1/chat/completions", 2, 2.0);
        let mut cfg_b = cfg("b", "m2", "http://x/v1/chat/completions", 2, 2.0);
        cfg_b.pattern = LoadPattern::Ramp {
            start: 1.0,
            end: 5.0,
            duration_secs: 2.0,
        };
        let cfg_c = cfg("c", "m1", "http://y/v1/chat/completions", 2, 2.0);

        let w = shape_warnings(&cfg_a, &cfg_b);
        assert!(w.iter().any(|s| s.contains("model")));
        assert!(w.iter().any(|s| s.contains("pattern")));

        let w = shape_warnings(&cfg_a, &cfg_c);
        assert!(w.iter().any(|s| s.contains("target")));

        let w = shape_warnings(&cfg_a, &cfg_a);
        assert!(w.is_empty());
    }

    #[test]
    fn text_render_includes_metrics_and_deltas() {
        let cfg_a = cfg("aaaa", "m", "http://x/v1/chat/completions", 2, 2.0);
        let cfg_b = cfg("bbbb", "m", "http://x/v1/chat/completions", 2, 2.0);
        let records_a = vec![make_record(50, Some(20), vec![10, 10], 10)];
        let records_b = vec![make_record(80, Some(30), vec![15, 15, 15], 12)];
        let inputs = CompareInputs {
            cfg_a,
            records_a,
            status_a: RunStatus::Completed,
            cfg_b,
            records_b,
            status_b: RunStatus::Completed,
        };

        let out = render_text(&inputs, false);
        assert!(out.contains("power_test compare"));
        assert!(out.contains("aaaa"));
        assert!(out.contains("bbbb"));
        assert!(out.contains("achieved rps"));
        assert!(out.contains("latency (ms)"));
        assert!(out.contains("tps (tokens/sec)"));
        assert!(out.contains("duration (s)"));
    }

    #[test]
    fn text_render_emits_ansi_when_color_enabled() {
        let cfg_a = cfg("a", "m", "http://x/v1/chat/completions", 2, 2.0);
        let cfg_b = cfg("b", "m", "http://x/v1/chat/completions", 2, 2.0);
        let records_a = vec![make_record(50, Some(20), vec![10, 10], 10)];
        let records_b = vec![make_record(20, Some(15), vec![7, 7], 12)]; // improved
        let inputs = CompareInputs {
            cfg_a,
            records_a,
            status_a: RunStatus::Completed,
            cfg_b,
            records_b,
            status_b: RunStatus::Completed,
        };
        let out_color = render_text(&inputs, true);
        let out_plain = render_text(&inputs, false);
        assert!(out_color.contains("\x1b["), "expected ANSI escape in color output");
        // The plain output should never contain ANSI escape sequences.
        assert!(!out_plain.contains('\x1b'));
    }

    #[test]
    fn html_render_contains_run_ids_and_metrics() {
        let cfg_a = cfg("run-aaa", "m", "http://x/v1/chat/completions", 2, 2.0);
        let cfg_b = cfg("run-bbb", "m", "http://x/v1/chat/completions", 2, 2.0);
        let records_a = vec![make_record(50, Some(20), vec![10, 10], 10)];
        let records_b = vec![make_record(80, Some(30), vec![15, 15, 15], 12)];
        let inputs = CompareInputs {
            cfg_a,
            records_a,
            status_a: RunStatus::Completed,
            cfg_b,
            records_b,
            status_b: RunStatus::Completed,
        };
        let out = render_html(&inputs);
        assert!(out.contains("run-aaa"));
        assert!(out.contains("run-bbb"));
        assert!(out.contains("chart.js"));
        assert!(out.contains("latency_ms"));
        assert!(out.contains("<table"));
    }

    #[test]
    fn html_render_does_not_panic_with_empty_data() {
        let cfg_a = cfg("a", "m", "http://x", 0, 1.0);
        let cfg_b = cfg("b", "m", "http://x", 0, 1.0);
        let inputs = CompareInputs {
            cfg_a,
            records_a: vec![],
            status_a: RunStatus::Completed,
            cfg_b,
            records_b: vec![],
            status_b: RunStatus::Completed,
        };
        // Just verify it doesn't panic and produces some HTML.
        let out = render_html(&inputs);
        assert!(out.contains("power_test compare"));
    }

    #[test]
    fn text_render_does_not_panic_with_empty_data() {
        let cfg_a = cfg("a", "m", "http://x", 0, 1.0);
        let cfg_b = cfg("b", "m", "http://x", 0, 1.0);
        let inputs = CompareInputs {
            cfg_a,
            records_a: vec![],
            status_a: RunStatus::Completed,
            cfg_b,
            records_b: vec![],
            status_b: RunStatus::Completed,
        };
        let out = render_text(&inputs, false);
        assert!(out.contains("power_test compare"));
    }

    #[test]
    fn colorize_wraps_in_ansi_codes() {
        assert!(colorize("hi", ColorClass::Good).contains("\x1b[32m"));
        assert!(colorize("hi", ColorClass::Bad).contains("\x1b[31m"));
        assert!(colorize("hi", ColorClass::Neutral).contains("\x1b[90m"));
    }

    #[test]
    fn snapshot_uses_metrics_correctly() {
        // Avoids running real network; build records and check the snapshot.
        let mut agg = MetricsAggregator::new();
        let mut m = crate::client::RequestMetrics::default();
        m.status = 200;
        m.total_duration = Duration::from_millis(100);
        m.ttft = Some(Duration::from_millis(50));
        m.completion_tokens = 10;
        m.itl_samples = vec![Duration::from_millis(10); 5];
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let cfg = cfg("s", "m", "http://x", 1, 1.0);
        let snap = AggregatorSnapshot::from(&agg, &cfg);
        assert!((snap.achieved_rps - 1.0).abs() < 1e-6);
        assert!((snap.success_rate - 100.0).abs() < 1e-6);
        assert!(snap.total_completion_tokens == 10);
        assert!((snap.tps_mean - 100.0).abs() < 1e-6); // 10 tokens / 0.1s
    }
}
