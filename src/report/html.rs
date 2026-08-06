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
  <script>{chart_js}</script>
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
    h3 {{ margin: 16px 0 8px 0; font-size: 13px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; font-weight: 500; }}
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
    <div class="sub">run <code>{run_id}</code> · started <code>{started}</code>{interrupted}</div>

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

    <h3>Run identity</h3>
    <table>
      <tbody>
        <tr><td>Run id</td><td><code>{run_id_html}</code></td></tr>
        <tr><td>Tag</td><td>{tag_html}</td></tr>
        <tr><td>Started</td><td><code>{started}</code></td></tr>
        <tr><td>Target</td><td><code style="word-break:break-all;">{target}</code></td></tr>
        <tr><td>API</td><td><code>{api}</code></td></tr>
        <tr><td>Model</td><td><code>{model}</code> {model_alias_inline}</td></tr>
      </tbody>
    </table>

    <h3>Load</h3>
    <table>
      <tbody>
        <tr><td>Pattern</td><td><code>{pattern_name}</code>{pattern_detail}</td></tr>
        <tr><td>Target RPS</td><td>{target_rps:.2}</td></tr>
        <tr><td>Duration</td><td>{duration}s</td></tr>
        <tr><td>Concurrency</td><td>{concurrency}</td></tr>
        <tr><td>Stream</td><td>{stream}</td></tr>
        <tr><td>Max tokens</td><td>{max_tokens}</td></tr>
      </tbody>
    </table>

    <h3>Dataset</h3>
    <table>
      <tbody>
        <tr><td>Kind</td><td><code>{dataset_name}</code> · strategy <code>{strategy}</code></td></tr>
        <tr><td>Source</td><td><code style="word-break:break-all;">{dataset_source}</code></td></tr>
        <tr><td>Prompt tokens</td><td>{prompt_count} prompts · min <strong>{prompt_min}</strong> · mean <strong>{prompt_mean:.1}</strong> · max <strong>{prompt_max}</strong></td></tr>
      </tbody>
    </table>

    <h2>Summary statistics</h2>
    <table>
      <thead><tr><th>Metric</th><th>p50</th><th>p90</th><th>p99</th><th>p99.9</th></tr></thead>
      <tbody>
        <tr><td>Latency (ms)</td><td>{lat_p50}</td><td>{lat_p90}</td><td>{lat_p99}</td><td>{lat_p999}</td></tr>
        <tr><td>TTFT (ms)</td><td>{ttft_p50}</td><td>{ttft_p90}</td><td>{ttft_p99}</td><td>{ttft_p999}</td></tr>
        <tr><td>ITL (ms)</td><td>{itl_p50}</td><td>{itl_p90}</td><td>{itl_p99}</td><td>{itl_p999}</td></tr>
        <tr><td>TPS (tok/s)</td><td>{tps_p50}</td><td>{tps_p90}</td><td>{tps_p99}</td><td>{tps_p999}</td></tr>
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
    // M6i: defer chart construction until the window's
    // `load` event fires so the grid layout has computed
    // final canvas dimensions. The previous eager
    // `new Chart(canvas, ...)` calls in the body footer
    // ran before layout in some headless renderers
    // (canvas size 0 → chart drew nothing visible).
    // Real browsers were usually fine but the symptom
    // surfaced during the qwen36 multi-turn screenshot
    // validation.
    window.addEventListener('load', function() {{
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
    }});  // close window.addEventListener('load', ...)
  </script>
</body>
</html>
"##,
        title = title,
        // M6i: Configuration is now three subsections
        // (Run identity / Load / Dataset) — the old
        // single-table layout crammed the model name, the
        // alias, the entire dataset path, and the prompt
        // distribution into one un-grouped list. Each
        // subsection is its own <h3> + <table> pair.
        run_id = html_escape(&cfg.run_id),
        run_id_html = html_escape(&cfg.run_id),
        tag_html = match cfg.tag.as_deref() {
            Some(t) if !t.is_empty() => format!("<code>{}</code>", html_escape(t)),
            _ => r#"<span style="color:var(--muted);font-style:italic;">(no tag)</span>"#.into(),
        },
        started = cfg.started_at.to_rfc3339(),
        interrupted = if interrupted { r#" · <span style="color:var(--bad)">interrupted</span>"# } else { "" },
        target = html_escape(&cfg.target),
        model = html_escape(&cfg.model),
        api = cfg.api.as_str(),
        // M6i: alias is now a small badge next to the
        // model name in the Run identity table, not a
        // separate row. Avoids two rows for the same
        // logical concept.
        model_alias_inline = match cfg.model_alias.as_deref() {
            Some(a) if !a.is_empty() => format!(
                r#" <span style="color:var(--muted);font-size:12px;">· alias <code>{}</code> (groups history + compare)</span>"#,
                html_escape(a)
            ),
            _ => String::new(),
        },
        pattern_name = cfg.pattern.name(),
        pattern_detail = html_escape(&format_pattern_detail(&cfg.pattern)),
        target_rps = cfg.target_rps,
        duration = cfg.duration_secs,
        concurrency = cfg.concurrency,
        stream = if cfg.stream { "true" } else { "false" },
        max_tokens = cfg.max_tokens,
        dataset_name = cfg.dataset.name(),
        strategy = cfg.strategy.as_str(),
        dataset_source = html_escape(&format_dataset_source(cfg)),
        prompt_count = cfg.prompt_distribution.count,
        prompt_min = cfg.prompt_distribution.min,
        prompt_mean = cfg.prompt_distribution.mean,
        prompt_max = cfg.prompt_distribution.max,
        achieved_rps = achieved_rps,
        success_rate = success_rate,
        success_class = if success_rate >= 99.0 { "good" } else if success_rate < 90.0 { "bad" } else { "" },
        total_requests = agg.total_requests(),
        total_tokens = agg.total_completion_tokens(),
        lat_p50 = fmt_ms_opt(agg.percentile(HistKind::Latency, 50.0)),
        lat_p90 = fmt_ms_opt(agg.percentile(HistKind::Latency, 90.0)),
        lat_p99 = fmt_ms_opt(agg.percentile(HistKind::Latency, 99.0)),
        lat_p999 = fmt_ms_opt(agg.percentile(HistKind::Latency, 99.9)),
        // M6h: TTFT / ITL / TPS now show all 4 percentiles
        // (p50 / p90 / p99 / p99.9) to match the Latency row
        // and the 4-column table header. Previously these
        // rows only had p50 + p99, with the p90 / p99.9
        // cells hard-coded to "—" — which made readers
        // wonder whether the data was missing.
        ttft_p50 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 50.0)),
        ttft_p90 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 90.0)),
        ttft_p99 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 99.0)),
        ttft_p999 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 99.9)),
        // M6h: itl_p50 used to be `mean` (an arithmetic
        // average), but the column header says "p50" so
        // the value is misleading. Switched to the actual
        // p50 percentile from the ITL histogram.
        itl_p50 = fmt_ms_opt(agg.percentile(HistKind::Itl, 50.0)),
        itl_p90 = fmt_ms_opt(agg.percentile(HistKind::Itl, 90.0)),
        itl_p99 = fmt_ms_opt(agg.percentile(HistKind::Itl, 99.0)),
        itl_p999 = fmt_ms_opt(agg.percentile(HistKind::Itl, 99.9)),
        tps_p50 = fmt_opt(agg.tps_percentile(50.0), 2),
        tps_p90 = fmt_opt(agg.tps_percentile(90.0), 2),
        tps_p99 = fmt_opt(agg.tps_percentile(99.0), 2),
        tps_p999 = fmt_opt(agg.tps_percentile(99.9), 2),
        errors_rows = errors_rows,
        // M6i fix: the JSON payload lives inside
        // `<script type="application/json">`, which the browser
        // does not parse as HTML or JS. Running it through
        // `html_escape` corrupts the JSON — `"` becomes
        // `&quot;`, `<` becomes `&lt;`, etc. — and the chart
        // script's `JSON.parse(...)` then fails at position 1
        // (the first `{` is fine, but the first key's opening
        // `"` is now `&quot;` which JSON.parse chokes on).
        // The only HTML-special substring we still need to
        // guard against is `</script>` itself, which would
        // prematurely close the data script tag. Escaping
        // just that substring keeps the JSON valid while
        // protecting the script element.
        data_json = data_json.replace("</script>", "<\\/script>"),
        cache_card = cache_card,
        version = env!("CARGO_PKG_VERSION"),
        // M6i: chart.js is inlined so the report is fully
        // self-contained — works offline, no jsdelivr CDN
        // dependency. The 200KB UMD bundle is `include_str!`'d
        // at compile time, so the runtime cost is just the
        // HTML size. Replace the file in `assets/` to upgrade
        // chart.js.
        chart_js = include_str!("../../assets/chart.umd.min.js"),
    )
}

/// M6i: the dataset's source string for the Configuration
/// table. For `literal` / `token-budget` / `built-in` we
/// show a short description; for `sharegpt` / `custom` we
/// show the file path so the user can find it. The old
/// `format_dataset_detail` had a leading " · " and
/// quoted a 40-char preview, which produced noisy
/// output for path-based datasets.
fn format_dataset_source(cfg: &RunConfig) -> String {
    use crate::config::DatasetSpec;
    match &cfg.dataset {
        DatasetSpec::Literal { text } => {
            let preview: String = text.chars().take(80).collect();
            let suffix = if text.chars().count() > 80 { "..." } else { "" };
            format!("literal prompt: \"{preview}{suffix}\"")
        }
        DatasetSpec::TokenBudget { target_tokens } => {
            format!("token-budget: ~{target_tokens} tokens")
        }
        DatasetSpec::Builtin => "built-in: hardcoded 12-prompt pool (mixed English + Chinese)".into(),
        DatasetSpec::ShareGpt { path } => format!("sharegpt file: {}", path.display()),
        DatasetSpec::Custom { path } => format!("custom file: {}", path.display()),
    }
}

fn fmt_ms_opt(us: Option<u64>) -> String {
    match us {
        Some(us) => format!("{:.2}", us as f64 / 1000.0),
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
        // M6i: chart.js is inlined (assets/chart.umd.min.js
        // via include_str!), no CDN. Verify the bundle
        // landed in the HTML body — the header banner is
        // the most reliable substring since it appears
        // in any chart.js version we ship.
        assert!(
            html.contains("Chart.js v4.4.0"),
            "expected chart.js UMD bundle to be inlined in the report"
        );
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

    /// M6h: every metric in the Summary statistics table
    /// must have all 4 percentiles filled, not "—". The
    /// previous template hard-coded "—" for TTFT/ITL/TPS
    /// p90 / p99.9 cells, even when the histograms had
    /// enough samples to compute those percentiles. This
    /// test seeds each histogram with 50 values (enough for
    /// all 4 percentiles) and asserts no "—" appears in the
    /// row for any metric.
    #[test]
    fn summary_table_fills_all_four_percentiles_per_metric() {
        let mut agg = MetricsAggregator::new();
        for i in 1..=50u32 {
            let mut m = crate::client::RequestMetrics::default();
            m.status = 200;
            m.total_duration = Duration::from_millis(100 + i as u64);
            m.ttft = Some(Duration::from_millis(40 + i as u64 / 2));
            m.completion_tokens = 5;
            // Add a few ITL samples per record so the ITL
            // histogram has multiple values.
            m.itl_samples = vec![
                Duration::from_millis(20),
                Duration::from_millis(30),
                Duration::from_millis(40),
            ];
            agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        }
        let html = render_html(&cfg(), &agg, false);
        // Pull out the Summary statistics section so the
        // assertions don't accidentally match "—" elsewhere
        // in the page (status code, errors, etc.).
        let summary_start = html.find("Summary statistics").expect("summary section");
        let summary_end = html[summary_start..]
            .find("</table>")
            .map(|i| summary_start + i + "</table>".len())
            .unwrap_or(html.len());
        let summary = &html[summary_start..summary_end];

        // The 4 metric rows must each have 4 numeric cells.
        for metric in ["Latency (ms)", "TTFT (ms)", "ITL (ms)", "TPS (tok/s)"] {
            let row_start = summary.find(metric).expect(metric);
            let row_end = summary[row_start..]
                .find("</tr>")
                .map(|i| row_start + i + "</tr>".len())
                .unwrap_or(summary.len());
            let row = &summary[row_start..row_end];
            assert!(
                !row.contains("—"),
                "M6h: {metric} row still has '—' in the Summary table: {row}"
            );
            // Every row should have 4 <td>...</td> data cells.
            let td_count = row.matches("<td>").count();
            assert_eq!(
                td_count, 4,
                "M6h: {metric} row should have 4 cells (label + 3 percentiles + 1), got {td_count}: {row}"
            );
        }
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
        // M6i: Configuration is now three subsections
        // (Run identity / Load / Dataset). Each gets
        // its own h3 + table — assert the subsection
        // headers and the row content rather than the
        // old single-table "Load pattern" string.
        assert!(html.contains("<h3>Run identity</h3>"), "missing Run identity subsection");
        assert!(html.contains("<h3>Load</h3>"), "missing Load subsection");
        assert!(html.contains("<h3>Dataset</h3>"), "missing Dataset subsection");
        assert!(html.contains(">ramp<"));
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

    /// M6i fix: the JSON data block inside
    /// `<script type="application/json" id="metrics-data">`
    /// must be valid JSON. The old code ran the JSON through
    /// `html_escape`, which turned every `"` into `&quot;`
    /// and made the chart script's `JSON.parse(...)` fail at
    /// position 1 ("Expected property name or '}'"). Verify
    /// the block round-trips through `serde_json::from_str`.
    #[test]
    fn metrics_data_block_is_valid_json() {
        let mut agg = MetricsAggregator::new();
        let mut m = crate::client::RequestMetrics::default();
        m.status = 200;
        m.total_duration = Duration::from_millis(50);
        m.completion_tokens = 3;
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let html = render_html(&cfg(), &agg, false);

        // Pull the JSON payload out of the data script tag.
        let marker = r##"<script id="metrics-data" type="application/json">"##;
        let start = html
            .find(marker)
            .expect("metrics-data script tag must exist")
            + marker.len();
        let end = html[start..]
            .find("</script>")
            .map(|i| start + i)
            .expect("metrics-data script must close");
        let payload = &html[start..end];
        let parsed: serde_json::Value =
            serde_json::from_str(payload).expect("metrics-data JSON must parse cleanly");
        // Spot-check a few fields so we don't accidentally pass on
        // an empty object — the failure mode the user reported was
        // `JSON.parse` failing on the very first key.
        assert!(parsed.get("summary").is_some(), "missing 'summary' key");
        assert!(
            parsed.get("latencyPercentiles").is_some(),
            "missing 'latencyPercentiles' key"
        );
        // The raw payload must still contain literal `"` characters
        // (the JSON key delimiters). The old bug escaped them to
        // `&quot;` and broke parsing.
        assert!(
            payload.contains('"'),
            "metrics-data JSON must contain literal quote characters; \
             got: {payload}"
        );
    }

    /// M6i fix: if a server error string contains `</script>`,
    /// the data script tag would close prematurely and break
    /// the rest of the page. Verify we escape that one
    /// substring while leaving the rest of the JSON intact.
    #[test]
    fn metrics_data_block_escapes_closing_script_substring() {
        let mut agg = MetricsAggregator::new();
        // The server returned an error string that happens to
        // contain a `</script>` substring. In a real LLM test
        // this happens when upstream returns a generic HTML
        // 5xx error page.
        let mut m = crate::client::RequestMetrics::default();
        m.status = 500;
        m.error = Some(r#"<html><body>oops</body></html> </script> ALERT"#.into());
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());

        let html = render_html(&cfg(), &agg, false);
        let marker = r##"<script id="metrics-data" type="application/json">"##;
        let start = html
            .find(marker)
            .expect("metrics-data script tag must exist")
            + marker.len();
        // The substring `</script>` must NOT appear inside the
        // data block (it would close the data script early).
        // Find the matching closing tag and ensure it comes
        // after our data content.
        let first_close = html[start..]
            .find("</script>")
            .expect("data script must have a closing tag");
        let payload = &html[start..start + first_close];
        assert!(
            !payload.contains("</script>"),
            "metrics-data block must not contain a raw </script> substring; \
             got: {payload}"
        );
        // The payload must still be valid JSON.
        let parsed: serde_json::Value = serde_json::from_str(payload)
            .expect("metrics-data JSON must parse even with </script> in error text");
        assert!(parsed.get("errors").is_some(), "errors key should be present");
    }
}
