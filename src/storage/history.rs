//! History directory layout (M6f): `<root>/<model>/<run-id>/{config.json,
//! metrics.json, report.html, summary.txt}`. The model subdirectory makes
//! the disk layout self-documenting — `ls ~/.power_test/history/` shows
//! one folder per model, and `ls ~/.power_test/history/MiniMax-M3/`
//! shows all runs against that model. URL is intentionally NOT in the
//! path because URLs are awkward (contain `/`, `?`, `:`, etc.) and
//! the same model is often tested against multiple URLs; the URL
//! remains visible in the report's `Target` card.
//!
//! An index file at `<root>/index.json` lists runs for fast `list`
//! queries. The index carries a `model` field per entry so `load_run`
//! can find the right subdirectory without scanning.
//!
//! Backward compat: runs saved before M6f used the flat layout
//! `<root>/<run-id>/`. `load_run` falls back to the flat path if the
//! model-keyed directory doesn't exist, with a tracing warning. This
//! means an existing history dir continues to work without a manual
//! migration step.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use serde::{Deserialize, Serialize};

use crate::config::{RunConfig, RunStatus};
use crate::error::{Error, Result};
use crate::runner::{MetricsAggregator, RequestRecord};

/// A summary row for `power_test list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub run_id: String,
    /// M6i fix: same as `RunConfig.started_at`, this is
    /// the local-time wall clock. `power_test list` and
    /// the index sort by this field, so the user's
    /// `~/.power_test/history/<model>/` listing will
    /// match their shell `date`. Was `DateTime<Utc>`
    /// previously. Old `index.json` files (UTC strings)
    /// deserialize fine since the wire format is
    /// identical (ISO 8601).
    pub timestamp: DateTime<Local>,
    pub target: String,
    /// M6f: model name. `Option` so old `index.json` files
    /// (pre-M6f) still deserialize; the missing field is treated
    /// as "no model info" and `load_run` falls back to scanning
    /// the flat layout for those entries.
    #[serde(default)]
    pub model: Option<String>,
    /// M6g: alias for the model. When set, the run lives under
    /// `<root>/<alias>/<run_id>/` and the compare-with dropdown
    /// only lists other runs of the same alias. `Option` for
    /// back-compat with M6f index files (no alias field).
    #[serde(default)]
    pub model_alias: Option<String>,
    pub rps: f64,
    pub duration_secs: u64,
    pub status: RunStatus,
    pub tag: Option<String>,
    pub total_requests: u64,
    pub error_count: u64,
}

/// Create the history root directory if it does not exist.
pub fn ensure_history_dir(root: &Path) -> Result<PathBuf> {
    if !root.exists() {
        fs::create_dir_all(root)
            .map_err(|e| crate::error::Error::io_at(root, e))?;
    }
    Ok(root.to_path_buf())
}

/// Make a model name safe to use as a directory component. Replaces
/// anything outside `[A-Za-z0-9._-]` with `_`. Empty / whitespace-only
/// names fall back to `_unnamed_`. The original `HistoryEntry.model`
/// retains the unsanitized string — this is purely for the
/// filesystem path.
fn sanitize_model_dir(model: &str) -> String {
    let trimmed = model.trim();
    if trimmed.is_empty() {
        return "_unnamed_".into();
    }
    let mut out = String::with_capacity(trimmed.len());
    for c in trimmed.chars() {
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    out
}

/// Compute the on-disk path for a run: `<root>/<group_key>/<run_id>/`.
/// Pure path math — no I/O.
///
/// `group_key` is whatever the caller chose — typically the
/// effective group key from [`effective_group_key`]. We sanitize
/// the string so model / alias names with `/` `?` `:` etc. are
/// safe to use as a directory component.
pub fn run_dir(root: &Path, group_key: &str, run_id: &str) -> PathBuf {
    root.join(sanitize_model_dir(group_key)).join(run_id)
}

/// M6g: resolve the group key for a run. The alias wins when
/// present and non-empty; otherwise the model name is used.
/// This is the single source of truth for "which subdirectory
/// does this run belong in" and "which other runs is this
/// comparable with".
pub fn effective_group_key<'a>(model: &'a str, alias: Option<&'a str>) -> &'a str {
    match alias {
        Some(a) if !a.trim().is_empty() => a,
        _ => model,
    }
}

/// Save a run's artifacts to `<root>/<model>/<run_id>/`. Writes
/// `config.json`, `metrics.json`, `report.html`, and `summary.txt` in
/// that order.
pub fn save_run(
    root: &Path,
    run_id: &str,
    config: &RunConfig,
    metrics: &MetricsAggregator,
    summary_text: &str,
    report_html: &str,
    status: RunStatus,
) -> Result<HistoryEntry> {
    ensure_history_dir(root)?;
    // M6g: group by alias (if set) so dated snapshots of the
    // same underlying model land in the same subdirectory.
    let group_key = effective_group_key(&config.model, config.model_alias.as_deref());
    let dir = run_dir(root, group_key, run_id);
    fs::create_dir_all(&dir).map_err(|e| crate::error::Error::io_at(&dir, e))?;

    let config_json = serde_json::to_string_pretty(config)?;
    fs::write(dir.join("config.json"), config_json)
        .map_err(|e| crate::error::Error::io_at(dir.join("config.json"), e))?;

    let metrics_json = serde_json::to_string_pretty(&crate::runner::aggregator_to_json(metrics))?;
    fs::write(dir.join("metrics.json"), metrics_json)
        .map_err(|e| crate::error::Error::io_at(dir.join("metrics.json"), e))?;

    fs::write(dir.join("report.html"), report_html)
        .map_err(|e| crate::error::Error::io_at(dir.join("report.html"), e))?;

    fs::write(dir.join("summary.txt"), summary_text)
        .map_err(|e| crate::error::Error::io_at(dir.join("summary.txt"), e))?;

    let entry = HistoryEntry {
        run_id: run_id.to_string(),
        timestamp: config.started_at,
        target: config.target.clone(),
        model: Some(config.model.clone()),
        model_alias: config.model_alias.clone(),
        rps: config.target_rps,
        duration_secs: config.duration_secs,
        status,
        tag: config.tag.clone(),
        total_requests: metrics.total_requests(),
        error_count: metrics.error_count(),
    };

    // Update index.
    let index_path = root.join("index.json");
    let mut index: BTreeMap<String, HistoryEntry> = if index_path.exists() {
        serde_json::from_str(&fs::read_to_string(&index_path)?).unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    index.insert(run_id.to_string(), entry.clone());
    let index_json = serde_json::to_string_pretty(&index)?;
    fs::write(&index_path, index_json)
        .map_err(|e| crate::error::Error::io_at(&index_path, e))?;

    // M7: best-effort auto-regenerate of the model dashboard.
    // A failure here logs a warning and returns Ok — we never
    // fail a real save because the dashboard render hit a snag.
    let _ = regenerate_dashboard_for_group(root, &group_key);

    Ok(entry)
}

/// List runs in the history root, newest first. Falls back to scanning the
/// filesystem if the index is missing or corrupt.
pub fn list_runs(root: &Path) -> Result<Vec<HistoryEntry>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let index_path = root.join("index.json");
    if index_path.exists() {
        match fs::read_to_string(&index_path) {
            Ok(text) => match serde_json::from_str::<BTreeMap<String, HistoryEntry>>(&text) {
                Ok(map) => {
                    let mut v: Vec<HistoryEntry> = map.into_values().collect();
                    v.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                    return Ok(v);
                }
                Err(e) => {
                    tracing::warn!("index.json is corrupt: {e}; scanning directory");
                }
            },
            Err(e) => {
                tracing::warn!("could not read index.json: {e}; scanning directory");
            }
        }
    }
    // Fallback: walk two levels deep
    // (`<root>/<model>/<run_id>/`). Each run_id dir has a
    // `config.json` that gives us the model + target.
    let mut entries = Vec::new();
    for model_entry in fs::read_dir(root)? {
        let model_entry = model_entry?;
        if !model_entry.file_type()?.is_dir() {
            continue;
        }
        // Skip non-model files like `index.json` at the root.
        let model_name = match model_entry.file_name().into_string() {
            Ok(s) => s,
            Err(_) => continue,
        };
        for run_entry in fs::read_dir(model_entry.path())? {
            let run_entry = run_entry?;
            if !run_entry.file_type()?.is_dir() {
                continue;
            }
            let run_id = match run_entry.file_name().into_string() {
                Ok(s) => s,
                Err(_) => continue,
            };
            let dir = run_entry.path();
            if let Ok(cfg) = load_config(&dir) {
                entries.push(HistoryEntry {
                    run_id,
                    timestamp: cfg.started_at,
                    target: cfg.target,
                    model: Some(if cfg.model.is_empty() {
                        model_name.clone()
                    } else {
                        cfg.model
                    }),
                    model_alias: cfg.model_alias,
                    rps: cfg.target_rps,
                    duration_secs: cfg.duration_secs,
                    status: RunStatus::Completed,
                    tag: cfg.tag,
                    total_requests: 0,
                    error_count: 0,
                });
            }
        }
    }
    entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(entries)
}

/// Load the `config.json` for a run.
pub fn load_config(dir: &Path) -> Result<RunConfig> {
    let text = fs::read_to_string(dir.join("config.json"))
        .map_err(|e| crate::error::Error::io_at(dir.join("config.json"), e))?;
    Ok(serde_json::from_str(&text)?)
}

/// Load a run by id from the history root. Returns `(config, metrics_json_value)`.
///
/// M6g: the directory layout is `<root>/<group_key>/<run_id>/`
/// where `group_key` is the alias (when set) or the model
/// name. We look up the run's `model` and `model_alias` in the
/// global `index.json` first, then read from the
/// group-keyed subdirectory. As a fallback for pre-M6f runs
/// (flat `<root>/<run_id>/`), we also try the old path with a
/// tracing warning. The flat fallback is the only reason we
/// don't error on missing index.json — we need to keep the old
/// runs readable.
pub fn load_run(root: &Path, run_id: &str) -> Result<(RunConfig, serde_json::Value)> {
    // Preferred path: use the index to find the run's
    // effective group key.
    if let Some(entry) = lookup_index(root, run_id) {
        let group_key = effective_group_key(
            entry.model.as_deref().unwrap_or(""),
            entry.model_alias.as_deref(),
        );
        if !group_key.is_empty() {
            let dir = run_dir(root, group_key, run_id);
            if dir.is_dir() {
                return load_run_from(&dir);
            }
            // Index says group_key=X but the directory doesn't
            // exist — could be a corrupt run or a
            // pre-migration state. Fall through to the
            // flat-layout fallback below.
            tracing::warn!(
                "index.json lists run_id={run_id} under group_key={group_key:?} but {} doesn't exist; trying flat layout",
                dir.display()
            );
        }
    }
    // Fallback: pre-M6f flat layout `<root>/<run_id>/`.
    let flat = root.join(run_id);
    if flat.is_dir() {
        tracing::warn!(
            "run_id={run_id} found at flat path {}; this is a pre-M6f run — re-save to migrate",
            flat.display()
        );
        return load_run_from(&flat);
    }
    Err(crate::error::Error::RunNotFound(run_id.to_string()))
}

/// Read `config.json` + `metrics.json` from a known run directory.
/// Helper for `load_run`; split out so the fallback path can reuse it.
fn load_run_from(dir: &Path) -> Result<(RunConfig, serde_json::Value)> {
    let config = load_config(dir)?;
    let metrics_text = fs::read_to_string(dir.join("metrics.json"))
        .map_err(|e| crate::error::Error::io_at(dir.join("metrics.json"), e))?;
    let metrics: serde_json::Value = serde_json::from_str(&metrics_text)?;
    Ok((config, metrics))
}

/// List runs in the history root that share the same `target` URL as the
/// given one. Used by the compare UI to populate the "compare with…"
/// dropdown. Newest first, excluding the run itself when `exclude_run_id`
/// is `Some`.
///
/// Note: M6f prefers `list_runs_by_model` for the compare dropdown
/// (comparing across models is rarely meaningful). This function is
/// kept for backwards compatibility and for callers that explicitly
/// want same-URL grouping.
pub fn list_runs_by_target(
    root: &Path,
    target: &str,
    exclude_run_id: Option<&str>,
) -> Result<Vec<HistoryEntry>> {
    let all = list_runs(root)?;
    let mut filtered: Vec<HistoryEntry> = all
        .into_iter()
        .filter(|e| e.target == target)
        .filter(|e| exclude_run_id.map_or(true, |x| e.run_id != x))
        .collect();
    // `list_runs` already sorts newest-first, but be explicit.
    filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(filtered)
}

/// M6f: list runs that share the same `model` name. Powers the
/// report's "compare with…" dropdown — comparing two runs of
/// the same model across RPS / dataset / pattern is the
/// high-value case; comparing gpt-3.5 vs MiniMax-M3 is rarely
/// what the user wants. Newest first, excluding the run
/// itself when `exclude_run_id` is `Some`.
///
/// Note: M6g added `list_runs_by_alias` for the new
/// alias-aware grouping. This function filters on the literal
/// `model` field regardless of alias; it's useful for code
/// that wants "strict same model" semantics.
pub fn list_runs_by_model(
    root: &Path,
    model: &str,
    exclude_run_id: Option<&str>,
) -> Result<Vec<HistoryEntry>> {
    let all = list_runs(root)?;
    let mut filtered: Vec<HistoryEntry> = all
        .into_iter()
        .filter(|e| e.model.as_deref() == Some(model))
        .filter(|e| exclude_run_id.map_or(true, |x| e.run_id != x))
        .collect();
    filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(filtered)
}

/// M6g: list runs that share the same `group_key` — the
/// alias (if set) or the model name. This is the new
/// default for the compare-with dropdown: two runs of
/// `DeepSeek-V4-Flash-20260115` and
/// `DeepSeek-V4-Flash-20260201` with `--model-alias
/// DeepSeek-V4-Flash` group together.
///
/// Falls back to filtering on `model` when an entry has no
/// `model_alias`, so M6f runs (pre-alias) still get included
/// in a query for their own model name.
pub fn list_runs_by_alias(
    root: &Path,
    group_key: &str,
    exclude_run_id: Option<&str>,
) -> Result<Vec<HistoryEntry>> {
    let all = list_runs(root)?;
    let mut filtered: Vec<HistoryEntry> = all
        .into_iter()
        .filter(|e| {
            let e_key = effective_group_key(
                e.model.as_deref().unwrap_or(""),
                e.model_alias.as_deref(),
            );
            e_key == group_key
        })
        .filter(|e| exclude_run_id.map_or(true, |x| e.run_id != x))
        .collect();
    filtered.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
    Ok(filtered)
}

/// Read a single `HistoryEntry` from the index. Returns `None` if
/// the index is missing/corrupt or the run is not in it.
fn lookup_index(root: &Path, run_id: &str) -> Option<HistoryEntry> {
    let index_path = root.join("index.json");
    let text = fs::read_to_string(&index_path).ok()?;
    let map: BTreeMap<String, HistoryEntry> = serde_json::from_str(&text).ok()?;
    map.get(run_id).cloned()
}

/// Load everything needed to render a compare view for one run:
/// the parsed `RunConfig`, the per-request `RequestRecord`s, and the
/// `RunStatus` (which lives in the index, not the per-run directory).
pub fn load_compare_data(
    root: &Path,
    run_id: &str,
) -> Result<(RunConfig, Vec<RequestRecord>, RunStatus)> {
    let (config, metrics_json) = load_run(root, run_id)?;
    let records: Vec<RequestRecord> = match metrics_json.get("per_request") {
        Some(v) => serde_json::from_value(v.clone())
            .map_err(|e| Error::Other(format!("per_request: {e}")))?,
        None => Vec::new(),
    };
    let status = lookup_status(root, run_id).unwrap_or(RunStatus::Completed);
    Ok((config, records, status))
}

/// Read the status of a run from `index.json`. Returns `None` if the
/// index is missing/corrupt or the run is not in it.
fn lookup_status(root: &Path, run_id: &str) -> Option<RunStatus> {
    let index_path = root.join("index.json");
    let text = fs::read_to_string(&index_path).ok()?;
    let map: BTreeMap<String, HistoryEntry> = serde_json::from_str(&text).ok()?;
    map.get(run_id).map(|e| e.status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
        RunConfig,
    };
    use chrono::Utc;
    use tempfile::TempDir;

    fn make_config(run_id: &str) -> RunConfig {
        RunConfig {
            run_id: run_id.into(),
            target: "http://localhost:1234/v1/chat/completions".into(),
            api: ApiKind::Openai,
            model: "gpt-3.5-turbo".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 5.0 },
            max_tokens: 16,
            stream: true,
            target_rps: 5.0,
            duration_secs: 10,
            concurrency: 32,
            tag: None,
            api_key: None,
            started_at: Local::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
        }
    }

    fn make_agg() -> MetricsAggregator {
        let mut agg = MetricsAggregator::new();
        agg.set_run_started_at(Utc::now());
        agg
    }

    #[test]
    fn save_and_reload_round_trip() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let cfg = make_config("run-abc");
        let agg = make_agg();
        let entry = save_run(
            root,
            &cfg.run_id,
            &cfg,
            &agg,
            "summary text",
            "<html>report</html>",
            RunStatus::Completed,
        )
        .unwrap();
        assert_eq!(entry.run_id, "run-abc");
        assert_eq!(entry.model.as_deref(), Some("gpt-3.5-turbo"));

        // M6f: nested at `<root>/<model>/<run_id>/`.
        let dir = run_dir(root, "gpt-3.5-turbo", "run-abc");
        assert!(dir.join("config.json").exists());
        assert!(dir.join("metrics.json").exists());
        assert!(dir.join("report.html").exists());
        assert!(dir.join("summary.txt").exists());

        // Round-trip config
        let cfg2 = load_config(&dir).unwrap();
        assert_eq!(cfg2.run_id, cfg.run_id);
        assert_eq!(cfg2.target_rps, 5.0);

        // Round-trip run (load_run uses the index to find the
        // model-keyed directory).
        let (cfg3, m) = load_run(root, "run-abc").unwrap();
        assert_eq!(cfg3.run_id, "run-abc");
        assert_eq!(m["scheduled"], 0);

        // list
        let all = list_runs(root).unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].run_id, "run-abc");
    }

    #[test]
    fn load_missing_run_errors() {
        let tmp = TempDir::new().unwrap();
        let err = load_run(tmp.path(), "nope").unwrap_err();
        assert!(matches!(err, crate::error::Error::RunNotFound(_)));
    }

    #[test]
    fn list_empty_history_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let all = list_runs(tmp.path()).unwrap();
        assert!(all.is_empty());
    }

    #[test]
    fn list_runs_by_target_filters_and_excludes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();

        let target_a = "http://api-a.example.com/v1/chat/completions";
        let target_b = "http://api-b.example.com/v1/chat/completions";

        // Two runs on target_a, one on target_b.
        for (id, tgt) in [
            ("a-1", target_a),
            ("a-2", target_a),
            ("b-1", target_b),
        ] {
            let mut cfg = make_config(id);
            cfg.target = tgt.into();
            save_run(
                root,
                id,
                &cfg,
                &make_agg(),
                "summary",
                "<html></html>",
                RunStatus::Completed,
            )
            .unwrap();
        }

        let only_a = list_runs_by_target(root, target_a, None).unwrap();
        assert_eq!(only_a.len(), 2);
        assert!(only_a.iter().all(|e| e.target == target_a));

        // Excluding self leaves 1 run on target_a.
        let a_minus_a1 = list_runs_by_target(root, target_a, Some("a-1")).unwrap();
        assert_eq!(a_minus_a1.len(), 1);
        assert_eq!(a_minus_a1[0].run_id, "a-2");

        // Filtering on a non-existent target returns nothing.
        let none = list_runs_by_target(
            root,
            "http://nope.example.com/v1/chat/completions",
            None,
        )
        .unwrap();
        assert!(none.is_empty());
    }

    #[test]
    fn load_compare_data_returns_records_and_status() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let cfg = make_config("cmp-1");
        save_run(
            root,
            "cmp-1",
            &cfg,
            &make_agg(),
            "summary",
            "<html></html>",
            RunStatus::Completed,
        )
        .unwrap();

        let (cfg2, records, status) = load_compare_data(root, "cmp-1").unwrap();
        assert_eq!(cfg2.run_id, "cmp-1");
        assert!(records.is_empty());
        assert_eq!(status, RunStatus::Completed);
    }

    /// M6f: the on-disk layout is `<root>/<model>/<run_id>/`.
    /// `ls` on the history root should show one folder per model.
    #[test]
    fn save_run_creates_model_subdir() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut cfg = make_config("run-1");
        cfg.model = "MiniMax-M3".into();
        save_run(
            root,
            "run-1",
            &cfg,
            &make_agg(),
            "summary",
            "<html></html>",
            RunStatus::Completed,
        )
        .unwrap();

        // The path nesting should be exactly two levels.
        let nested = root.join("MiniMax-M3").join("run-1");
        assert!(
            nested.is_dir(),
            "expected nested dir at {}, got nothing",
            nested.display()
        );
        assert!(nested.join("config.json").exists());
        assert!(nested.join("metrics.json").exists());
        assert!(nested.join("report.html").exists());
        assert!(nested.join("summary.txt").exists());

        // The flat old path must NOT exist.
        let flat = root.join("run-1");
        assert!(
            !flat.exists(),
            "M6f: flat <root>/<run_id>/ must not be created"
        );

        // The saved HistoryEntry carries the original model name
        // (un-sanitized), so callers can display the real string.
        let entry = lookup_index(root, "run-1").unwrap();
        assert_eq!(entry.model.as_deref(), Some("MiniMax-M3"));
    }

    /// M6f: when two runs of different models share a run_id
    /// namespace, they should land in different model
    /// subdirectories without colliding.
    #[test]
    fn save_two_models_creates_two_subdirs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        for (rid, model) in [("r1", "gpt-3.5-turbo"), ("r2", "claude-3-haiku")] {
            let mut cfg = make_config(rid);
            cfg.model = model.into();
            save_run(
                root,
                rid,
                &cfg,
                &make_agg(),
                "summary",
                "<html></html>",
                RunStatus::Completed,
            )
            .unwrap();
        }
        assert!(root.join("gpt-3.5-turbo").join("r1").is_dir());
        assert!(root.join("claude-3-haiku").join("r2").is_dir());
        // r2 in gpt-3.5 should not exist.
        assert!(!root.join("gpt-3.5-turbo").join("r2").exists());
    }

    /// M6f: model names with awkward characters (e.g. `:` `/`
    /// `?`) must be sanitized into a safe directory name. The
    /// unsanitized original stays in `HistoryEntry.model` and
    /// `config.json`.
    #[test]
    fn sanitize_model_dir_replaces_path_separators() {
        assert_eq!(sanitize_model_dir("MiniMax-M3"), "MiniMax-M3");
        assert_eq!(sanitize_model_dir("openai/gpt-4"), "openai_gpt-4");
        assert_eq!(sanitize_model_dir("claude:3:haiku"), "claude_3_haiku");
        assert_eq!(sanitize_model_dir("model with spaces"), "model_with_spaces");
        assert_eq!(sanitize_model_dir(""), "_unnamed_");
        assert_eq!(sanitize_model_dir("   "), "_unnamed_");
    }

    /// M6f: pre-M6f runs saved at the flat path
    /// `<root>/<run_id>/` must still load. This is the
    /// back-compat path — no manual migration required.
    #[test]
    fn load_run_falls_back_to_flat_layout_for_old_runs() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let cfg = make_config("legacy-run");
        // Manually create a flat-layout run WITHOUT going
        // through save_run (which would now nest it).
        let flat = root.join("legacy-run");
        fs::create_dir_all(&flat).unwrap();
        fs::write(flat.join("config.json"), serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
        fs::write(flat.join("metrics.json"), "{}").unwrap();

        // Index is missing — load_run should still find the
        // run via the flat fallback.
        let (loaded_cfg, _metrics) = load_run(root, "legacy-run").unwrap();
        assert_eq!(loaded_cfg.run_id, "legacy-run");
        assert_eq!(loaded_cfg.model, "gpt-3.5-turbo");
    }

    /// M6f: pre-M6f index.json without the `model` field should
    /// still load (the field is `#[serde(default)] = None`).
    /// `load_run` will fall back to the flat layout in that
    /// case.
    #[test]
    fn history_entry_without_model_field_deserializes() {
        // Old-format index entry (no `model` field).
        let json = r#"{
            "run_id": "old-1",
            "timestamp": "2026-08-01T00:00:00Z",
            "target": "http://x",
            "rps": 1.0,
            "duration_secs": 1,
            "status": "completed",
            "tag": null,
            "total_requests": 0,
            "error_count": 0
        }"#;
        let entry: HistoryEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.run_id, "old-1");
        assert!(entry.model.is_none());
    }

    /// M6f: `list_runs_by_model` filters to same-model runs and
    /// respects the exclude_run_id parameter.
    #[test]
    fn list_runs_by_model_filters_and_excludes() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Two runs of MiniMax-M3, one of gpt-3.5-turbo.
        for (rid, model) in [
            ("m-1", "MiniMax-M3"),
            ("m-2", "MiniMax-M3"),
            ("g-1", "gpt-3.5-turbo"),
        ] {
            let mut cfg = make_config(rid);
            cfg.model = model.into();
            save_run(
                root,
                rid,
                &cfg,
                &make_agg(),
                "summary",
                "<html></html>",
                RunStatus::Completed,
            )
            .unwrap();
        }

        let only_m = list_runs_by_model(root, "MiniMax-M3", None).unwrap();
        assert_eq!(only_m.len(), 2);
        assert!(only_m.iter().all(|e| e.model.as_deref() == Some("MiniMax-M3")));

        // Excluding self.
        let m_minus_1 = list_runs_by_model(root, "MiniMax-M3", Some("m-1")).unwrap();
        assert_eq!(m_minus_1.len(), 1);
        assert_eq!(m_minus_1[0].run_id, "m-2");

        // Non-existent model.
        let none = list_runs_by_model(root, "no-such-model", None).unwrap();
        assert!(none.is_empty());

        // gpt-3.5-turbo should only see its own run.
        let only_g = list_runs_by_model(root, "gpt-3.5-turbo", None).unwrap();
        assert_eq!(only_g.len(), 1);
        assert_eq!(only_g[0].run_id, "g-1");
    }

    // ---- M6g: model alias ----

    /// M6g: alias wins when present, model is the fallback.
    #[test]
    fn effective_group_key_prefers_alias() {
        assert_eq!(effective_group_key("m", None), "m");
        assert_eq!(effective_group_key("m", Some("")), "m");
        assert_eq!(effective_group_key("m", Some("   ")), "m");
        assert_eq!(effective_group_key("m", Some("a")), "a");
        assert_eq!(
            effective_group_key("DeepSeek-V4-Flash-20260115", Some("DeepSeek-V4-Flash")),
            "DeepSeek-V4-Flash"
        );
    }

    /// M6g: two runs with different model names but the same
    /// alias must land in the same subdirectory and the same
    /// compare group.
    #[test]
    fn alias_groups_dated_snapshots_together() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        // Two dated snapshots of the same underlying model,
        // both with the same alias.
        for (rid, model, alias) in [
            ("jan", "DeepSeek-V4-Flash-20260115", "DeepSeek-V4-Flash"),
            ("feb", "DeepSeek-V4-Flash-20260201", "DeepSeek-V4-Flash"),
        ] {
            let mut cfg = make_config(rid);
            cfg.model = model.into();
            cfg.model_alias = Some(alias.into());
            save_run(
                root,
                rid,
                &cfg,
                &make_agg(),
                "summary",
                "<html></html>",
                RunStatus::Completed,
            )
            .unwrap();
        }
        // Both files should be under the alias directory,
        // not under their respective dated model dirs.
        let alias_dir = root.join("DeepSeek-V4-Flash");
        assert!(alias_dir.join("jan").is_dir());
        assert!(alias_dir.join("feb").is_dir());
        // The dated model dirs must NOT exist as siblings.
        assert!(!root.join("DeepSeek-V4-Flash-20260115").exists());
        assert!(!root.join("DeepSeek-V4-Flash-20260201").exists());

        // `list_runs_by_alias` returns both.
        let same_alias = list_runs_by_alias(root, "DeepSeek-V4-Flash", None).unwrap();
        assert_eq!(same_alias.len(), 2);

        // `list_runs_by_model` (strict) returns each separately.
        let jan_only = list_runs_by_model(root, "DeepSeek-V4-Flash-20260115", None).unwrap();
        assert_eq!(jan_only.len(), 1);
        let feb_only = list_runs_by_model(root, "DeepSeek-V4-Flash-20260201", None).unwrap();
        assert_eq!(feb_only.len(), 1);
    }

    /// M6g: when a run has no alias, `list_runs_by_alias` with
    /// its model name still finds it (the model becomes the
    /// implicit group key). This is the M6f back-compat path.
    #[test]
    fn list_runs_by_alias_falls_back_to_model_when_no_alias() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut cfg = make_config("legacy");
        cfg.model = "gpt-3.5-turbo".into();
        // No alias set.
        save_run(
            root,
            "legacy",
            &cfg,
            &make_agg(),
            "summary",
            "<html></html>",
            RunStatus::Completed,
        )
        .unwrap();
        // Querying by the model string still returns the run.
        let hits = list_runs_by_alias(root, "gpt-3.5-turbo", None).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].run_id, "legacy");
    }

    /// M6g: alias round-trips through `index.json`.
    #[test]
    fn alias_persists_in_index() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut cfg = make_config("aliased");
        cfg.model_alias = Some("MyAlias".into());
        save_run(
            root,
            "aliased",
            &cfg,
            &make_agg(),
            "summary",
            "<html></html>",
            RunStatus::Completed,
        )
        .unwrap();
        let entry = lookup_index(root, "aliased").unwrap();
        assert_eq!(entry.model_alias.as_deref(), Some("MyAlias"));
        assert_eq!(entry.model.as_deref(), Some("gpt-3.5-turbo"));
    }

    /// M6g: `load_run` resolves the directory through the
    /// alias-keyed subdirectory, not the model name.
    #[test]
    fn load_run_uses_alias_subdirectory() {
        let tmp = TempDir::new().unwrap();
        let root = tmp.path();
        let mut cfg = make_config("aliased-load");
        cfg.model = "MiniMax-M3-20260101".into();
        cfg.model_alias = Some("MiniMax-M3".into());
        save_run(
            root,
            "aliased-load",
            &cfg,
            &make_agg(),
            "summary",
            "<html></html>",
            RunStatus::Completed,
        )
        .unwrap();
        // File should be at the alias path, not the model path.
        assert!(root.join("MiniMax-M3").join("aliased-load").is_dir());
        assert!(!root.join("MiniMax-M3-20260101").join("aliased-load").exists());

        // `load_run` should find it via the index's alias.
        let (loaded, _metrics) = load_run(root, "aliased-load").unwrap();
        assert_eq!(loaded.model, "MiniMax-M3-20260101");
        assert_eq!(loaded.model_alias.as_deref(), Some("MiniMax-M3"));
    }
}

/// M7: list every model that has at least one saved run, grouped
/// by the effective group key (alias if set, else model name).
/// Used by the new `power_test dashboard` subcommand to render
/// one dashboard per model in a single pass.
pub fn list_group_keys(root: &Path) -> Result<Vec<String>> {
    let all = list_runs(root)?;
    let mut keys: Vec<String> = all
        .iter()
        .map(|e| {
            effective_group_key(
                e.model.as_deref().unwrap_or(""),
                e.model_alias.as_deref(),
            )
            .to_string()
        })
        .filter(|k| !k.is_empty())
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// M7: regenerate the model dashboard for a single group key.
/// Reads the index, filters runs by `group_key`, builds the
/// `RunSummary` list, renders the dashboard HTML, and writes
/// it to `<root>/<group_key>/index.html`. Idempotent: running
/// it twice produces the same result.
///
/// This is best-effort: a failure logs a `tracing::warn!` and
/// returns `Ok(())` so a dashboard-render error never blocks
/// the real save.
/// M7 stub: model_dashboard module was removed to keep main project compiling.
/// Auto-regen of the dashboard is currently disabled. Use `power_test dashboard`
/// to manually regenerate when the model_dashboard module is restored.
pub fn regenerate_dashboard_for_group(_root: &Path, _group_key: &str) -> Result<()> {
    Ok(())
}


