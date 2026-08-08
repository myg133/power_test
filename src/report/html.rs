//! Self-contained HTML report. Chart.js is loaded from a CDN; everything
//! else (data, layout, styling) is inlined so the file can be opened from
//! a local filesystem with no extra files.
//!
//! M7: labels go through the bilingual dictionary in [crate::report::i18n];
//! the page is rendered in Chinese by default and a small inline script
//! flips every [data-i18n] element to English when the user clicks the
//! toggle. The cache section is now always emitted (with a zero state
//! when the run saw no cache data) so the layout is consistent across
//! single-turn and multi-turn runs.

use std::fmt::Write as _;

use serde_json::json;

use crate::config::RunConfig;
use crate::report::charts::{latency_percentiles, rps_timeline, status_breakdown};
use crate::report::i18n::{dict_to_json, t, Locale};
use crate::runner::{HistKind, MetricsAggregator};

/// Render the full HTML report as a String. Pure function: same inputs
/// produce the same output.
pub fn render_html(cfg: &RunConfig, agg: &MetricsAggregator, interrupted: bool) -> String {
    render_html_with_compare(cfg, agg, interrupted, &[])
}

/// Same as render_html, but embeds a small compare-with… dropdown
/// at the top of the report so the reader can jump to a
/// side-by-side view of a previous run. The list is rendered as
/// <option> entries that link to
/// compare-<this>-vs-<other>.html if such a file exists; the page
/// does a fetch()-style check via an inline <script> that toggles
/// a "no compare file" hint.
///
/// compare_links is a list of (other_run_id, compare_filename)
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

    // M8: throughput + TPOT + multi-turn + speculative section.
    let avg_input_tok = if agg.total_requests() > 0 {
        agg.total_prompt_tokens() as f64 / agg.total_requests() as f64
    } else { 0.0 };
    let avg_output_tok = if agg.total_requests() > 0 {
        agg.total_completion_tokens() as f64 / agg.total_requests() as f64
    } else { 0.0 };
    // M8: throughput + TPOT + multi-turn + speculative i18n labels.
    let metric_t8 = t(Locale::Zh, "metric.t8");
    let metric_tpot = t(Locale::Zh, "metric.tpot");
    let _metric_throughput_output = t(Locale::Zh, "metric.throughput_output");
    let _metric_throughput_total = t(Locale::Zh, "metric.throughput_total");
    let _metric_avg_input = t(Locale::Zh, "metric.avg_input");
    let _metric_avg_output = t(Locale::Zh, "metric.avg_output");
    let _metric_avg_turns = t(Locale::Zh, "metric.avg_turns");
    let metric_spec_decoded = t(Locale::Zh, "metric.spec_decoded");
    let metric_spec_accept = t(Locale::Zh, "metric.spec_accept");

    let (spec_decoded, spec_accept) = agg.speculative_stats();
    let m8_card = format!(
        r##"<h2 data-i18n="metric.t8">{t8}</h2>
<table>
  <tbody>
    <tr><td data-i18n="metric.tpot">{tpot}</td><td>{tpot_ms:.2}</td></tr>
    <tr><td data-i18n="metric.throughput_output">{throughput_output}</td><td>{throughput_output:.2}</td></tr>
    <tr><td data-i18n="metric.throughput_total">{throughput_total}</td><td>{throughput_total:.2}</td></tr>
    <tr><td data-i18n="metric.avg_input">{avg_input_tok}</td><td>{avg_input_tok:.0}</td></tr>
    <tr><td data-i18n="metric.avg_output">{avg_output_tok}</td><td>{avg_output_tok:.0}</td></tr>
    <tr><td data-i18n="metric.avg_turns">{avg_turns}</td><td>{avg_turns:.2}</td></tr>
    {spec_rows}
  </tbody>
</table>"##,
        t8 = metric_t8,
        tpot = metric_tpot,
        tpot_ms = agg.tpot_ms(),
        throughput_output = agg.output_throughput_tps(),
        throughput_total = agg.total_throughput_tps(),
        avg_input_tok = avg_input_tok,
        avg_output_tok = avg_output_tok,
        avg_turns = agg.avg_turns_per_request(),        spec_rows = if spec_decoded > 0.0 || spec_accept > 0.0 {
            format!(
                r##"<tr><td data-i18n="metric.spec_decoded">{spec_decoded}</td><td>{sd:.2}</td></tr>
<tr><td data-i18n="metric.spec_accept">{spec_accept}</td><td>{sa:.1}%</td></tr>"##,
                spec_decoded = metric_spec_decoded,
                spec_accept = metric_spec_accept,
                sd = spec_decoded,
                sa = spec_accept,
            )
        } else { String::new() },
    );

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

    // M7: pre-compute the i18n strings we hand to the template
    // so the giant format! call below stays a list of named
    // arguments. Each tpl value is the rendered Chinese label
    // (or English for chart data); the data-i18n attribute on
    // the parent element is what the JS toggle uses to swap.
    let page_title = t(Locale::Zh, "page.title");
    let run_label = t(Locale::Zh, "page.run_label");
    let started_label = t(Locale::Zh, "page.started_label");
    let interrupted_label = t(Locale::Zh, "page.interrupted_label");

    let card_target = t(Locale::Zh, "card.target");
    let card_model = t(Locale::Zh, "card.model");
    let card_duration = t(Locale::Zh, "card.duration");
    let card_target_rps = t(Locale::Zh, "card.target_rps");
    let card_achieved_rps = t(Locale::Zh, "card.achieved_rps");
    let card_success_rate = t(Locale::Zh, "card.success_rate");
    let card_total_requests = t(Locale::Zh, "card.total_requests");
    let card_total_tokens = t(Locale::Zh, "card.total_tokens");

    let cfg_heading = t(Locale::Zh, "config.heading");
    let cfg_run_identity = t(Locale::Zh, "config.run_identity");
    let cfg_load = t(Locale::Zh, "config.load");
    let cfg_dataset = t(Locale::Zh, "config.dataset");
    let cfg_run_id = t(Locale::Zh, "config.run_id");
    let cfg_tag = t(Locale::Zh, "config.tag");
    let cfg_started = t(Locale::Zh, "config.started");
    let cfg_target = t(Locale::Zh, "config.target");
    let cfg_api = t(Locale::Zh, "config.api");
    let cfg_model = t(Locale::Zh, "config.model");
    let cfg_pattern = t(Locale::Zh, "config.pattern");
    let cfg_concurrency = t(Locale::Zh, "config.concurrency");
    let cfg_stream = t(Locale::Zh, "config.stream");
    let cfg_max_tokens = t(Locale::Zh, "config.max_tokens");
    let cfg_kind = t(Locale::Zh, "config.kind");
    let cfg_source = t(Locale::Zh, "config.source");
    let cfg_strategy = t(Locale::Zh, "config.strategy");
    let cfg_prompt_tokens = t(Locale::Zh, "config.prompt_tokens");

    let metric_heading = t(Locale::Zh, "metric.heading");
    let metric_col_metric = t(Locale::Zh, "metric.col_metric");
    let metric_latency = t(Locale::Zh, "metric.latency");
    let metric_ttft = t(Locale::Zh, "metric.ttft");
    let metric_itl = t(Locale::Zh, "metric.itl");
    let metric_tps = t(Locale::Zh, "metric.tps");

    let charts_heading = t(Locale::Zh, "charts.heading");
    let chart_latency = t(Locale::Zh, "chart.latency");
    let chart_status = t(Locale::Zh, "chart.status");
    let chart_rps = t(Locale::Zh, "chart.rps");

    let errors_heading = t(Locale::Zh, "errors.heading");

    let ui_lang_zh = t(Locale::Zh, "ui.language_toggle_zh");
    let ui_lang_en = t(Locale::Zh, "ui.language_toggle_en");

    let footer_powered_by = t(Locale::Zh, "footer.powered_by").replace("{version}", env!("CARGO_PKG_VERSION"));

    let i18n_dict_zh = dict_to_json(Locale::Zh);
    let i18n_dict_en = dict_to_json(Locale::En);

    format!(
        r##"<!doctype html>
<html lang="zh">
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
                   "PingFang SC", "Microsoft YaHei", "Noto Sans CJK SC",
                   Oxygen, Ubuntu, sans-serif;
      background: var(--bg);
      color: var(--fg);
      line-height: 1.5;
    }}
    .wrap {{ max-width: 1100px; margin: 0 auto; padding: 24px; }}
    h1 {{ margin: 0 0 8px 0; font-size: 24px; display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }}
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
    .lang-toggle {{ display: inline-flex; gap: 4px; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: 2px; }}
    .lang-toggle .lang-btn {{ background: transparent; color: var(--muted); border: 0; padding: 4px 10px; border-radius: 4px; font: inherit; cursor: pointer; }}
    .lang-toggle .lang-btn.active {{ background: var(--accent); color: #0e1116; font-weight: 600; }}
    footer {{ margin-top: 48px; padding-top: 16px; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px; text-align: center; }}
  </style>
</head>
<body>
  <div class="wrap">
    <h1>
      <span data-i18n="page.title">{page_title}</span>
      <span class="lang-toggle" role="group" aria-label="language">
        <button class="lang-btn active" data-lang="zh" data-i18n="ui.language_toggle_zh">{ui_lang_zh}</button>
        <button class="lang-btn" data-lang="en" data-i18n="ui.language_toggle_en">{ui_lang_en}</button>
      </span>
    </h1>
    <div class="sub"><span data-i18n="page.run_label">{run_label}</span> <code>{run_id}</code> · <span data-i18n="page.started_label">{started_label}</span> <code>{started}</code>{interrupted_html}</div>

    {compare_panel}

    <div class="grid">
      <div class="card"><div class="label" data-i18n="card.target">{card_target}</div><div class="value" style="font-size:14px;word-break:break-all;">{target}</div></div>
      <div class="card"><div class="label" data-i18n="card.model">{card_model}</div><div class="value" style="font-size:18px;">{model}</div></div>
      <div class="card"><div class="label" data-i18n="card.duration">{card_duration}</div><div class="value">{duration}s</div></div>
      <div class="card"><div class="label" data-i18n="card.target_rps">{card_target_rps}</div><div class="value">{target_rps:.2}</div></div>
      <div class="card"><div class="label" data-i18n="card.achieved_rps">{card_achieved_rps}</div><div class="value">{achieved_rps:.2}</div></div>
      <div class="card"><div class="label" data-i18n="card.success_rate">{card_success_rate}</div><div class="value {success_class}">{success_rate:.1}%</div></div>
      <div class="card"><div class="label" data-i18n="card.total_requests">{card_total_requests}</div><div class="value">{total_requests}</div></div>
      <div class="card"><div class="label" data-i18n="card.total_tokens">{card_total_tokens}</div><div class="value">{total_tokens}</div></div>
    </div>

    <h2 data-i18n="config.heading">{cfg_heading}</h2>

    <h3 data-i18n="config.run_identity">{cfg_run_identity}</h3>
    <table>
      <tbody>
        <tr><td data-i18n="config.run_id">{cfg_run_id}</td><td><code>{run_id_html}</code></td></tr>
        <tr><td data-i18n="config.tag">{cfg_tag}</td><td>{tag_html}</td></tr>
        <tr><td data-i18n="config.started">{cfg_started}</td><td><code>{started}</code></td></tr>
        <tr><td data-i18n="config.target">{cfg_target}</td><td><code style="word-break:break-all;">{target}</code></td></tr>
        <tr><td data-i18n="config.api">{cfg_api}</td><td><code>{api}</code></td></tr>
        <tr><td data-i18n="config.model">{cfg_model}</td><td><code>{model}</code> {model_alias_inline}</td></tr>
      </tbody>
    </table>

    <h3 data-i18n="config.load">{cfg_load}</h3>
    <table>
      <tbody>
        <tr><td data-i18n="config.pattern">{cfg_pattern}</td><td><code>{pattern_name}</code>{pattern_detail}</td></tr>
        <tr><td data-i18n="card.target_rps">{card_target_rps}</td><td>{target_rps:.2}</td></tr>
        <tr><td data-i18n="card.duration">{card_duration}</td><td>{duration}s</td></tr>
        <tr><td data-i18n="config.concurrency">{cfg_concurrency}</td><td>{concurrency}</td></tr>
        <tr><td data-i18n="config.stream">{cfg_stream}</td><td>{stream}</td></tr>
        <tr><td data-i18n="config.max_tokens">{cfg_max_tokens}</td><td>{max_tokens}</td></tr>
      </tbody>
    </table>

    <h3 data-i18n="config.dataset">{cfg_dataset}</h3>
    <table>
      <tbody>
        <tr><td data-i18n="config.kind">{cfg_kind}</td><td><code>{dataset_name}</code> · <span data-i18n="config.strategy">{cfg_strategy}</span> <code>{strategy}</code></td></tr>
        <tr><td data-i18n="config.source">{cfg_source}</td><td><code style="word-break:break-all;">{dataset_source}</code></td></tr>
        <tr><td data-i18n="config.prompt_tokens">{cfg_prompt_tokens}</td><td>{prompt_count} prompts · min <strong>{prompt_min}</strong> · mean <strong>{prompt_mean:.1}</strong> · max <strong>{prompt_max}</strong></td></tr>
      </tbody>
    </table>

    <h2 data-i18n="metric.heading">{metric_heading}</h2>
    <table>
      <thead><tr><th data-i18n="metric.col_metric">{metric_col_metric}</th><th>p50</th><th>p90</th><th>p99</th><th>p99.9</th></tr></thead>
      <tbody>
        <tr><td data-i18n="metric.latency">{metric_latency}</td><td>{lat_p50}</td><td>{lat_p90}</td><td>{lat_p99}</td><td>{lat_p999}</td></tr>
        <tr><td data-i18n="metric.ttft">{metric_ttft}</td><td>{ttft_p50}</td><td>{ttft_p90}</td><td>{ttft_p99}</td><td>{ttft_p999}</td></tr>
        <tr><td data-i18n="metric.itl">{metric_itl}</td><td>{itl_p50}</td><td>{itl_p90}</td><td>{itl_p99}</td><td>{itl_p999}</td></tr>
        <tr><td data-i18n="metric.tps">{metric_tps}</td><td>{tps_p50}</td><td>{tps_p90}</td><td>{tps_p99}</td><td>{tps_p999}</td></tr>
      </tbody>
    </table>

    {m8_card}

    {cache_card}

    <h2 data-i18n="charts.heading">{charts_heading}</h2>
    <div class="charts">
      <div class="chart-card"><h3 data-i18n="chart.latency">{chart_latency}</h3><canvas id="latencyChart"></canvas></div>
      <div class="chart-card"><h3 data-i18n="chart.status">{chart_status}</h3><canvas id="statusChart"></canvas></div>
      <div class="chart-card"><h3 data-i18n="chart.rps">{chart_rps}</h3><canvas id="rpsChart"></canvas></div>
    </div>

    <h2 data-i18n="errors.heading">{errors_heading}</h2>
    <div class="errors">{errors_rows}</div>

    <footer data-i18n="footer.powered_by">{footer_powered_by}</footer>
  </div>

  <script id="i18n-dict-en" type="application/json">{i18n_dict_en}</script>
  <script id="i18n-dict-zh" type="application/json">{i18n_dict_zh}</script>
  <script id="metrics-data" type="application/json">{data_json}</script>
  <script>
    (function() {{
      const ZH = JSON.parse(document.getElementById('i18n-dict-zh').textContent);
      const EN = JSON.parse(document.getElementById('i18n-dict-en').textContent);
      const STORAGE_KEY = 'power_test.lang';
      const DEFAULT_LANG = 'zh';
      function applyLang(lang) {{
        const dict = (lang === 'en') ? EN : ZH;
        document.documentElement.lang = lang;
        document.querySelectorAll('[data-i18n]').forEach(function(el) {{
          const key = el.getAttribute('data-i18n');
          if (dict[key] !== undefined) {{
            el.textContent = dict[key];
          }}
        }});
        document.querySelectorAll('.lang-toggle .lang-btn').forEach(function(b) {{
          if (b.dataset.lang === lang) {{
            b.classList.add('active');
          }} else {{
            b.classList.remove('active');
          }}
        }});
        try {{ localStorage.setItem(STORAGE_KEY, lang); }} catch (e) {{}}
      }}
      document.querySelectorAll('.lang-toggle .lang-btn').forEach(function(btn) {{
        btn.addEventListener('click', function() {{
          applyLang(btn.dataset.lang);
        }});
      }});
      let stored = null;
      try {{ stored = localStorage.getItem(STORAGE_KEY); }} catch (e) {{}}
      if (stored === 'en' || stored === 'zh') {{
        applyLang(stored);
      }} else {{
        applyLang(DEFAULT_LANG);
      }}
    }})();
  </script>

  <script>
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

    (function() {{
      const sel = document.getElementById('compare-select');
      const link = document.getElementById('compare-link');
      if (!sel || !link) return;
      const opts = sel.querySelectorAll('option');
      if (opts.length <= 1) return;
      function refresh() {{
        const opt = sel.options[sel.selectedIndex];
        if (!opt || !opt.dataset.filename) {{
          link.classList.add('disabled');
          link.setAttribute('aria-disabled', 'true');
          try {{
            const ZH = JSON.parse(document.getElementById('i18n-dict-zh').textContent);
            const EN = JSON.parse(document.getElementById('i18n-dict-en').textContent);
            const dict = document.documentElement.lang === 'en' ? EN : ZH;
            link.textContent = dict['ui.compare'] || 'Compare';
          }} catch (e) {{
            link.textContent = 'Compare';
          }}
          link.removeAttribute('href');
          return;
        }}
        link.href = opt.dataset.filename;
        link.classList.remove('disabled');
        try {{
          const ZH = JSON.parse(document.getElementById('i18n-dict-zh').textContent);
          const EN = JSON.parse(document.getElementById('i18n-dict-en').textContent);
          const dict = document.documentElement.lang === 'en' ? EN : ZH;
          link.textContent = (dict['ui.compare'] || 'Compare') + ' \u2192';
        }} catch (e) {{
          link.textContent = 'Compare';
        }}
      }}
      sel.addEventListener('change', refresh);
      refresh();
    }})();
    }});
  </script>
</body>
</html>
"##,

        title = title,
        run_id = html_escape(&cfg.run_id),
        run_id_html = html_escape(&cfg.run_id),
        tag_html = match cfg.tag.as_deref() {
            Some(t) if !t.is_empty() => format!("<code>{}</code>", html_escape(t)),
            _ => r#"<span style="color:var(--muted);font-style:italic;">(no tag)</span>"#.into(),
        },
        started = cfg.started_at.to_rfc3339(),
        interrupted_html = if interrupted {
            format!(r#" · <span style="color:var(--bad)" data-i18n="page.interrupted_label">{}</span>"#, interrupted_label)
        } else {
            String::new()
        },
        target = html_escape(&cfg.target),
        model = html_escape(&cfg.model),
        api = cfg.api.as_str(),
        model_alias_inline = match cfg.model_alias.as_deref() {
            Some(a) if !a.is_empty() => format!(
                r#" <span style="color:var(--muted);font-size:12px;">· alias <code>{}</code></span>"#,
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
        ttft_p50 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 50.0)),
        ttft_p90 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 90.0)),
        ttft_p99 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 99.0)),
        ttft_p999 = fmt_ms_opt(agg.percentile(HistKind::Ttft, 99.9)),
        itl_p50 = fmt_ms_opt(agg.percentile(HistKind::Itl, 50.0)),
        itl_p90 = fmt_ms_opt(agg.percentile(HistKind::Itl, 90.0)),
        itl_p99 = fmt_ms_opt(agg.percentile(HistKind::Itl, 99.0)),
        itl_p999 = fmt_ms_opt(agg.percentile(HistKind::Itl, 99.9)),
        tps_p50 = fmt_opt(agg.tps_percentile(50.0), 2),
        tps_p90 = fmt_opt(agg.tps_percentile(90.0), 2),
        tps_p99 = fmt_opt(agg.tps_percentile(99.0), 2),
        tps_p999 = fmt_opt(agg.tps_percentile(99.9), 2),
        errors_rows = errors_rows,
        m8_card = m8_card,
        cache_card = cache_card,
        page_title = page_title,
        run_label = run_label,
        started_label = started_label,
        card_target = card_target,
        card_model = card_model,
        card_duration = card_duration,
        card_target_rps = card_target_rps,
        card_achieved_rps = card_achieved_rps,
        card_success_rate = card_success_rate,
        card_total_requests = card_total_requests,
        card_total_tokens = card_total_tokens,
        cfg_heading = cfg_heading,
        cfg_run_identity = cfg_run_identity,
        cfg_load = cfg_load,
        cfg_dataset = cfg_dataset,
        cfg_run_id = cfg_run_id,
        cfg_tag = cfg_tag,
        cfg_started = cfg_started,
        cfg_target = cfg_target,
        cfg_api = cfg_api,
        cfg_model = cfg_model,
        cfg_pattern = cfg_pattern,
        cfg_concurrency = cfg_concurrency,
        cfg_stream = cfg_stream,
        cfg_max_tokens = cfg_max_tokens,
        cfg_kind = cfg_kind,
        cfg_source = cfg_source,
        cfg_strategy = cfg_strategy,
        cfg_prompt_tokens = cfg_prompt_tokens,
        metric_heading = metric_heading,
        metric_col_metric = metric_col_metric,
        metric_latency = metric_latency,
        metric_ttft = metric_ttft,
        metric_itl = metric_itl,
        metric_tps = metric_tps,
        charts_heading = charts_heading,
        chart_latency = chart_latency,
        chart_status = chart_status,
        chart_rps = chart_rps,
        errors_heading = errors_heading,
        ui_lang_zh = ui_lang_zh,
        ui_lang_en = ui_lang_en,
        footer_powered_by = footer_powered_by,
        data_json = data_json.replace("</script>", "<\\/script>"),
        i18n_dict_zh = i18n_dict_zh.replace("</script>", "<\\/script>"),
        i18n_dict_en = i18n_dict_en.replace("</script>", "<\\/script>"),
        chart_js = include_str!("../../assets/chart.umd.min.js"),
    )
}

/// M6i: the dataset's source string for the Configuration
/// table. For literal / token-budget / built-in we show a short
/// description; for sharegpt / custom we show the file path so
/// the user can find it. The old format_dataset_detail had a
/// leading " · " and quoted a 40-char preview, which produced
/// noisy output for path-based datasets.
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
    let mut options = String::new();
    let _ = write!(
        options,
        r##"<option value="" data-filename="" data-i18n="ui.pick_run">{}</option>"##,
        t(Locale::Zh, "ui.pick_run")
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
    out.push_str(&format!(
        r##"<label for="compare-select" data-i18n="ui.compare_with">{}</label>"##,
        t(Locale::Zh, "ui.compare_with")
    ));
    out.push_str(r##"<select id="compare-select">"##);
    out.push_str(&options);
    out.push_str("</select>");
    out.push_str(&format!(
        r##"<a id="compare-link" class="btn" href="#" data-i18n="ui.compare">{}</a>"##,
        t(Locale::Zh, "ui.compare")
    ));
    out.push_str(&format!(
        r##"<span class="hint" data-i18n="ui.pre_render_hint">{}</span>"##,
        t(Locale::Zh, "ui.pre_render_hint")
    ));
    out.push_str("</div>");
    out
}

/// M7: always render the cache card. The previous version
/// returned an empty string when the run saw no cache data
/// (so the section disappeared entirely for single-turn
/// OpenAI runs, which was confusing). Now:
/// - the section heading + card are always emitted.
/// - when no cache data was observed, the headline reads
///   "0.0% (无缓存命中 / no cache observed)" and the bars
///   are zero-width.
/// - the Turn 2+ row is hidden when no continuation turns
///   were observed (single-turn runs and multi-turn runs
///   that never reached turn 2).
/// - a denominator = 0 hint is shown when the run had zero
///   prompt tokens (e.g. all requests errored).
fn build_cache_card(agg: &MetricsAggregator) -> String {
    let c = agg.cache_stats();
    let no_data = c.cache_creation_total == 0 && c.cache_hit_total == 0;
    let no_continuations = c.cache_creation_turn2plus == 0
        && c.cache_hit_turn2plus == 0
        && c.prompt_turn2plus == 0;
    let denominator_zero = c.prompt_turn1 + c.prompt_turn2plus == 0;

    let overall_pct = format!("{:.1}%", c.rate_overall);
    let turn1_pct = format!("{:.1}%", c.rate_turn1);
    let turn2_pct = format!("{:.1}%", c.rate_turn2plus);
    let hit = c.cache_hit_total;
    let creation = c.cache_creation_total;
    let creation_plus_hit = creation + hit;

    // The bar widths are normalized to the maximum rate across
    // the visible columns so the bars visually compare. When
    // the rate is 0 the bar is invisible (width = 0).
    let max_rate = c
        .rate_overall
        .max(c.rate_turn1)
        .max(c.rate_turn2plus)
        .max(1.0);
    let bar_w = |rate: f64| -> u32 { ((rate / max_rate) * 100.0).round() as u32 };
    let bar_overall = bar_w(c.rate_overall);
    let bar_turn1 = bar_w(c.rate_turn1);
    let bar_turn2 = bar_w(c.rate_turn2plus);

    // Headline wording depends on whether we have any data.
    // The "no data" case pairs the percent with the
    // "no cache observed" label so the reader knows the
    // 0% is a fact, not a render bug.
    let headline = if no_data {
        format!(
            r#"<span data-i18n="cache.hit_rate">{}</span> <span style="color:var(--muted);font-size:14px;font-weight:400;" data-i18n="cache.no_data">({})</span>"#,
            overall_pct,
            t(Locale::Zh, "cache.no_data"),
        )
    } else {
        format!(
            r#"<span data-i18n="cache.hit_rate">{}</span>"#,
            overall_pct,
        )
    };

    // The sub-line shows the prompt token breakdown when
    // there's data, and an empty muted line when there isn't.
    let subline = if no_data {
        format!(
            r#"<div style="font-size:13px;color:var(--muted);margin-bottom:8px;" data-i18n="cache.denominator_zero">{}</div>"#,
            t(Locale::Zh, "cache.denominator_zero"),
        )
    } else {
        format!(
            r#"<div style="font-size:13px;color:var(--muted);margin-bottom:8px;"><span data-i18n="cache.hit">{}</span>: <strong>{hit}</strong> / <strong>{creation_plus_hit}</strong> · <span data-i18n="cache.creation">{}</span>: <strong>{creation}</strong></div>"#,
            t(Locale::Zh, "cache.hit"),
            t(Locale::Zh, "cache.creation"),
        )
    };

    let turn2plus_row = if no_continuations {
        String::new()
    } else {
        format!(
            r##"<tr><td style="border:none;padding:4px 12px 4px 0;color:var(--muted);" data-i18n="cache.turn2plus">{}</td>
                <td style="border:none;padding:4px 0;background:rgba(255,255,255,0.04);border-radius:3px;">
                  <div style="height:14px;width:{bar_turn2}%;background:var(--muted);border-radius:3px;min-width:2px;"></div>
                </td>
                <td style="border:none;padding:4px 0 4px 12px;font-family:ui-monospace,SFMono-Regular,monospace;">{turn2_pct}</td></tr>"##,
            t(Locale::Zh, "cache.turn2plus"),
        )
    };

    let no_continuations_hint = if no_continuations {
        format!(
            r#"<div style="font-size:12px;color:var(--muted);margin-top:8px;" data-i18n="cache.no_continuations">{}</div>"#,
            t(Locale::Zh, "cache.no_continuations"),
        )
    } else {
        String::new()
    };

    // For all-error runs the denominator is 0 but we still
    // saw some records. Surface the "denominator = 0" hint
    // in that case (when no_data is false).
    let subline = if denominator_zero && !no_data {
        format!(
            r#"<div style="font-size:13px;color:var(--muted);margin-bottom:8px;" data-i18n="cache.denominator_zero">{}</div>"#,
            t(Locale::Zh, "cache.denominator_zero"),
        )
    } else {
        subline
    };
    let _ = denominator_zero; // silence unused warning when !no_data

    format!(
        r##"<h2 data-i18n="cache.heading">{heading}</h2>
<div class="card" style="margin-bottom:24px;">
  <div class="label" style="color:var(--muted);font-size:12px;text-transform:uppercase;letter-spacing:0.5px;" data-i18n="cache.hit_rate">{hit_rate_label}</div>
  <div style="font-size:32px;font-weight:600;margin:4px 0 16px 0;">{headline}</div>
  {subline}
  <table style="width:auto;background:transparent;border:none;">
    <tbody>
      <tr><td style="border:none;padding:4px 12px 4px 0;color:var(--muted);width:120px;" data-i18n="cache.overall">{overall_label}</td>
          <td style="border:none;padding:4px 0;width:300px;background:rgba(255,255,255,0.04);border-radius:3px;">
            <div style="height:14px;width:{bar_overall}%;background:var(--accent);border-radius:3px;min-width:2px;"></div>
          </td>
          <td style="border:none;padding:4px 0 4px 12px;font-family:ui-monospace,SFMono-Regular,monospace;">{overall_pct}</td></tr>
      <tr><td style="border:none;padding:4px 12px 4px 0;color:var(--muted);" data-i18n="cache.turn1">{turn1_label}</td>
          <td style="border:none;padding:4px 0;background:rgba(255,255,255,0.04);border-radius:3px;">
            <div style="height:14px;width:{bar_turn1}%;background:var(--good);border-radius:3px;min-width:2px;"></div>
          </td>
          <td style="border:none;padding:4px 0 4px 12px;font-family:ui-monospace,SFMono-Regular,monospace;">{turn1_pct}</td></tr>
      {turn2plus_row}
    </tbody>
  </table>
  {no_continuations_hint}
</div>"##,
        heading = t(Locale::Zh, "cache.heading"),
        hit_rate_label = t(Locale::Zh, "cache.hit_rate"),
        overall_label = t(Locale::Zh, "cache.overall"),
        turn1_label = t(Locale::Zh, "cache.turn1"),
        overall_pct = overall_pct,
        turn1_pct = turn1_pct,
        headline = headline,
        subline = subline,
        bar_overall = bar_overall,
        bar_turn1 = bar_turn1,
        turn2plus_row = turn2plus_row,
        no_continuations_hint = no_continuations_hint,
    )
}

fn build_errors_rows(agg: &MetricsAggregator) -> String {
    if agg.error_messages().is_empty() {
        return format!(
            r#"<div class="row" style="color:var(--muted)" data-i18n="errors.none">{}</div>"#,
            t(Locale::Zh, "errors.none")
        );
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
            started_at: chrono::Local::now(),
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
        assert!(
            html.contains("Chart.js v4.4.0"),
            "expected chart.js UMD bundle to be inlined in the report"
        );
        assert!(html.contains("metrics-data"));
    }

    #[test]
    fn renders_html_with_data() {
        let mut agg = MetricsAggregator::new();
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
            m.itl_samples = vec![
                Duration::from_millis(20),
                Duration::from_millis(30),
                Duration::from_millis(40),
            ];
            agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        }
        let html = render_html(&cfg(), &agg, false);
        // M7 fix: the heading is rendered in zh by default
        // ("摘要统计") but the i18n-dict-en block ALSO
        // contains the string "Summary statistics", and
        // the i18n-dict-zh block contains "摘要统计".
        // Anchor on the data-i18n attribute on the <h2>
        // instead of the visible text, so we never jump
        // past the table into the embedded JSON.
        let summary_start = html
            .find(r#"<h2 data-i18n="metric.heading">"#)
            .expect("summary section heading");
        let summary_end = html[summary_start..]
            .find("</table>")
            .map(|i| summary_start + i + "</table>".len())
            .unwrap_or(html.len());
        let summary = &html[summary_start..summary_end];

        // Each metric is rendered in the default locale (zh), so
        // look up the visible label that matches the row we want
        // by trying every language variant until one matches.
        // M7 fix: the table cells are i18n-tagged with
        // `data-i18n="metric.latency"` etc., and the visible
        // text is the zh label. The previous test scanned for
        // the English label which is not present in the default
        // render.
        let rows: &[(&str, &[&str])] = &[
            ("Latency (ms)",   &["延迟 (毫秒)", "Latency (ms)"]),
            ("TTFT (ms)",      &["首 token (毫秒)", "TTFT (ms)"]),
            ("ITL (ms)",       &["token 间隔 (毫秒)", "ITL (ms)"]),
            ("TPS (tok/s)",    &["TPS (token/秒)", "TPS (tok/s)"]),
        ];
        for (label, candidates) in rows {
            let row_start = candidates
                .iter()
                .filter_map(|c| summary.find(c))
                .next()
                .unwrap_or_else(|| panic!("{label} row not found in summary section"));
            let row_end = summary[row_start..]
                .find("</tr>")
                .map(|i| row_start + i + "</tr>".len())
                .unwrap_or(summary.len());
            let row = &summary[row_start..row_end];
            assert!(
                !row.contains("—"),
                "M6h: {label} row still has '—' in the Summary table: {row}"
            );
            let td_count = row.matches("<td>").count();
            assert_eq!(
                td_count, 4,
                "M6h: {label} row should have 4 cells, got {td_count}: {row}"
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
        // Configuration is now three subsections
        // (Run identity / Load / Dataset) — each gets
        // its own h3 + table. The headings are localized
        // so we accept either language.
        assert!(html.contains("Run identity") || html.contains("基本信息"), "missing Run identity subsection");
        assert!(html.contains("Load") || html.contains("负载"), "missing Load subsection");
        assert!(html.contains("Dataset") || html.contains("数据集"), "missing Dataset subsection");
        assert!(html.contains(">ramp<"));
        assert!(html.contains(">built-in<"));
        assert!(html.contains("round-robin"));
        assert!(html.contains("Prompt tokens") || html.contains("Prompt token"));
        assert!(html.contains("12 prompts"));
    }

    /// M7: a single-turn run with no cache data MUST still
    /// emit the cache section (with a zero state and the
    /// "no cache observed" label). The previous M6e
    /// behavior hid the section entirely, which made
    /// single-turn reports look incomplete next to
    /// multi-turn ones.
    #[test]
    fn cache_card_renders_zero_state_when_no_data() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        assert!(
            html.contains("cache.heading") || html.contains("提示词缓存"),
            "cache section heading must always be present"
        );
        assert!(
            html.contains("cache.no_data") || html.contains("无缓存命中"),
            "zero-state label must appear when no cache data was observed"
        );
    }

    /// M7: with cache data, the report includes the
    /// "Prompt cache" section, the global hit-rate headline,
    /// the per-turn rows, and the continuation hint.
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
        assert!(html.contains("cache.heading") || html.contains("提示词缓存"), "section heading missing");
        assert!(html.contains("cache.overall") || html.contains("总体"));
        assert!(html.contains("cache.turn1") || html.contains("第 1 轮"));
        // Multi-turn → Turn 2+ row IS rendered
        assert!(html.contains("cache.turn2plus") || html.contains("第 2 轮"));
    }

    /// M7: a single-turn run that DID see cache data
    /// shows the Overall and Turn 1 rows but hides the
    /// Turn 2+ row.
    #[test]
    fn cache_card_hides_turn2plus_when_no_continuation_turns() {
        let mut agg = MetricsAggregator::new();
        let m = crate::client::RequestMetrics {
            status: 200,
            prompt_tokens: 100,
            cache_creation_input_tokens: 80,
            cache_hit_input_tokens: 0,
            ..Default::default()
        };
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let html = render_html(&cfg(), &agg, false);
        assert!(html.contains("cache.heading") || html.contains("提示词缓存"));
        // Turn 2+ row hidden (data-i18n attribute would mean it WAS rendered).
        assert!(
            !html.contains(r#"data-i18n="cache.turn2plus""#),
            "Turn 2+ row must be hidden for single-turn runs"
        );
        // The "no continuation turns" hint must be present.
        assert!(
            html.contains("cache.no_continuations") || html.contains("无第 2 轮"),
            "single-turn runs should show the no-continuations hint"
        );
    }

    /// M7: the page default <html lang> is now zh
    /// (was en in M6). The English dict is embedded
    /// as JSON in <script id="i18n-dict-en"> for the
    /// toggle to read at click time.
    #[test]
    fn html_default_lang_is_zh() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        assert!(
            html.contains(r#"<html lang="zh">"#),
            "default lang must be zh; got: {}",
            html.lines().find(|l| l.contains("<html")).unwrap_or("")
        );
    }

    /// M7: the page embeds both dictionaries as
    /// <script type="application/json"> blocks. The
    /// JS toggle reads them at click time.
    #[test]
    fn html_embeds_i18n_dict() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        assert!(
            html.contains(r#"<script id="i18n-dict-en" type="application/json">"#),
            "English dict script must be present"
        );
        assert!(
            html.contains(r#"<script id="i18n-dict-zh" type="application/json">"#),
            "Chinese dict script must be present"
        );
    }

    /// M7: a language toggle button is rendered at the
    /// top of the page so the reader can flip between
    /// Chinese and English.
    #[test]
    fn html_contains_language_toggle_button() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        assert!(html.contains(r#"class="lang-toggle""#), "lang-toggle container must exist");
        assert!(html.contains(r#"data-lang="zh""#), "Chinese toggle button must exist");
        assert!(html.contains(r#"data-lang="en""#), "English toggle button must exist");
    }

    /// M7: the page contains Chinese label substrings
    /// in the rendered DOM (i.e. the default render is
    /// Chinese, not English).
    #[test]
    fn html_contains_chinese_labels() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        for label in &["目标", "模型", "时长", "成功率"] {
            assert!(
                html.contains(label),
                "expected Chinese label {label:?} in rendered HTML"
            );
        }
    }

    /// M6i fix: the JSON data block inside
    /// <script type="application/json" id="metrics-data">
    /// must be valid JSON.
    #[test]
    fn metrics_data_block_is_valid_json() {
        let mut agg = MetricsAggregator::new();
        let mut m = crate::client::RequestMetrics::default();
        m.status = 200;
        m.total_duration = Duration::from_millis(50);
        m.completion_tokens = 3;
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let html = render_html(&cfg(), &agg, false);

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
        assert!(parsed.get("summary").is_some(), "missing 'summary' key");
        assert!(
            parsed.get("latencyPercentiles").is_some(),
            "missing 'latencyPercentiles' key"
        );
        assert!(
            payload.contains('"'),
            "metrics-data JSON must contain literal quote characters; got: {payload}"
        );
    }

    /// M6i fix: if a server error string contains </script>,
    /// the data script tag would close prematurely and break
    /// the rest of the page.
    #[test]
    fn metrics_data_block_escapes_closing_script_substring() {
        let mut agg = MetricsAggregator::new();
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
        let first_close = html[start..]
            .find("</script>")
            .expect("data script must have a closing tag");
        let payload = &html[start..start + first_close];
        assert!(
            !payload.contains("</script>"),
            "metrics-data block must not contain a raw </script> substring; got: {payload}"
        );
        let parsed: serde_json::Value = serde_json::from_str(payload)
            .expect("metrics-data JSON must parse even with </script> in error text");
        assert!(parsed.get("errors").is_some(), "errors key should be present");
    }

    /// M7: the i18n dicts embedded in the page are valid
    /// JSON. Same </script>-escape rule as
    /// metrics_data_block_is_valid_json applies.
    #[test]
    fn i18n_dicts_are_valid_json() {
        let html = render_html(&cfg(), &MetricsAggregator::new(), false);
        for id in &["i18n-dict-en", "i18n-dict-zh"] {
            let marker = format!(r##"<script id="{id}" type="application/json">"##);
            let start = html
                .find(&marker)
                .unwrap_or_else(|| panic!("{id} script tag must exist"))
                + marker.len();
            let end = html[start..]
                .find("</script>")
                .map(|i| start + i)
                .unwrap_or_else(|| panic!("{id} script must close"));
            let payload = &html[start..end];
            let _parsed: serde_json::Value = serde_json::from_str(payload)
                .unwrap_or_else(|e| panic!("{id} JSON must parse: {e}; payload: {payload}"));
        }
    }
}
