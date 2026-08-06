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

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::config::{RunConfig, RunStatus};
use crate::error::{Error, Result};
use crate::runner::{MetricsAggregator, RequestRecord};

/// A summary row for `power_test list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub run_id: String,
    pub timestamp: DateTime<Utc>,
    pub target: String,
    /// M6f: model name. `Option` so old `index.json` files
    /// (pre-M6f) still deserialize; the missing field is treated
    /// as "no model info" and `load_run` falls back to scanning
    /// the flat layout for those entries.
    #[serde(default)]
    pub model: Option<String>,
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

/// Compute the on-disk path for a run: `<root>/<model>/<run_id>/`.
/// Pure path math — no I/O.
pub fn run_dir(root: &Path, model: &str, run_id: &str) -> PathBuf {
    root.join(sanitize_model_dir(model)).join(run_id)
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
    let dir = run_dir(root, &config.model, run_id);
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
/// M6f: with the model-grouped layout
/// (`<root>/<model>/<run_id>/`), we look up the run's `model` in
/// the global `index.json` first, then read from the
/// model-keyed subdirectory. As a fallback for pre-M6f runs (flat
/// `<root>/<run_id>/`), we also try the old path with a tracing
/// warning. The flat fallback is the only reason we don't error
/// on missing index.json — we need to keep the old runs readable.
pub fn load_run(root: &Path, run_id: &str) -> Result<(RunConfig, serde_json::Value)> {
    // Preferred path: use the index to find the run's model.
    if let Some(entry) = lookup_index(root, run_id) {
        if let Some(model) = &entry.model {
            let dir = run_dir(root, model, run_id);
            if dir.is_dir() {
                return load_run_from(&dir);
            }
            // Index says model=X but the directory doesn't exist
            // — could be a corrupt run or a pre-migration state.
            // Fall through to the flat-layout fallback below.
            tracing::warn!(
                "index.json lists run_id={run_id} under model={model:?} but {} doesn't exist; trying flat layout",
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
/// Returns an empty list if the index has no entries with the
/// given model (e.g. for a model that has been removed from
/// the config since the run was saved).
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
            started_at: Utc::now(),
            raw_body_file: None,
            raw_content_type: None,
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
}
