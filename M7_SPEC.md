# M7 spec — model dashboard + cache always-shown + bilingual reports

## Goal

Three independent changes that ship together as M7. They are designed
to be reviewable and revertible one PR at a time even though they
land as a single milestone.

1. **Model dashboard** — a self-contained HTML page per model
   (`<root>/<model>/index.html`) that lists every run of that model
   and lets the reader pick two runs to compare side-by-side.
2. **Cache always shown** — every HTML report, every text summary,
   and the new model dashboard always render the prompt-cache
   section, even when the run had no cache data.
3. **Bilingual reports (zh / en, default zh)** — every HTML report
   ships with Chinese as the default language and a JS toggle to
   flip to English. Persisted in `localStorage`.

## Non-goals

- No new CLI subcommand surface beyond `power_test dashboard`.
- No new external dependencies. JS toggle is hand-rolled, no i18n
  library. Chart.js stays inlined (assets/chart.umd.min.js).
- No new storage layout. The dashboard reuses the existing
  `<root>/<model>/<run_id>/` directories; the only new file is
  `<root>/<model>/index.html`.
- No changes to the Rust `compare` CLI text output. The diff math
  in the model dashboard is ported to JS; the Rust `compare`
  pipeline is unchanged.
- No new HTTP server. The dashboard is a static file you open
  with `file://` (same as todays `report.html`).
- No cache stats for the `raw` API kind. The client leaves both
  cache fields at 0, the dashboard shows that as
  `0.0% (no cache observed)` — same shape as a single-turn OpenAI
  run that does not report cached tokens.

## Design overview

### 1. i18n module — `src/report/i18n.rs` (new)

A tiny lookup-table i18n. No external deps. Two locales (`zh`, `en`),
default `zh`. Key naming: dot-separated paths
(`metric.latency.p50`, `cache.hit_rate`, `dashboard.compare_pick`,
`run_card.target`, etc.). Unknown keys fall back to the key string
itself so a missing translation never silently renders an empty cell.

```rust
pub enum Locale { Zh, En }
impl Locale { pub fn default() -> Self { Locale::Zh } }
pub fn t(locale: Locale, key: &str) -> &'static str { ... }
```

The HTML body is rendered in the **default** locale (`zh`) at build
time. All translatable elements carry a `data-i18n="<key>"`
attribute. A `<script id="i18n-dict">` block at the top of `<body>`
contains the full `en` dictionary as JSON. A small inline script:

- reads `localStorage.getItem('power_test.lang')` (default `zh`),
- on toggle button click, swaps every `[data-i18n]` element's
  `textContent` with the matching key in the dictionary, and
- updates `<html lang="...">` and the toggle button label.

This avoids threading a `Locale` parameter through every renderer.

### 2. `src/report/html.rs` — labels go through `t(Locale::Zh, ...)`

Every user-visible label in `render_html_with_compare` is replaced
with `t(Locale::Zh, '<key>')` lookups. Each gets a
`data-i18n='<key>'` attribute so the JS toggle can flip it.

The `metrics-data` JSON block keeps its existing English keys
(`summary`, `latencyPercentiles`, ...). Chart.js speaks English
labels natively. We do **not** translate chart axis labels in this
milestone.

### 3. `src/compare.rs` — same treatment

The HTML compare page is re-rendered with the same i18n approach.
The text compare output stays English.

### 4. Cache card always-shown — `src/report/html.rs::build_cache_card`

Todays `build_cache_card` returns `''` when
`c.cache_creation_total == 0 && c.cache_hit_total == 0`. Change:

- Always emit the `<h2>Prompt cache</h2>` + card block.
- Headline reads `0.0% (no cache observed)` in the no-data case;
  otherwise just the percent.
- Add a small 'denominator = 0' hint line when the run had zero
  prompt tokens (e.g. all requests errored).
- The two bar rows (`Overall`, `Turn 1`) are always present.
  The `Turn 2+` row is **hidden** when `c.prompt_turn2plus == 0`
  (no continuation turns were observed, i.e. either single-turn
  only or a multi-turn run that never advanced past turn 1).

The summary's 'prompt cache' block is updated the same way.

### 5. Model dashboard — `src/report/model_dashboard.rs` (new)

A new module. Public API:

```rust
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

pub fn run_summary_from_run(
    config: &RunConfig,
    agg: &MetricsAggregator,
    report_filename: impl Into<String>,
) -> RunSummary;

pub fn render_dashboard(
    group_key: &str,
    model: &str,
    alias: Option<&str>,
    runs: &[RunSummary],
) -> String;
```

`render_dashboard` is a pure function: same inputs → same output.
It does no I/O. The caller is responsible for writing the file.

### 6. Dashboard diff — JS-side

The inline `<script>` at the bottom of the dashboard computes the
diff using the embedded `RunSummary[]`. Math is ported from
`src/compare.rs` (`Delta::new`, `color_class` rules) to a small
JS function. Each delta cell is colored green / red / grey using
the same 0.5% tolerance threshold. Two bar charts reuse chart.js.

### 7. CLI — `power_test dashboard [NAME]`

A new subcommand:

```
power_test dashboard                  # render dashboards for every model
power_test dashboard <NAME>           # render one model
power_test dashboard <NAME> --no-write  # dry-run
```

New args struct `DashboardArgs { name: Option<String>, no_write: bool }`.

Wiring in `src/main.rs::cmd_dashboard`:

1. `list_runs(root)` → group by `effective_group_key`.
2. For each group, load every run's `config.json` + the
   `metrics.json` summary.
3. Build `Vec<RunSummary>`, call `render_dashboard(...)`.
4. Write to `<root>/<group_key>/index.html` (mkdir if missing).

### 8. Auto-regenerate on `save_run`

In `src/storage/history.rs::save_run`, after writing all four
artifacts and updating `index.json`, we call:

```rust
pub fn regenerate_dashboard_for_group(root: &Path, group_key: &str) -> Result<()>;
```

It re-loads the index, filters by `group_key`, builds the
summaries, and writes `<root>/<group_key>/index.html`. The write
is best-effort: a failure logs a `tracing::warn!` and returns
`Ok(())` so a dashboard-render error never blocks the real save.

This hook is also called from `cmd_report` so the dashboard picks
up the latest metrics after a manual re-render.

### 9. File-by-file change list

| File | Change |
|---|---|
| `src/report/i18n.rs` | **new** — `Locale`, `t()`, `zh_dict()`, `en_dict()`. |
| `src/report/mod.rs` | add `pub mod i18n;` + re-exports. |
| `src/report/html.rs` | replace literal labels with `t(Zh, ...)`; add `data-i18n` attrs; emit `<script id="i18n-dict">`; add language toggle button; rewrite `build_cache_card` to always emit. |
| `src/report/summary.rs` | always emit the cache block; no i18n (CLI output stays English). |
| `src/report/model_dashboard.rs` | **new** — `RunSummary`, `render_dashboard`, `run_summary_from_run`. |
| `src/compare.rs` | translate the HTML compare page labels; no change to text output. |
| `src/storage/history.rs` | add `regenerate_dashboard_for_group`; call at the end of `save_run` (best-effort); export. |
| `src/cli.rs` | add `Dashboard(DashboardArgs)` variant + tests. |
| `src/main.rs` | add `cmd_dashboard`; dispatch `Command::Dashboard`. |
| `docs/references/report-interpretation.md` | document the dashboard + cache always-shown + i18n. |
| `README.md` | add a 'Model dashboard' section + a 'Languages' note. |
| `M7_SPEC.md` | this file. |
| `AGENTS.md` | update the M-series test inventory + layout block. |

### 10. i18n dictionary (selected keys; full list in src/report/i18n.rs)

The Rust side holds both `zh` and `en` dictionaries. `t(Locale::Zh, k)`
returns the Chinese string, `t(Locale::En, k)` returns English.
The HTML body is rendered in `zh` and the `en` dict is embedded in
the `<script id="i18n-dict">` block. Key categories (sample):

- `ui.*` — compare-with UI, language toggle labels
- `card.*` — top-of-page metric cards
- `config.*` — configuration tables
- `metric.*` — summary statistics table headers / percentile labels
- `summary.heading`, `charts.heading`, `errors.*`
- `cache.*` — cache section
- `dash.*` — model dashboard only

### 11. Tests

| Layer | New tests |
|---|---|
| `report/i18n.rs` | `lookup_zh_default`; `lookup_en_returns_english`; `missing_key_returns_key_string`; `dictionaries_have_same_keys`. |
| `report/html.rs` | `html_default_lang_is_zh`; `html_contains_chinese_labels`; `html_contains_language_toggle_button`; `html_embeds_i18n_dict`; `cache_card_renders_zero_state_when_no_data`; `cache_card_hides_turn2plus_when_no_continuation_turns`; `cache_card_renders_for_single_turn_with_data`. Replaces `html_omits_cache_card_when_no_data`. |
| `report/summary.rs` | `summary_always_includes_cache_section`; `summary_cache_zero_state_text`. |
| `report/model_dashboard.rs` | `run_summary_captures_headline_metrics`; `run_summary_cache_stats_match_aggregator`; `dashboard_html_lists_all_runs`; `dashboard_html_default_lang_is_zh`; `dashboard_html_embeds_run_summaries`; `dashboard_html_has_compare_picker`; `dashboard_html_compare_view_initially_hidden`; `dashboard_html_no_runs_message`; `dashboard_html_for_two_runs_renders_diff_columns`. |
| `storage/history.rs` | `save_run_regenerates_dashboard_for_group`; `regenerate_dashboard_creates_file`; `regenerate_dashboard_is_idempotent`; `regenerate_dashboard_filters_by_group_key`. |
| `cli.rs` | `dashboard_subcommand_present`; `dashboard_args_parses_name`; `dashboard_args_parses_no_write`. |
| e2e (`tests/e2e.rs`) | `e2e_dashboard_for_one_model_2_runs`; `e2e_save_run_auto_regenerates_dashboard`. |

### 12. Backward compatibility

- Old `report.html` files still load (we add labels but never
  remove test-id substrings). The new test asserts
  `<html lang="zh">`; the old `lang="en"` assertion is updated.
- The new `<script id="i18n-dict">` block is additive. Browsers
  that do not run the toggle still see a valid Chinese page.
- `cache_stats()` math is unchanged. We only change the renderer
  to always emit. Old `metrics.json` files deserialize the same way.
- `<root>/<model>/index.html` is a new file. If the user has a
  hand-rolled `index.html` at that path, our
  `regenerate_dashboard_for_group` will overwrite it. We document
  this in the README.
- `power_test dashboard` is a new subcommand. Existing scripts
  using `compare` / `list` / `report` / `run` are unaffected.

### 13. Constraints (carry-over from M1–M6)

- `#![deny(warnings)]` — every change must keep the tree warning-free.
- Stable Rust, async I/O only.
- No new external deps. The dashboard's diff math is hand-rolled
  in JS.
- All M1–M6 tests must still pass (with the 1 known rename:
  `html_omits_cache_card_when_no_data` →
  `cache_card_renders_zero_state_when_no_data`).
- `RunConfig` new fields stay `#[serde(default)]`. We do not touch
  `RunConfig` in M7 — only renderer code.
- Do not touch `target/`, `~/.power_test/history/`, `Cargo.lock`.

## Deliverable format (same as M5/M6)

- `BUILD: pass` / `BUILD: fail` + the error.
- `TESTS: <n> passed, <n> failed` from the last `cargo test`.
- `NEW FILES: src/report/i18n.rs, src/report/model_dashboard.rs, M7_SPEC.md` + docs updates.
- `MODIFIED FILES: ...` (the list from §9).
- `OPEN ITEMS: ...` (anything that did not land in this PR).
