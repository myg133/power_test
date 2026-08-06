//! Self-contained HTML report. Chart.js is loaded from a CDN; everything
//! else (data, layout, styling) is inlined so the file can be opened from
//! a local filesystem with no extra files.

use std::fmt::Write as _;

use serde_json::json;

use crate::config::RunConfig;
use crate::report::charts::{latency_percentiles, rps_timeline, status_breakdown};
use crate::runner::{HistKind, MetricsAggregator};

/// Render the full HTML report as a String. Pure function: same inputs
/// produce the same output.
pub fn render_html(cfg: &RunConfig, agg: &MetricsAggregator, interrupted: bool) -> String {
    render_html_with_compare(cfg, agg, interrupted, &[])
}

/// Same as [`render_html`], but embeds a small "compare with…"
/// dropdown at the top of the report so the reader can jump to a
/// side-by-side view of a previous run. The list is rendered as
/// `<option>` entries that link to
/// `compare-<this>-vs-<other>.html` if such a file exists; the page
/// does a `fetch()`-style check via an inline `<script>` that toggles
/// a "no compare file" hint.
///
/// `compare_links` is a list of `(other_run_id, compare_filename)`
/// pairs. Pass an empty slice to render a single-run report (the
/// current M1/M2 default).
pub fn render_html_with_compare(
    cfg: &RunConfig,
    agg: &MetricsAggregator,
    interrupted: bool,
    compare_links: &[(String, String)],
) -> String {
    let summary_stats = build_summary_stats(cfg, agg, interrupted);
    let lp = latency_percentiles(agg);
    let sb = status_breakdown(agg);
    let rt = rps_timeline(agg);
    let cache_card = build_cache_card(agg);

    let title = format!("power_test report — {}", cfg.run_id);
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

    let data_json = json!({
        "summary": summary_stats,
        "latencyPercentiles": lp,
        "statusBreakdown": sb,
        "rpsTimeline": rt,
        "errors": agg.error_messages(),
    })
    .to_string();

    let errors_rows = build_errors_rows(agg);
    let compare_panel = build_compare_panel(compare_links);

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
    .wrap {{ max-width: 1100px; margin: 0 auto; padding: 24px; }}
    h1 {{ margin: 0 0 8px 0; font-size: 24px; }}
    h2 {{ margin: 32px 0 12px 0; font-size: 18px; border-bottom: 1px solid var(--border); padding-bottom: 6px; }}
    .sub {{ color: var(--muted); margin-bottom: 24px; }}
    .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 12px; margin-bottom: 24px; }}
    .card {{ background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 14px; }}
    .card .label {{ color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; }}
    .card .value {{ font-size: 22px; font-weight: 600; margin-top: 4px; }}
    .card .value.good {{ color: var(--good); }}
    .card .value.bad {{ color: var(--bad); }}
    table {{ width: 100%; border-collapse: collapse; background: var(--card); border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }}
    th, td {{ text-align: left; padding: 8px 12px; border-bottom: 1px solid var(--border); }}
    th {{ color: var(--muted); font-weight: 500; font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; background: rgba(255,255,255,0.02); }}
    tr:last-child td {{ border-bottom: none; }}
    .charts {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 16px; }}
    .chart-card {{ background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 16px; min-height: 320px; }}
    .chart-card h3 {{ margin: 0 0 12px 0; font-size: 14px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }}
    .chart-card canvas {{ max-height: 260px; }}
    .errors {{ background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 0; }}
    .errors .row {{ padding: 10px 14px; border-bottom: 1px solid var(--border); font-family: ui-monospace, SFMono-Regular, monospace; font-size: 13px; }}
    .errors .row:last-child {{ border-bottom: none; }}
    .errors .count {{ color: var(--bad); font-weight: 600; margin-right: 8px; }}
    .compare-panel {{
      display: flex; align-items: center; gap: 10px;
      background: var(--card);
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 10px 14px;
      margin-bottom: 24px;
    }}
    .compare-panel label {{ color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; }}
    .compare-panel select {{
      flex: 1; min-width: 200px;
      background: var(--bg);
      color: var(--fg);
      border: 1px solid var(--border);
      border-radius: 6px;
      padding: 6px 8px;
      font: inherit;
    }}
    .compare-panel a.btn {{
      background: var(--accent); color: #0e1116;
      padding: 6px 12px; border-radius: 6px;
      text-decoration: none; font-weight: 600;
    }}
    .compare-panel a.btn.disabled {{
      background: var(--border); color: var(--muted); pointer-events: none;
    }}
    .compare-panel .hint {{ color: var(--muted); font-size: 12px; }}
    footer {{ margin-top: 48px; padding-top: 16px; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px; text-align: center; }}
  </style>
</head>
<body>
  <div class="wrap">
    <h1>power_test report</h1>
    <div class="sub">run <code>{run_id}</code>{tag_html} · started <code>{started}</code>{interrupted_html}</div>

    {compare_panel}

    <div class="grid">
      <div class="card"><div class="label">Target</div><div class="value" style="font-size:14px;word-break:break-all;">{target}</div></div>
      <div class="card"><div class="label">Model</div><div class="value" style="font-size:18px;">{model}</div></div>
      <div class="card"><div class="label">Duration</div><div class="value">{duration}s</div></div>
      <div class="card"><div class="label">Target RPS</div><div class="value">{target_rps:.2}</div></div>
      <div class="card"><div class="label">Achieved RPS</div><div class="value">{achieved_rps:.2}</div></div>
      <div class="card"><div class="label">Success rate</div><div class="value {success_class}">{success_rate:.1}%</div></div>
      <div class="card"><div class="label">Total requests</div><div class="value">{total_requests}</div></div>
      <div class="card"><div class="label">Total tokens</div><div class="value">{total_tokens}</div></div>
    </div>

    <h2>Configuration</h2>
    <table>
      <tbody>
        <tr><td>Load pattern</td><td><code>{pattern_name}</code>{pattern_detail}</td></tr>
        <tr><td>Dataset</td><td><code>{dataset_name}</code> · strategy <code>{strategy}</code>{dataset_detail}</td></tr>
        <tr><td>Prompt tokens</td><td>{prompt_count} prompts · min <strong>{prompt_min}</strong> · mean <strong>{prompt_mean:.1}</strong> · max <strong>{prompt_max}</strong></td></tr>
        <tr><td>Concurrency</td><td>{concurrency}</td></tr>
        {model_alias_row}
      </tbody>
    </table>

    <h2>Summary statistics</h2>
    <table>
      <thead><tr><th>Metric</th><th>p50</th><th>p90</th><th>p99</th><th>p99.9</th></tr></thead>
      <tbody>
        <tr><td>Latency (ms)</td><td>{lat_p50}</td><td>{lat_p90}</td><td>{lat_p99}</td><td>{lat_p999}</td></tr>
        <tr><td>TTFT (ms)</td><td>{ttft_p50}</td><td>—</td><td>{ttft_p99}</td><td>—</td></tr>
        <tr><td>ITL (ms)</td><td>{itl_p50}</td><td>—</td><td>{itl_p99}</td><td>—</td></tr>
        <tr><td>TPS (tok/s)</td><td>{tps_p50}</td><td>—</td><td>{tps_p99}</td><td>—</td></tr>
      </tbody>
    </table>

    {cache_card}

    <h2>Charts</h2>
    <div class="charts">
      <div class="chart-card"><h3>Latency percentiles (ms)</h3><canvas id="latencyChart"></canvas></div>
      <div class="chart-card"><h3>Status code distribution</h3><canvas id="statusChart"></canvas></div>
      <div class="chart-card"><h3>Per-second RPS</h3><canvas id="rpsChart"></canvas></div>
    </div>

    <h2>Errors</h2>
    <div class="errors">{errors_rows}</div>

    <footer>powered by power_test v{version}</footer>
  </div>

  <script id="metrics-data" type="application/json">{data_json}</script>
  <script>
    const DATA = JSON.parse(document.getElementById('metrics-data').textContent);
    const palette = ['#58a6ff', '#3fb950', '#d29922', '#a371f7', '#f85149', '#8b949e'];
    const chartFont = {{ size: 12 }};

    function fmtMs(us) {{ return (us / 1000).toFixed(2); }}

    new Chart(document.getElementById('latencyChart'), {{
      type: 'bar',
      data: {{
        labels: DATA.latencyPercentiles.labels,
        datasets: [{{
          label: 'Latency (ms)',
          data: DATA.latencyPercentiles.values_ms,
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

    new Chart(document.getElementById('statusChart'), {{
      type: 'doughnut',
      data: {{
        labels: DATA.statusBreakdown.labels,
        datasets: [{{ data: DATA.statusBreakdown.counts, backgroundColor: palette }}]
      }},
      options: {{
        responsive: true,
        maintainAspectRatio: false,
        plugins: {{ legend: {{ position: 'bottom', labels: {{ font: chartFont }} }} }}
      }}
    }});

    new Chart(document.getElementById('rpsChart'), {{
      type: 'line',
      data: {{
        labels: DATA.rpsTimeline.seconds.map(s => s + 's'),
        datasets: [
          {{ label: 'Started', data: DATA.rpsTimeline.started, borderColor: palette[0], backgroundColor: palette[0], tension: 0.1 }},
          {{ label: 'Completed', data: DATA.rpsTimeline.completed, borderColor: palette[1], backgroundColor: palette[1], tension: 0.1 }}
        ]
      }},
      options: {{
        responsive: true,
        maintainAspectRatio: false,
        plugins: {{ legend: {{ position: 'bottom', labels: {{ font: chartFont }} }} }},
        scales: {{ y: {{ beginAtZero: true, ticks: {{ font: chartFont, precision: 0 }} }}, x: {{ ticks: {{ font: chartFont }} }} }}
      }}
    }});

    // Compare-with panel: when a file is selected, "Compare" links to
    // compare-<this>-vs-<other>.html. If no pre-rendered compare file
    // exists, the link is greyed out and a hint suggests running
    // `power_test compare --html` from the CLI.
    (function() {{
      const sel = document.getElementById('compare-select');
      const link = document.getElementById('compare-link');
      if (!sel || !link) return;
      const opts = sel.querySelectorAll('option');
      if (opts.length <= 1) return; // only the placeholder
      function refresh() {{
        const opt = sel.options[sel.selectedIndex];
        if (!opt || !opt.dataset.filename) {{
          link.classList.add('disabled');
          link.setAttribute('aria-disabled', 'true');
          link.textContent = 'Compare';
          link.removeAttribute('href');
          return;
        }}
        link.href = opt.dataset.filename;
        link.classList.remove('disabled');
        link.textContent = 'Compare →';
      }}
      sel.addEventListener('change', refresh);
      refresh();
    }})();
  </script>
</body>
</html>
"##,
        title = title,
        run_id = html_escape(&cfg.run_id),
        tag_html = cfg
            .tag
            .as_deref()
            .map(|t| format!(" · tag <code>{}</code>", html_escape(t)))
            .unwrap_or_default(),
        started = cfg.started_at.to_rfc3339(),
        interrupted_html = if interrupted { " · <span style=\"color:var(--bad)\">interrupted</span>" } else { "" },
        compare_panel = compare_panel,
        target = html_escape(&cfg.target),
        model = html_escape(&cfg.model),
        duration = cfg.duration_secs,
        target_rps = cfg.target_rps,
        achieved_rps = achieved_rps,
        success_rate = success_rate,
        success_class = if success_rate >= 99.0 { "good" } else if success_rate < 90.0 { "bad" } else { "" },
        total_requests = agg.total_requests(),
        total_tokens = agg.total_completion_tokens(),
        lat_p50 = fmt_ms_opt(agg.percentile(HistKind::Latency, 50.0)),
        lat_p90 = fmt_ms_opt(agg.percentile(HistKind::Latency, 90.0)),
        lat_p99 = fmt_ms_opt(agg.percentile(HistKind::Latency, 99.0)),
        lat_p999 = fmt_ms_opt(agg.percentile(HistKind::Latency, 99.9)),
        ttft_p50 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 50.0)),
        ttft_p99 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 99.0)),
        itl_p50 = fmt_ms_opt_f64(agg.mean(HistKind::Itl)),
        itl_p99 = fmt_ms_opt(agg.percentile(HistKind::Itl, 99.0)),
        tps_p50 = fmt_opt(agg.tps_mean(), 2),
        tps_p99 = fmt_opt(agg.tps_percentile(99.0), 2),
        errors_rows = errors_rows,
        data_json = html_escape(&data_json),
        cache_card = cache_card,
        version = env!("CARGO_PKG_VERSION"),
        pattern_name = cfg.pattern.name(),
        pattern_detail = html_escape(&format_pattern_detail(&cfg.pattern)),
        dataset_name = cfg.dataset.name(),
        strategy = cfg.strategy.as_str(),
        dataset_detail = html_escape(&format_dataset_detail(cfg)),
        prompt_count = cfg.prompt_distribution.count,
        prompt_min = cfg.prompt_distribution.min,
        prompt_mean = cfg.prompt_distribution.mean,
        prompt_max = cfg.prompt_distribution.max,
        concurrency = cfg.concurrency,
        model_alias_row = build_model_alias_row(cfg),
    )
}

/// M6g: when a `--model-alias` was set, render an extra row
/// in the Configuration table showing it. The alias is what
/// groups this run with its siblings in the history
/// directory; the actual model name is the row above.
fn build_model_alias_row(cfg: &RunConfig) -> String {
    match &cfg.model_alias {
        Some(alias) if !alias.is_empty() => format!(
            r#"<tr><td>Model alias</td><td><code>{}</code> <span style="color:var(--muted);font-size:12px;">(groups history + compare)</span></td></tr>"#,
            html_escape(alias)
        ),
        _ => String::new(),
    }
}

fn fmt_ms_opt(us: Option<u64>) -> String {
    match us {
        Some(us) => format!("{:.2}", us as f64 / 1000.0),
        None => "—".into(),
    }
}

fn fmt_ms_opt_f64(us: Option<f64>) -> String {
    match us {
        Some(us) => format!("{:.2}", us / 1000.0),
        None => "—".into(),
    }
}

fn fmt_opt(v: f64, decimals: usize) -> String {
    if v > 0.0 {
        format!("{v:.*}", decimals)
    } else {
        "—".into()
    }
}

fn build_summary_stats(cfg: &RunConfig, agg: &MetricsAggregator, interrupted: bool) -> serde_json::Value {
    json!({
        "run_id": cfg.run_id,
        "target": cfg.target,
        "model": cfg.model,
        "duration_secs": cfg.duration_secs,
        "target_rps": cfg.target_rps,
        "concurrency": cfg.concurrency,
        "stream": cfg.stream,
        "interrupted": interrupted,
        "tag": cfg.tag,
        "scheduled": agg.scheduled(),
        "skipped": agg.skipped(),
        "total_requests": agg.total_requests(),
        "success_count": agg.success_count(),
        "error_count": agg.error_count(),
        "total_completion_tokens": agg.total_completion_tokens(),
        "total_prompt_tokens": agg.total_prompt_tokens(),
        "pattern_name": cfg.pattern.name(),
        "pattern": cfg.pattern,
        "dataset_name": cfg.dataset.name(),
        "dataset": cfg.dataset,
        "strategy": cfg.strategy.as_str(),
        "prompt_distribution": cfg.prompt_distribution,
    })
}

fn build_compare_panel(links: &[(String, String)]) -> String {
    if links.is_empty() {
        return String::new();
    }
    let mut options = String::from(
        r##"<option value="" data-filename="">-- pick a previous run --</option>"##,
    );
    for (other_id, filename) in links {
        let _ = write!(
            options,
            r##"<option value="{id}" data-filename="{file}">{id}</option>"##,
            id = html_escape(other_id),
            file = html_escape(filename),
        );
    }
    let mut out = String::new();
    out.push_str(r##"<div class="compare-panel">"##);
    out.push_str(r##"<label for="compare-select">Compare with...</label>"##);
    out.push_str(r##"<select id="compare-select">"##);
    out.push_str(&options);
    out.push_str("</select>");
    out.push_str(r##"<a id="compare-link" class="btn" href="#">Compare</a>"##);
    out.push_str(
        r##"<span class="hint">pre-render via <code>power_test compare --html</code></span>"##,
    );
    out.push_str("</div>");
    out
}

/// M6e: render a single "Cache" card with the global hit rate
/// plus a turn-1 vs turn-2+ bar pair. Returns an empty string
/// when the run saw no cache data so the section disappears
/// for single-turn / non-caching runs.
fn build_cache_card(agg: &MetricsAggregator) -> String {
    let c = agg.cache_stats();
    if c.cache_creation_total == 0 && c.cache_hit_total == 0 {
        return String::new();
    }
    let overall_pct = format!("{:.1}%", c.rate_overall);
    let creation = c.cache_creation_total;
    let hit = c.cache_hit_total;
    // The bar widths are normalized to the maximum rate across the
    // three columns (overall / turn 1 / turn 2+) so the bars
    // visually compare. When the rate is 0 the bar is invisible
    // (width = 0); the label still shows.
    let max_rate = c
        .rate_overall
        .max(c.rate_turn1)
        .max(c.rate_turn2plus)
        .max(1.0);
    let bar_w = |rate: f64| -> u32 { ((rate / max_rate) * 100.0).round() as u32 };
    let bar_overall = bar_w(c.rate_overall);
    let bar_turn1 = bar_w(c.rate_turn1);
    let bar_turn2 = bar_w(c.rate_turn2plus);
    format!(
        r##"<h2>Prompt cache</h2>
<div class="card" style="margin-bottom:24px;">
  <div class="label" style="color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:0.5px;">Cache hit rate</div>
  <div style="font-size:32px;font-weight:600;margin:4px 0 16px 0;">{overall_pct}</div>
  <div style="font-size:13px;color:var(--muted);margin-bottom:8px;">{hit} of {creation_plus_hit} prompt tokens served from prefix cache · {creation} tokens written to cache</div>
  <table style="width:auto;background:transparent;border:none;">
    <tbody>
      <tr><td style="border:none;padding:4px 12px 4px 0;color:var(--muted);width:120px;">Overall</td>
          <td style="border:none;padding:4px 0;width:300px;background:rgba(255,255,255,0.04);border-radius:3px;">
            <div style="height:14px;width:{bar_overall}%;background:var(--accent);border-radius:3px;min-width:2px;"></div>
          </td>
          <td style="border:none;padding:4px 0 4px 12px;font-family:ui-monospace,SFMono-Regular,monospace;">{overall_pct_text}</td></tr>
      <tr><td style="border:none;padding:4px 12px 4px 0;color:var(--muted);">Turn 1</td>
          <td style="border:none;padding:4px 0;background:rgba(255,255,255,0.04);border-radius:3px;">
            <div style="height:14px;width:{bar_turn1}%;background:var(--good);border-radius:3px;min-width:2px;"></div>
          </td>
          <td style="border:none;padding:4px 0 4px 12px;font-family:ui-monospace,SFMono-Regular,monospace;">{turn1_pct_text}</td></tr>
      <tr><td style="border:none;padding:4px 12px 4px 0;color:var(--muted);">Turn 2+</td>
          <td style="border:none;padding:4px 0;background:rgba(255,255,255,0.04);border-radius:3px;">
            <div style="height:14px;width:{bar_turn2}%;background:var(--accent);border-radius:3px;min-width:2px;"></div>
          </td>
          <td style="border:none;padding:4px 0 4px 12px;font-family:ui-monospace,SFMono-Regular,monospace;">{turn2_pct_text}</td></tr>
    </tbody>
  </table>
  <div style="font-size:11px;color:var(--muted);margin-top:8px;">Turn 1 is the seed (often a miss); turn 2+ shows the steady-state hit rate for multi-turn sessions.</div>
</div>"##,
        overall_pct = overall_pct,
        hit = hit,
        creation = creation,
        creation_plus_hit = creation + hit,
        bar_overall = bar_overall,
        bar_turn1 = bar_turn1,
        bar_turn2 = bar_turn2,
        overall_pct_text = format!("{:.1}%", c.rate_overall),
        turn1_pct_text = format!("{:.1}%", c.rate_turn1),
        turn2_pct_text = format!("{:.1}%", c.rate_turn2plus),
    )
}

fn build_errors_rows(agg: &MetricsAggregator) -> String {
    if agg.error_messages().is_empty() {
        return r#"<div class="row" style="color:var(--muted)">No errors. 🎉</div>"#.to_string();
    }
    let mut rows = String::new();
    let mut errs: Vec<_> = agg.error_messages().iter().collect();
    errs.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
    for (msg, count) in errs {
        rows.push_str(&format!(
            r#"<div class="row"><span class="count">[{}]</span>{}</div>"#,
            count,
            html_escape(msg)
        ));
    }
    rows
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// A short, human-friendly description of a load pattern for the report.
/// Always starts with a non-breaking space so it visually attaches to the
/// pattern name in the row.
fn format_pattern_detail(p: &crate::config::LoadPattern) -> String {
    match p {
        crate::config::LoadPattern::Constant { rps } => format!(" · {rps:.2} rps"),
        crate::config::LoadPattern::Ramp { start, end, duration_secs } => {
            format!(" · {start:.2} → {end:.2} rps over {duration_secs:.0}s")
        }
        crate::config::LoadPattern::Spike { baseline, spikes } => {
            let mut s = format!(" · baseline {baseline:.2} rps, spikes:");
            for sp in spikes {
                let _ = std::fmt::Write::write_fmt(
                    &mut s,
                    format_args!(" t={:.0}s@{:.0}rps/{}s", sp.at_secs, sp.rps, sp.duration_secs),
                );
            }
            s
        }
        crate::config::LoadPattern::Soak {
            rps,
            checkpoint_secs,
        } => format!(" · {rps:.2} rps · checkpoint every {checkpoint_secs}s"),
    }
}

/// A short, human-friendly description of a dataset for the report.
fn format_dataset_detail(cfg: &crate::config::RunConfig) -> String {
    use crate::config::DatasetSpec;
    match &cfg.dataset {
        DatasetSpec::Literal { text } => {
            let preview: String = text.chars().take(40).collect();
            let suffix = if text.chars().count() > 40 { "…" } else { "" };
            format!(" · \"{preview}{suffix}\"")
        }
        DatasetSpec::TokenBudget { target_tokens } => {
            format!(" · ~{target_tokens} tokens")
        }
        DatasetSpec::Builtin => " · hardcoded pool (12 prompts)".into(),
        DatasetSpec::ShareGpt { path } => format!(" · {}", path.display()),
        DatasetSpec::Custom { path } => format!(" · {}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
        RunConfig,
    };
    use std::time::Duration;

    fn cfg() -> RunConfig {
        RunConfig {
            run_id: "test-run".into(),
            target: "https://api.example.com/v1/chat/completions".into(),
            api: ApiKind::Openai,
            model: "gpt-3.5-turbo".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 2.0 },
            max_tokens: 16,
            stream: true,
            target_rps: 2.0,
            duration_secs: 5,
            concurrency: 8,
            tag: Some("smoke".into()),
            api_key: None,
            started_at: chrono::Utc::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
        }
    }

    #[test]
    fn renders_basic_html() {
        let agg = MetricsAggregator::new();
        let html = render_html(&cfg(), &agg, false);
        assert!(html.contains("power_test report"));
        assert!(html.contains("test-run"));
        assert!(html.contains("smoke"));
        assert!(html.contains("cdn.jsdelivr.net"));
        assert!(html.contains("metrics-data"));
    }

    #[test]
    fn renders_html_with_data() {
        let mut agg = MetricsAggregator::new();
        // Several requests with the same latency so p50 is deterministic.
        for _ in 0..5 {
            let mut m = crate::client::RequestMetrics::default();
            m.status = 200;
            m.total_duration = Duration::from_millis(42);
            m.ttft = Some(Duration::from_millis(20));
            m.completion_tokens = 7;
            agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        }
        let mut err = crate::client::RequestMetrics::default();
        err.status = 500;
        err.total_duration = Duration::from_millis(100);
        err.error = Some("boom".into());
        agg.record_completed(&err, 0, &crate::runner::metrics::CompletionContext::none());
        let html = render_html(&cfg(), &agg, false);
        // HdrHistogram rounds to bucket boundaries, so accept anything in 42.0x.
        assert!(
            html.contains("42.0") || html.contains("42.1"),
            "expected '42.0x' in HTML output"
        );
        assert!(html.contains("boom"));
    }

    #[test]
    fn html_escapes_quotes() {
        let s = r#"<script>alert("x")</script>"#;
        let e = html_escape(s);
        assert!(!e.contains('<'));
        assert!(!e.contains('>'));
        assert!(!e.contains('"'));
    }

    #[test]
    fn html_includes_m2_fields_for_ramp_and_builtin() {
        let mut c = cfg();
        c.pattern = LoadPattern::Ramp {
            start: 1.0,
            end: 5.0,
            duration_secs: 10.0,
        };
        c.dataset = DatasetSpec::Builtin;
        c.strategy = RequestStrategy::RoundRobin;
        c.prompt_distribution = PromptDistribution {
            count: 12,
            min: 1,
            max: 200,
            mean: 50.0,
        };
        let html = render_html(&c, &MetricsAggregator::new(), false);
        assert!(html.contains("Load pattern"));
        assert!(html.contains(">ramp<"));
        assert!(html.contains("Dataset"));
        assert!(html.contains(">built-in<"));
        assert!(html.contains("round-robin"));
        assert!(html.contains("Prompt tokens"));
        assert!(html.contains("12 prompts"));
    }

    /// M6e: a single-turn run with no cache data must NOT emit
    /// the cache card — otherwise every report would show a
    /// 0.0% headline and confuse the reader.
    #[test]
    fn html_omits_cache_card_when_no_data() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        assert!(
            !html.contains("Prompt cache"),
            "cache card should be hidden when no cache data was observed"
        );
    }

    /// M6e: with cache data, the report must include the
    /// "Prompt cache" section, the global hit-rate headline,
    /// and the per-turn rows.
    #[test]
    fn html_includes_cache_card_when_data_present() {
        let mut agg = MetricsAggregator::new();
        for turn in 1u32..=2 {
            let m = crate::client::RequestMetrics {
                status: 200,
                prompt_tokens: 100,
                cache_creation_input_tokens: if turn == 1 { 100 } else { 0 },
                cache_hit_input_tokens: if turn == 1 { 0 } else { 100 },
                ..Default::default()
            };
            agg.record_completed(
                &m,
                0,
                &crate::runner::metrics::CompletionContext::turn("s", turn, turn > 1),
            );
        }
        let html = render_html(&cfg(), &agg, false);
        assert!(html.contains("Prompt cache"), "section heading missing");
        assert!(html.contains("Cache hit rate"), "headline missing");
        // Per-turn rows
        assert!(html.contains("Overall"));
        assert!(html.contains("Turn 1"));
        assert!(html.contains("Turn 2+"));
    }
}
