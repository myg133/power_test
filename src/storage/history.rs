//! History directory layout: `<root>/<run-id>/{config.json, metrics.json,
//! report.html, summary.txt}`. An index file at `<root>/index.json` lists
//! runs for fast `list` queries.

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

/// Save a run's artifacts to `<root>/<run_id>/`. Writes `config.json`,
/// `metrics.json`, `report.html`, and `summary.txt` in that order.
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
    let dir = root.join(run_id);
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
    // Fallback: scan subdirs.
    let mut entries = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            let dir = entry.path();
            let run_id = match dir.file_name().and_then(|s| s.to_str()) {
                Some(s) => s.to_string(),
                None => continue,
            };
            if let Ok(cfg) = load_config(&dir) {
                entries.push(HistoryEntry {
                    run_id,
                    timestamp: cfg.started_at,
                    target: cfg.target,
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
pub fn load_run(root: &Path, run_id: &str) -> Result<(RunConfig, serde_json::Value)> {
    let dir = root.join(run_id);
    if !dir.is_dir() {
        return Err(crate::error::Error::RunNotFound(run_id.to_string()));
    }
    let config = load_config(&dir)?;
    let metrics_text = fs::read_to_string(dir.join("metrics.json"))
        .map_err(|e| crate::error::Error::io_at(dir.join("metrics.json"), e))?;
    let metrics: serde_json::Value = serde_json::from_str(&metrics_text)?;
    Ok((config, metrics))
}

/// List runs in the history root that share the same `target` URL as the
/// given one. Used by the compare UI to populate the "compare with…"
/// dropdown. Newest first, excluding the run itself when `exclude_run_id`
/// is `Some`.
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

        let dir = root.join("run-abc");
        assert!(dir.join("config.json").exists());
        assert!(dir.join("metrics.json").exists());
        assert!(dir.join("report.html").exists());
        assert!(dir.join("summary.txt").exists());

        // Round-trip config
        let cfg2 = load_config(&dir).unwrap();
        assert_eq!(cfg2.run_id, cfg.run_id);
        assert_eq!(cfg2.target_rps, 5.0);

        // Round-trip run
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
}
