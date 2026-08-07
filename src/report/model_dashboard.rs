//! Per-model dashboard: lists every run for a model, lets the
//! reader pick two runs to compare side-by-side. Diff math runs
//! in the browser. Default language is Chinese with an English
//! toggle in the top-right corner.

use std::fmt::Write as _;

use serde::Serialize;

use crate::config::RunConfig;
use crate::report::i18n::{dict_to_json, t, Locale};
use crate::runner::{HistKind, MetricsAggregator};

#[derive(Debug, Clone, Serialize)]
pub struct RunSummary {
    pub run_id: String,
    pub started_at: String,
    pub target: String,
    pub model: String,
    pub model_alias: Option<String>,
    pub target_rps: f64,
    pub duration_secs: u64,
    pub total_requests: u64,
    pub success_rate: f64,
    pub achieved_rps: f64,
    pub status: String,
    pub tag: Option<String>,
    pub latency_ms: LatencyQuartiles,
    pub ttft_ms: LatencyPair,
    pub itl_ms: LatencyPair,
    pub tps: LatencyPair,
    pub total_completion_tokens: u64,
    pub cache: CacheStatsView,
    pub report_filename: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyQuartiles {
    pub p50: f64,
    pub p90: f64,
    pub p99: f64,
    pub p99_9: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct LatencyPair {
    pub first: f64,
    pub second: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CacheStatsView {
    pub rate_overall: f64,
    pub rate_turn1: f64,
    pub rate_turn2plus: f64,
    pub creation_total: u64,
    pub hit_total: u64,
    pub has_continuations: bool,
    pub denominator_total: u64,
}

pub fn run_summary_from_run(
    config: &RunConfig,
    agg: &MetricsAggregator,
    report_filename: impl Into<String>,
) -> RunSummary {
    let c = agg.cache_stats();
    let has_continuations =
        c.cache_creation_turn2plus > 0 || c.cache_hit_turn2plus > 0 || c.prompt_turn2plus > 0;
    let denominator_total = c.prompt_turn1 + c.prompt_turn2plus;
    let to_ms = |us: Option<u64>| us.map(|v| v as f64 / 1000.0).unwrap_or(0.0);
    RunSummary {
        run_id: config.run_id.clone(),
        started_at: config.started_at.to_rfc3339(),
        target: config.target.clone(),
        model: config.model.clone(),
        model_alias: config.model_alias.clone(),
        target_rps: config.target_rps,
        duration_secs: config.duration_secs,
        total_requests: agg.total_requests(),
        success_rate: if agg.total_requests() > 0 {
            100.0 * agg.success_count() as f64 / agg.total_requests() as f64
        } else { 0.0 },
        achieved_rps: if config.duration_secs > 0 {
            agg.total_requests() as f64 / config.duration_secs as f64
        } else { 0.0 },
        status: "completed".to_string(),
        tag: config.tag.clone(),
        latency_ms: LatencyQuartiles {
            p50: to_ms(agg.percentile(HistKind::Latency, 50.0)),
            p90: to_ms(agg.percentile(HistKind::Latency, 90.0)),
            p99: to_ms(agg.percentile(HistKind::Latency, 99.0)),
            p99_9: to_ms(agg.percentile(HistKind::Latency, 99.9)),
        },
        ttft_ms: LatencyPair {
            first: to_ms(agg.percentile(HistKind::Ttft, 50.0)),
            second: to_ms(agg.percentile(HistKind::Ttft, 99.0)),
        },
        itl_ms: LatencyPair {
            first: agg.mean(HistKind::Itl).map(|v| v / 1000.0).unwrap_or(0.0),
            second: to_ms(agg.percentile(HistKind::Itl, 99.0)),
        },
        tps: LatencyPair {
            first: agg.tps_mean(),
            second: agg.tps_percentile(99.0),
        },
        total_completion_tokens: agg.total_completion_tokens(),
        cache: CacheStatsView {
            rate_overall: c.rate_overall,
            rate_turn1: c.rate_turn1,
            rate_turn2plus: c.rate_turn2plus,
            creation_total: c.cache_creation_total,
            hit_total: c.cache_hit_total,
            has_continuations,
            denominator_total,
        },
        report_filename: report_filename.into(),
    }
}

fn build_runs_table(
    runs: &[RunSummary],
    col_run_id: &str,
    col_target: &str,
    col_rps: &str,
    col_duration: &str,
    col_requests: &str,
    col_success: &str,
    col_p50: &str,
    col_p99: &str,
    col_tps: &str,
    col_cache: &str,
) -> String {
    if runs.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str("<table class=\"runs\">");
    out.push_str(
        "<colgroup>\
        <col style=\"width:200px\">\
        <col style=\"width:280px\">\
        <col style=\"width:60px\">\
        <col style=\"width:60px\">\
        <col style=\"width:70px\">\
        <col style=\"width:80px\">\
        <col style=\"width:70px\">\
        <col style=\"width:70px\">\
        <col style=\"width:70px\">\
        <col style=\"width:90px\">\
        </colgroup>",
    );
    let _ = write!(
        out,
        "<thead><tr><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th><th>{}</th></tr></thead>",
        html_escape(col_run_id),
        html_escape(col_target),
        html_escape(col_rps),
        html_escape(col_duration),
        html_escape(col_requests),
        html_escape(col_success),
        html_escape(col_p50),
        html_escape(col_p99),
        html_escape(col_tps),
        html_escape(col_cache),
    );
    out.push_str("<tbody>");
    for r in runs {
        let _ = write!(
            out,
            r#"<tr class="row" data-report="{}">"#,
            html_escape(&r.report_filename)
        );
        let _ = write!(
            out,
            "<td><code>{}</code></td>\
            <td class=\"target\" title=\"{}\"><a class=\"url\" href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a></td>\
            <td class=\"num\">{:.2}</td>\
            <td class=\"num\">{}s</td>\
            <td class=\"num\">{}</td>\
            <td class=\"num\">{:.1}%</td>\
            <td class=\"num\">{:.2}</td>\
            <td class=\"num\">{:.2}</td>\
            <td class=\"num\">{:.2}</td>\
            <td class=\"num\">{:.1}%</td>",
            html_escape(&r.run_id),
            html_escape(&r.target),
            html_escape(&r.target),
            html_escape(&r.target),
            r.target_rps,
            r.duration_secs,
            r.total_requests,
            r.success_rate,
            r.latency_ms.p50,
            r.latency_ms.p99,
            r.tps.first,
            r.cache.rate_overall,
        );
        out.push_str("</tr>");
    }
    out.push_str("</tbody></table>");
    out
}

fn html_escape(s: &str) -> String {
    s.replace("&", "&amp;")
        .replace("<", "&lt;")
        .replace(">", "&gt;")
        .replace("\"", "&quot;")
        .replace("'", "&#39;")
}

pub fn render_dashboard(
    _group_key: &str,
    model: &str,
    alias: Option<&str>,
    runs: &[RunSummary],
) -> String {
    let dash_title = t(Locale::Zh, "dash.title");
    let dash_subtitle = if let Some(a) = alias {
        t(Locale::Zh, "dash.subtitle")
            .replace("{n}", &runs.len().to_string())
            .replace("{alias}", a)
    } else {
        t(Locale::Zh, "dash.subtitle_no_alias")
            .replace("{n}", &runs.len().to_string())
    };
    let dash_compare_pick = t(Locale::Zh, "dash.compare_pick");
    let dash_compare = t(Locale::Zh, "dash.compare");
    let dash_reset = t(Locale::Zh, "dash.reset");
    let dash_runs_heading = t(Locale::Zh, "dash.runs_heading");
    let dash_diff_heading = t(Locale::Zh, "dash.diff_heading");
    let dash_no_runs = t(Locale::Zh, "dash.no_runs");
    let col_run_id = t(Locale::Zh, "dash.col_run_id");
    let col_target = t(Locale::Zh, "dash.col_target");
    let col_rps = t(Locale::Zh, "dash.col_rps");
    let col_duration = t(Locale::Zh, "dash.col_duration");
    let col_requests = t(Locale::Zh, "dash.col_requests");
    let col_success = t(Locale::Zh, "dash.col_success");
    let col_p50 = t(Locale::Zh, "dash.col_p50");
    let col_p99 = t(Locale::Zh, "dash.col_p99");
    let col_tps = t(Locale::Zh, "dash.col_tps");
    let col_cache = t(Locale::Zh, "dash.col_cache");
    let ui_lang_zh = t(Locale::Zh, "ui.language_toggle_zh");
    let ui_lang_en = t(Locale::Zh, "ui.language_toggle_en");
    let footer_powered_by = t(Locale::Zh, "footer.powered_by")
        .replace("{version}", env!("CARGO_PKG_VERSION"));
    let i18n_dict_zh = dict_to_json(Locale::Zh);
    let i18n_dict_en = dict_to_json(Locale::En);
    let chart_js = String::from(include_str!("../../assets/chart.umd.min.js"));
    let runs_table = build_runs_table(
        runs,
        &col_run_id,
        &col_target,
        &col_rps,
        &col_duration,
        &col_requests,
        &col_success,
        &col_p50,
        &col_p99,
        &col_tps,
        &col_cache,
    );
    let no_runs_message = if runs.is_empty() {
        format!(
            r##"<div class="empty-state" data-i18n="dash.no_runs">{}</div>"##,
            dash_no_runs
        )
    } else { String::new() };
    let runs_json = serde_json::to_string(runs).unwrap_or_else(|_| "[]".to_string());
    let runs_json = runs_json.replace("</script>", "<\\/script>");
    let i18n_dict_zh_safe = i18n_dict_zh.replace("</script>", "<\\/script>");
    let i18n_dict_en_safe = i18n_dict_en.replace("</script>", "<\\/script>");
    let title = format!("power_test dashboard — {model}");

    let css: String = r##"  <style>
    :root { --bg:#0e1116; --fg:#e6edf3; --muted:#8b949e; --card:#161b22; --border:#30363d; --accent:#58a6ff; --good:#3fb950; --bad:#f85149; }
    * { box-sizing: border-box; }
    body { margin: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, "PingFang SC", "Microsoft YaHei", "Noto Sans CJK SC", Oxygen, Ubuntu, sans-serif; background: var(--bg); color: var(--fg); line-height: 1.5; }
    .wrap { max-width: 1200px; margin: 0 auto; padding: 24px; }
    h1 { margin: 0 0 8px 0; font-size: 24px; display: flex; align-items: center; justify-content: space-between; gap: 16px; flex-wrap: wrap; }
    h2 { margin: 32px 0 12px 0; font-size: 18px; border-bottom: 1px solid var(--border); padding-bottom: 6px; }
    .sub { color: var(--muted); margin-bottom: 16px; }
    .compare-bar { display: flex; align-items: center; gap: 12px; background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 12px 14px; margin-bottom: 16px; flex-wrap: wrap; }
    .compare-bar label { color: var(--muted); font-size: 12px; text-transform: uppercase; letter-spacing: 0.5px; flex: 0 0 auto; }
    .compare-bar select { background: var(--bg); color: var(--fg); border: 1px solid var(--border); border-radius: 6px; padding: 6px 8px; font: inherit; flex: 1 1 200px; min-width: 0; }
    .compare-bar a.btn { background: var(--accent); color: #0e1116; padding: 6px 14px; border-radius: 6px; text-decoration: none; font-weight: 600; cursor: pointer; border: 0; font: inherit; flex: 0 0 auto; }
    .compare-bar a.btn.disabled { background: var(--border); color: var(--muted); pointer-events: none; }
    .compare-bar a.btn.secondary { background: var(--card); color: var(--fg); border: 1px solid var(--border); }
    table { width: 100%; border-collapse: collapse; background: var(--card); border: 1px solid var(--border); border-radius: 8px; overflow: hidden; }
    table.runs { table-layout: fixed; width: max-content; }
    table.runs col { min-width: 0; }
    th, td { text-align: left; padding: 10px 12px; border-bottom: 1px solid var(--border); font-size: 13px; }
    th { letter-spacing: 0.6px; color: var(--fg); font-weight: 600; font-size: 11px; text-transform: uppercase; letter-spacing: 0.5px; background: rgba(255,255,255,0.04); white-space: nowrap; padding-bottom: 12px; }
    th { text-align: center !important; font-weight: 600; }
    tr:last-child td { border-bottom: none; }
    tr.row { transition: background-color 0.1s ease; cursor: pointer; }
    tr.row:hover { background: rgba(88,166,255,0.10); }
    td { border-right: 1px solid rgba(255,255,255,0.05); }
    td:last-child { border-right: none; }
    td { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    td code, td.target { display: block; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: ui-monospace, SFMono-Regular, monospace; font-size: 12px; }
    td.target a.url { display: block; max-width: 100%; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--accent); text-decoration: none; }
    td.num { font-family: ui-monospace, SFMono-Regular, monospace; text-align: right; color: var(--fg); }
    .empty-state { background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 24px; text-align: center; color: var(--muted); }
    .runs-wrap { overflow-x: auto; }
    .charts { display: grid; grid-template-columns: repeat(auto-fit, minmax(380px, 1fr)); gap: 16px; margin-top: 16px; }
    .chart-card { background: var(--card); border: 1px solid var(--border); border-radius: 8px; padding: 16px; min-height: 280px; }
    .chart-card h3 { margin: 0 0 12px 0; font-size: 14px; color: var(--muted); text-transform: uppercase; letter-spacing: 0.5px; }
    .chart-card canvas { max-height: 240px; }
    .hidden { display: none; }
    .lang-toggle { display: inline-flex; gap: 4px; background: var(--card); border: 1px solid var(--border); border-radius: 6px; padding: 2px; }
    .lang-toggle .lang-btn { background: transparent; color: var(--muted); border: 0; padding: 4px 10px; border-radius: 4px; font: inherit; cursor: pointer; }
    .lang-toggle .lang-btn.active { background: var(--accent); color: #0e1116; font-weight: 600; }
    footer { margin-top: 48px; padding-top: 16px; border-top: 1px solid var(--border); color: var(--muted); font-size: 12px; text-align: center; }
    .diff-table { width: 100%; }
    .diff-table th, .diff-table td { white-space: nowrap; }
  </style>
"##.to_string();

    let mut body: String = String::new();
    body.push_str("<!doctype html>\n<html lang=\"zh\">\n<head>\n");
    body.push_str("  <meta charset=\"utf-8\">\n");
    body.push_str("  <title>");
    body.push_str(&title);
    body.push_str("</title>\n");
    body.push_str("  <script>");
    body.push_str(&chart_js);
    body.push_str("</script>\n");
    body.push_str(&css);
    body.push_str("</head>\n<body>\n  <div class=\"wrap\">\n");
    body.push_str("    <h1>\n      <span data-i18n=\"dash.title\">");
    body.push_str(&dash_title);
    body.push_str("</span>\n      <span class=\"lang-toggle\" role=\"group\" aria-label=\"language\">\n");
    body.push_str("        <button class=\"lang-btn active\" data-lang=\"zh\" data-i18n=\"ui.language_toggle_zh\">");
    body.push_str(&ui_lang_zh);
    body.push_str("</button>\n        <button class=\"lang-btn\" data-lang=\"en\" data-i18n=\"ui.language_toggle_en\">");
    body.push_str(&ui_lang_en);
    body.push_str("</button>\n      </span>\n    </h1>\n");
    body.push_str("    <div class=\"sub\">");
    body.push_str(&dash_subtitle);
    body.push_str("</div>\n\n");
    // Runs table at the top.
    body.push_str("    <h2 data-i18n=\"dash.runs_heading\">");
    body.push_str(&dash_runs_heading);
    body.push_str("</h2>\n");
    body.push_str("    <div class=\"runs-wrap\">");
    body.push_str(&runs_table);
    body.push_str("</div>\n");
    body.push_str(&no_runs_message);
    body.push_str("\n");
    // Compare picker.
    body.push_str("    <h2 data-i18n=\"dash.compare_pick\">");
    body.push_str(&dash_compare_pick);
    body.push_str("</h2>\n    <div class=\"compare-bar\">\n");
    body.push_str("      <label for=\"run-a\" data-i18n=\"dash.col_a\">A</label>\n      <select id=\"run-a\"></select>\n");
    body.push_str("      <label for=\"run-b\" data-i18n=\"dash.col_b\">B</label>\n      <select id=\"run-b\"></select>\n");
    body.push_str("      <a class=\"btn\" id=\"compare-btn\">");
    body.push_str(&dash_compare);
    body.push_str("</a>\n      <a class=\"btn secondary\" id=\"reset-btn\">");
    body.push_str(&dash_reset);
    body.push_str("</a>\n    </div>\n\n");
    // Diff view (rendered by JS at runtime).
    body.push_str("    <div id=\"compare-view\" class=\"hidden\">\n");
    body.push_str("      <h2 data-i18n=\"dash.diff_heading\">");
    body.push_str(&dash_diff_heading);
    body.push_str("</h2>\n");
    body.push_str("      <div id=\"run-heads\" style=\"display:flex;gap:16px;margin-bottom:12px;flex-wrap:wrap;\"></div>\n");
    body.push_str("      <table class=\"diff-table\" id=\"diff-table\"></table>\n");
    body.push_str("      <div class=\"charts\">\n");
    body.push_str("        <div class=\"chart-card\"><h3 id=\"lat-a-head\">A latency (ms)</h3><canvas id=\"latA\"></canvas></div>\n");
    body.push_str("        <div class=\"chart-card\"><h3 id=\"lat-b-head\">B latency (ms)</h3><canvas id=\"latB\"></canvas></div>\n");
    body.push_str("      </div>\n    </div>\n\n");
    body.push_str("    <footer data-i18n=\"footer.powered_by\">");
    body.push_str(&footer_powered_by);
    body.push_str("</footer>\n  </div>\n\n");
    // I18n dicts + data block.
    body.push_str("  <script id=\"i18n-dict-en\" type=\"application/json\">");
    body.push_str(&i18n_dict_en_safe);
    body.push_str("</script>\n  <script id=\"i18n-dict-zh\" type=\"application/json\">");
    body.push_str(&i18n_dict_zh_safe);
    body.push_str("</script>\n  <script id=\"dash-data\" type=\"application/json\">");
    body.push_str(&runs_json);
    body.push_str("</script>\n");

    body.push_str("  <script>\n    (function() {\n      const ZH = JSON.parse(document.getElementById('i18n-dict-zh').textContent);\n      const EN = JSON.parse(document.getElementById('i18n-dict-en').textContent);\n      const STORAGE_KEY = 'power_test.lang';\n      const DEFAULT_LANG = 'zh';\n      function applyLang(lang) {\n        const dict = (lang === 'en') ? EN : ZH;\n        document.documentElement.lang = lang;\n        document.querySelectorAll('[data-i18n]').forEach(function(el) {\n          const key = el.getAttribute('data-i18n');\n          if (dict[key] !== undefined) { el.textContent = dict[key]; }\n        });\n        document.querySelectorAll('.lang-toggle .lang-btn').forEach(function(b) {\n          if (b.dataset.lang === lang) { b.classList.add('active'); } else { b.classList.remove('active'); }\n        });\n        try { localStorage.setItem(STORAGE_KEY, lang); } catch (e) {}\n      }\n      document.querySelectorAll('.lang-toggle .lang-btn').forEach(function(btn) {\n        btn.addEventListener('click', function() { applyLang(btn.dataset.lang); });\n      });\n      let stored = null;\n      try { stored = localStorage.getItem(STORAGE_KEY); } catch (e) {}\n      applyLang(stored === 'en' ? 'en' : DEFAULT_LANG);\n    })();\n  </script>\n");
    body.push_str("  <script>\n    (function() {\n      const DATA = JSON.parse(document.getElementById('dash-data').textContent);\n      const selA = document.getElementById('run-a');\n      const selB = document.getElementById('run-b');\n      const compareBtn = document.getElementById('compare-btn');\n      const resetBtn = document.getElementById('reset-btn');\n      const compareView = document.getElementById('compare-view');\n      function populate() {\n        [selA, selB].forEach(function(sel, idx) {\n          sel.innerHTML = '';\n          DATA.forEach(function(r, i) {\n            const opt = document.createElement('option');\n            opt.value = String(i);\n            opt.textContent = r.run_id;\n            sel.appendChild(opt);\n          });\n          sel.selectedIndex = idx === 0 ? 0 : Math.max(0, DATA.length - 1);\n        });\n      }\n      populate();\n      function delta(a, b) {\n        const abs = b - a;\n        const pct = (Math.abs(a) > 1e-9) ? (100 * abs / a) : null;\n        return { abs: abs, pct: pct };\n      }\n      function colorClass(delta, direction) {\n        if (direction === 'neutral' || delta.pct === null) return 'neutral';\n        if (Math.abs(delta.pct) < 0.5) return 'neutral';\n        const positive = delta.pct > 0;\n        if (direction === 'up') return positive ? 'good' : 'bad';\n        if (direction === 'down') return positive ? 'bad' : 'good';\n        return 'neutral';\n      }\n      function fmtNum(v, decimals) { return (v || 0).toFixed(decimals || 0); }\n      function fmtPct(v) { return (v || 0).toFixed(1) + '%'; }\n      function fmtDelta(d, showPct, decimals) {\n        const sign = d.abs >= 0 ? '+' : '';\n        const abs = decimals === undefined ? sign + d.abs.toFixed(2) : sign + d.abs.toFixed(decimals);\n        if (!showPct) return abs;\n        if (d.pct === null) return abs + ' (n/a)';\n        return abs + ' (' + sign + d.pct.toFixed(2) + '%)';\n      }\n      const ZH = JSON.parse(document.getElementById('i18n-dict-zh').textContent);\n      const EN = JSON.parse(document.getElementById('i18n-dict-en').textContent);\n      function labelFor(key) {\n        const dict = (document.documentElement.lang === 'en') ? EN : ZH;\n        return dict[key] || key;\n      }\n      function buildRows(a, b) {\n        return [\n          ['dash.metric.achieved_rps', a.achieved_rps, b.achieved_rps, 'up', true, 2],\n          ['dash.metric.total_requests', a.total_requests, b.total_requests, 'up', false, 0],\n          ['dash.metric.success_rate', a.success_rate, b.success_rate, 'up', true, 2],\n          ['dash.metric.latency_p50', a.latency_ms.p50, b.latency_ms.p50, 'down', true, 2],\n          ['dash.metric.latency_p90', a.latency_ms.p90, b.latency_ms.p90, 'down', true, 2],\n          ['dash.metric.latency_p99', a.latency_ms.p99, b.latency_ms.p99, 'down', true, 2],\n          ['dash.metric.latency_p999', a.latency_ms.p99_9, b.latency_ms.p99_9, 'down', true, 2],\n          ['dash.metric.ttft_p50', a.ttft_ms.first, b.ttft_ms.first, 'down', true, 2],\n          ['dash.metric.ttft_p99', a.ttft_ms.second, b.ttft_ms.second, 'down', true, 2],\n          ['dash.metric.itl_mean', a.itl_ms.first, b.itl_ms.first, 'down', true, 2],\n          ['dash.metric.itl_p99', a.itl_ms.second, b.itl_ms.second, 'down', true, 2],\n          ['dash.metric.tps_mean', a.tps.first, b.tps.first, 'up', true, 2],\n          ['dash.metric.tps_p99', a.tps.second, b.tps.second, 'up', true, 2],\n          ['dash.metric.total_tokens', a.total_completion_tokens, b.total_completion_tokens, 'up', false, 0],\n          ['dash.metric.duration', a.duration_secs, b.duration_secs, 'neutral', true, 2],\n          ['dash.metric.cache_hit_rate', a.cache.rate_overall, b.cache.rate_overall, 'up', true, 2],\n          ['dash.metric.cache_creation_total', a.cache.creation_total, b.cache.creation_total, 'neutral', false, 0],\n        ];\n      }\n      let chartA = null;\n      let chartB = null;\n      function clearCharts() {\n        if (chartA) { chartA.destroy(); chartA = null; }\n        if (chartB) { chartB.destroy(); chartB = null; }\n      }\n      function renderCompare() {\n        if (DATA.length === 0) { compareView.classList.add('hidden'); return; }\n        const ai = parseInt(selA.value, 10);\n        const bi = parseInt(selB.value, 10);\n        if (isNaN(ai) || isNaN(bi)) { compareView.classList.add('hidden'); return; }\n        const a = DATA[ai];\n        const b = DATA[bi];\n        compareView.classList.remove('hidden');\n        // Diff names: run_id is the target URL (clickable).\n        // The target is the config target (e.g. the LLM endpoint);\n        // making it clickable lets the user copy the URL or open\n        // it in a new tab from the dashboard.\n        document.getElementById('run-heads').innerHTML =\n          '<div><strong>' + (document.documentElement.lang === 'en' ? 'A:' : 'A:') + ' </strong>' +\n            '<a href=\"' + escapeAttr(a.target) + '\" target=\"_blank\" rel=\"noopener noreferrer\" title=\"' + escapeAttr(a.target) + '\">' + escapeHtml(a.run_id) + '</a></div>' +\n          '<div><strong>' + (document.documentElement.lang === 'en' ? 'B:' : 'B:') + ' </strong>' +\n            '<a href=\"' + escapeAttr(b.target) + '\" target=\"_blank\" rel=\"noopener noreferrer\" title=\"' + escapeAttr(b.target) + '\">' + escapeHtml(b.run_id) + '</a></div>';\n        const rows = buildRows(a, b);\n        let html = '<thead><tr><th>' + (document.documentElement.lang === 'en' ? 'metric' : 'metric') + '</th><th>A</th><th>B</th><th>\\u0394</th><th>%</th></tr></thead><tbody>';\n        rows.forEach(function(r) {\n          const key = r[0]; const av = r[1]; const bv = r[2]; const dir = r[3]; const showPct = r[4]; const decimals = r[5];\n          const d = delta(av, bv);\n          const cls = colorClass(d, dir);\n          const deltaStr = fmtDelta(d, showPct, decimals);\n          const pctStr = showPct ? (d.pct === null ? '\\u2014' : (d.pct >= 0 ? '+' : '') + d.pct.toFixed(2) + '%') : '\\u2014';\n          html += '<tr>' +\n            '<td>' + labelFor(key) + '</td>' +\n            '<td class=\"num\">' + (typeof av === 'number' ? (decimals === 0 ? fmtNum(av) : av.toFixed(decimals)) : av) + '</td>' +\n            '<td class=\"num\">' + (typeof bv === 'number' ? (decimals === 0 ? fmtNum(bv) : bv.toFixed(decimals)) : bv) + '</td>' +\n            '<td class=\"num delta ' + cls + '\">' + deltaStr + '</td>' +\n            '<td class=\"num delta ' + cls + '\">' + pctStr + '</td>' +\n            '</tr>';\n        });\n        if (a.run_id === b.run_id) {\n          html += '<tr><td colspan=\"5\" style=\"color: var(--muted); font-style: italic; text-align: center;\">' + labelFor('dash.pick_same_run_warning') + '</td></tr>';\n        }\n        html += '</tbody>';\n        document.getElementById('diff-table').innerHTML = html;\n        clearCharts();\n        const palette = ['#58a6ff', '#3fb950', '#d29922', '#a371f7', '#f85149', '#8b949e'];\n        const chartFont = { size: 12 };\n        document.getElementById('lat-a-head').textContent = a.run_id + ' \\u00b7 ' + a.target + ' latency (ms)';\n        document.getElementById('lat-b-head').textContent = b.run_id + ' \\u00b7 ' + b.target + ' latency (ms)';\n        const lats = ['p50', 'p90', 'p99', 'p99.9'];\n        const latsKey = ['p50', 'p90', 'p99', 'p99_9'];\n        const avals = latsKey.map(function(k) { return a.latency_ms[k]; });\n        const bvals = latsKey.map(function(k) { return b.latency_ms[k]; });\n        if (window.Chart) {\n          chartA = new Chart(document.getElementById('latA'), {\n            type: 'bar', data: { labels: lats, datasets: [{ label: 'latency', data: avals, backgroundColor: palette[0] }] },\n            options: { responsive: true, maintainAspectRatio: false, plugins: { legend: { display: false } }, scales: { y: { beginAtZero: true, ticks: { font: chartFont } }, x: { ticks: { font: chartFont } } } }\n          });\n          chartB = new Chart(document.getElementById('latB'), {\n            type: 'bar', data: { labels: lats, datasets: [{ label: 'latency', data: bvals, backgroundColor: palette[1] }] },\n            options: { responsive: true, maintainAspectRatio: false, plugins: { legend: { display: false } }, scales: { y: { beginAtZero: true, ticks: { font: chartFont } }, x: { ticks: { font: chartFont } } } }\n          });\n        }\n      }\n      function escapeHtml(s) { return String(s).replace(/[&<>\"']/g, function(c) { return {'&': '&amp;', '<': '&lt;', '>': '&gt;', '\"': '&quot;', \"'\": '&#39;'}[c]; }); }\n      function escapeAttr(s) { return String(s).replace(/\"/g, '&quot;'); }\n      compareBtn.addEventListener('click', renderCompare);\n      selA.addEventListener('change', renderCompare);\n      selB.addEventListener('change', renderCompare);\n      resetBtn.addEventListener('click', function() {\n        if (DATA.length >= 2) { selA.selectedIndex = 0; selB.selectedIndex = DATA.length - 1; }\n        else { selA.selectedIndex = 0; selB.selectedIndex = 0; }\n        renderCompare();\n      });\n      window.addEventListener('load', renderCompare);\n    })();\n  </script>\n");
    body.push_str("</body>\n</html>\n");
    body
}
