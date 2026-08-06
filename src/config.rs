//! Run configuration: snapshot of every CLI flag that affects a test run.
//!
//! `RunConfig` is what gets serialized to `config.json` in the run's history
//! directory, so it must be self-contained and stable across versions.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Which HTTP API style to speak. M1 only implements [`ApiKind::Openai`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApiKind {
    Openai,
    Anthropic,
    Raw,
}

impl ApiKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ApiKind::Openai => "openai",
            ApiKind::Anthropic => "anthropic",
            ApiKind::Raw => "raw",
        }
    }
}

// ---------------------------------------------------------------------------
// M2: load patterns, datasets, and request strategy.
// ---------------------------------------------------------------------------

/// How the test schedules requests over time. M1 only had constant RPS
/// (modelled here as [`LoadPattern::Constant`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LoadPattern {
    /// Hold RPS at a fixed value for the entire run.
    Constant { rps: f64 },
    /// Linearly interpolate RPS from `start` to `end` over `duration_secs`.
    Ramp {
        start: f64,
        end: f64,
        duration_secs: f64,
    },
    /// Hold RPS at `baseline`, with one or more short bursts at `spikes`.
    Spike {
        baseline: f64,
        spikes: Vec<SpikeSpec>,
    },
    /// Constant RPS for long runs; the executor periodically flushes
    /// `metrics.json` to disk every `checkpoint_secs` so a mid-run crash
    /// leaves analyzable data behind.
    Soak {
        rps: f64,
        checkpoint_secs: u64,
    },
}

impl Default for LoadPattern {
    /// Backwards-compat default for older `config.json`. We pick a benign
    /// constant RPS=1 so the report renders sanely.
    fn default() -> Self {
        LoadPattern::Constant { rps: 1.0 }
    }
}

impl LoadPattern {
    /// The peak RPS this pattern will ever produce. Used for default
    /// concurrency sizing.
    pub fn peak_rps(&self) -> f64 {
        match self {
            LoadPattern::Constant { rps } => *rps,
            LoadPattern::Ramp { start, end, .. } => start.max(*end),
            LoadPattern::Spike { baseline, spikes } => spikes
                .iter()
                .map(|s| s.rps)
                .fold(*baseline, f64::max),
            LoadPattern::Soak { rps, .. } => *rps,
        }
    }

    /// A short, human-readable name for the index/HTML/summary.
    pub fn name(&self) -> &'static str {
        match self {
            LoadPattern::Constant { .. } => "constant",
            LoadPattern::Ramp { .. } => "ramp",
            LoadPattern::Spike { .. } => "spike",
            LoadPattern::Soak { .. } => "soak",
        }
    }
}

/// One burst within a [`LoadPattern::Spike`]. `at_secs` is measured from the
/// start of the run; `duration_secs` is how long the burst lasts.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SpikeSpec {
    pub at_secs: f64,
    pub rps: f64,
    pub duration_secs: f64,
}

/// The dataset the runner will pull prompts from. M1 had only literal and
/// token-budget; M2 adds built-in pool, ShareGPT, and custom JSON/JSONL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DatasetSpec {
    /// Always return the same literal text.
    Literal { text: String },
    /// Generate a prompt that roughly fills `target_tokens` tokens.
    TokenBudget { target_tokens: u32 },
    /// Hardcoded ~10-prompt pool of varying length (English + Chinese).
    Builtin,
    /// Load the first human turn of each conversation in a ShareGPT JSON file.
    /// Limited to the first 1000 prompts at load time.
    ShareGpt { path: PathBuf },
    /// Custom JSON array of `{"prompt": "..."}` or JSONL of the same shape.
    Custom { path: PathBuf },
}

impl DatasetSpec {
    /// Short, human-readable name.
    pub fn name(&self) -> &'static str {
        match self {
            DatasetSpec::Literal { .. } => "literal",
            DatasetSpec::TokenBudget { .. } => "token-budget",
            DatasetSpec::Builtin => "built-in",
            DatasetSpec::ShareGpt { .. } => "sharegpt",
            DatasetSpec::Custom { .. } => "custom",
        }
    }
}

impl Default for DatasetSpec {
    /// Backwards-compat default for loading older `config.json` files that
    /// predate the M2 dataset refactor. We use an empty literal so the
    /// diff / report still renders; the run itself is not replayed.
    fn default() -> Self {
        DatasetSpec::Literal { text: String::new() }
    }
}

/// How a multi-prompt dataset picks the next item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestStrategy {
    RoundRobin,
    Random,
}

impl Default for RequestStrategy {
    fn default() -> Self {
        RequestStrategy::Random
    }
}

impl RequestStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            RequestStrategy::RoundRobin => "round-robin",
            RequestStrategy::Random => "random",
        }
    }
}

/// Min / max / mean of `estimated_prompt_tokens` for the resolved dataset.
/// Computed at config time so the report can render it without re-reading
/// any source files.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PromptDistribution {
    pub count: usize,
    pub min: u32,
    pub max: u32,
    pub mean: f64,
}

impl Default for PromptDistribution {
    fn default() -> Self {
        Self {
            count: 0,
            min: 0,
            max: 0,
            mean: 0.0,
        }
    }
}

impl PromptDistribution {
    pub fn from_single(tokens: u32) -> Self {
        Self {
            count: 1,
            min: tokens,
            max: tokens,
            mean: tokens as f64,
        }
    }

    pub fn from_slice(tokens: &[u32]) -> Self {
        if tokens.is_empty() {
            return Self {
                count: 0,
                min: 0,
                max: 0,
                mean: 0.0,
            };
        }
        let mut min = u32::MAX;
        let mut max = 0u32;
        let mut sum: u64 = 0;
        for &t in tokens {
            if t < min {
                min = t;
            }
            if t > max {
                max = t;
            }
            sum += t as u64;
        }
        Self {
            count: tokens.len(),
            min,
            max,
            mean: sum as f64 / tokens.len() as f64,
        }
    }
}

// ---------------------------------------------------------------------------
// Legacy M1 prompt source, kept for backward-compatible config.json
// round-tripping. The runner no longer reads it — [`DatasetSpec`] is the
// authoritative source. [`PromptSource::resolve`] is reused internally for
// Literal / TokenBudget resolution.
// ---------------------------------------------------------------------------

/// How a single literal / token-budget prompt is constructed. Persists in
/// `config.json` so old reports stay loadable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromptSource {
    /// Use the user-supplied literal text.
    Literal { text: String },
    /// Generate a prompt targeting roughly N tokens.
    TokenBudget { target_tokens: u32 },
}

impl PromptSource {
    /// Resolve the source into an actual prompt string and an estimated
    /// token count. The token estimate is a rough heuristic (≈ 4 chars
    /// per token for English text) and is reported as such in the run
    /// output.
    pub fn resolve(&self) -> (String, u32) {
        match self {
            PromptSource::Literal { text } => {
                let tokens = estimate_tokens(text);
                (text.clone(), tokens)
            }
            PromptSource::TokenBudget { target_tokens } => {
                let text = generate_prompt_for_tokens(*target_tokens);
                let tokens = estimate_tokens(&text);
                (text, tokens)
            }
        }
    }
}

/// Heuristic English token estimate: roughly 4 characters per token.
pub fn estimate_tokens(text: &str) -> u32 {
    // Count on whitespace boundaries; fall back to char count for CJK-ish text.
    let whitespace_tokens = text.split_whitespace().count() as u32;
    if whitespace_tokens > 0 {
        // Average English word ≈ 1.3 tokens; this is a rough lower bound.
        return whitespace_tokens.max(text.len() as u32 / 4);
    }
    (text.chars().count() as u32 + 3) / 4
}

fn generate_prompt_for_tokens(target: u32) -> String {
    // 44 chars ≈ 11 tokens; repeat to fill.
    const CHUNK: &str = "the quick brown fox jumps over the lazy dog ";
    let chars = (target as usize).saturating_mul(4);
    let mut out = String::with_capacity(chars + CHUNK.len());
    while out.len() < chars {
        out.push_str(CHUNK);
    }
    out.truncate(chars);
    out
}

/// The full configuration for one test run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunConfig {
    pub run_id: String,
    pub target: String,
    pub api: ApiKind,
    pub model: String,
    /// Legacy prompt source, preserved so `config.json` from M1 loads
    /// unchanged. New runs derive this from `dataset` at config time.
    pub prompt: PromptSource,
    /// Active dataset spec (M2).
    #[serde(default)]
    pub dataset: DatasetSpec,
    /// How to pick items from a multi-prompt dataset (M2).
    #[serde(default)]
    pub strategy: RequestStrategy,
    /// Pre-computed prompt token distribution, for reporting.
    #[serde(default)]
    pub prompt_distribution: PromptDistribution,
    /// Active load pattern (M2).
    #[serde(default)]
    pub pattern: LoadPattern,
    /// Convenience copy of the peak RPS — the report uses this for
    /// "Target RPS". Set by `to_run_config`.
    #[serde(default)]
    pub target_rps: f64,
    pub max_tokens: u32,
    pub stream: bool,
    pub duration_secs: u64,
    pub concurrency: usize,
    pub tag: Option<String>,
    /// API key for the target. Sent as `Authorization: Bearer <key>` when set.
    /// Optional: some endpoints (e.g. local ollama) accept unauthenticated
    /// requests.
    pub api_key: Option<String>,
    pub started_at: chrono::DateTime<chrono::Utc>,

    // -------- M4: raw HTTP client --------
    /// Path to a static body file for `--api raw`. `None` means empty body.
    #[serde(default)]
    pub raw_body_file: Option<PathBuf>,
    /// Content-Type for the raw body. Defaults to `application/json`.
    #[serde(default)]
    pub raw_content_type: Option<String>,

    // -------- M6g: model alias --------
    /// M6g: an optional alias used to group runs in the history
    /// directory and the compare-with dropdown. When set, runs
    /// with different `--model` strings (e.g. dated snapshots
    /// like `DeepSeek-V4-Flash-20260115`) can share the same
    /// alias (`DeepSeek-V4-Flash`) and end up in the same
    /// `<root>/<alias>/` subdirectory. When `None`, the actual
    /// `model` is used as the group key. `#[serde(default)]`
    /// so old `config.json` files (pre-M6g) still load.
    #[serde(default)]
    pub model_alias: Option<String>,
}

impl RunConfig {
    /// Compute the default concurrency from a target RPS:
    /// `max(256, rps * 4)` capped at 1024.
    pub fn default_concurrency(rps: f64) -> usize {
        let v = (rps * 4.0).ceil() as usize;
        let v = v.max(256);
        v.min(1024)
    }
}

/// Resolve the default history directory: `%USERPROFILE%/.power_test/history`
/// on Windows, `~/.power_test/history` elsewhere.
pub fn default_history_dir() -> PathBuf {
    if cfg!(target_os = "windows") {
        if let Ok(profile) = std::env::var("USERPROFILE") {
            return PathBuf::from(profile).join(".power_test").join("history");
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home).join(".power_test").join("history");
    }
    PathBuf::from(".power_test").join("history")
}

/// A run's final outcome as recorded in the history index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStatus {
    Completed,
    Interrupted,
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Completed => "completed",
            RunStatus::Interrupted => "interrupted",
            RunStatus::Failed => "failed",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_concurrency_uses_floor_256() {
        assert_eq!(RunConfig::default_concurrency(1.0), 256);
        assert_eq!(RunConfig::default_concurrency(10.0), 256);
        assert_eq!(RunConfig::default_concurrency(100.0), 400);
        assert_eq!(RunConfig::default_concurrency(1000.0), 1024);
        assert_eq!(RunConfig::default_concurrency(100_000.0), 1024);
    }

    #[test]
    fn estimate_tokens_basic() {
        // 4 short words
        assert!(estimate_tokens("hello world foo bar") >= 4);
        // empty
        assert_eq!(estimate_tokens(""), 0);
    }

    #[test]
    fn generate_prompt_for_tokens_grows() {
        let p1 = generate_prompt_for_tokens(10);
        let p2 = generate_prompt_for_tokens(500);
        assert!(p2.len() > p1.len());
        assert!(estimate_tokens(&p2) >= 400);
    }

    #[test]
    fn prompt_source_round_trip() {
        let s = PromptSource::Literal {
            text: "hi".into(),
        };
        let json = serde_json::to_string(&s).unwrap();
        let back: PromptSource = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn load_pattern_peak_rps() {
        let p = LoadPattern::Constant { rps: 5.0 };
        assert!((p.peak_rps() - 5.0).abs() < 1e-9);

        let p = LoadPattern::Ramp {
            start: 2.0,
            end: 10.0,
            duration_secs: 30.0,
        };
        assert!((p.peak_rps() - 10.0).abs() < 1e-9);

        let p = LoadPattern::Spike {
            baseline: 3.0,
            spikes: vec![
                SpikeSpec {
                    at_secs: 5.0,
                    rps: 20.0,
                    duration_secs: 2.0,
                },
                SpikeSpec {
                    at_secs: 15.0,
                    rps: 8.0,
                    duration_secs: 1.0,
                },
            ],
        };
        assert!((p.peak_rps() - 20.0).abs() < 1e-9);
    }

    #[test]
    fn dataset_spec_name() {
        assert_eq!(
            DatasetSpec::Literal { text: "x".into() }.name(),
            "literal"
        );
        assert_eq!(
            DatasetSpec::TokenBudget { target_tokens: 10 }.name(),
            "token-budget"
        );
        assert_eq!(DatasetSpec::Builtin.name(), "built-in");
        assert_eq!(
            DatasetSpec::ShareGpt {
                path: PathBuf::from("/x")
            }
            .name(),
            "sharegpt"
        );
        assert_eq!(
            DatasetSpec::Custom {
                path: PathBuf::from("/x")
            }
            .name(),
            "custom"
        );
    }

    #[test]
    fn prompt_distribution_from_slice() {
        let d = PromptDistribution::from_slice(&[10, 20, 30]);
        assert_eq!(d.count, 3);
        assert_eq!(d.min, 10);
        assert_eq!(d.max, 30);
        assert!((d.mean - 20.0).abs() < 1e-9);

        let d = PromptDistribution::from_slice(&[]);
        assert_eq!(d.count, 0);
        assert_eq!(d.min, 0);
        assert_eq!(d.max, 0);

        let d = PromptDistribution::from_single(42);
        assert_eq!(d.count, 1);
        assert_eq!(d.min, 42);
        assert_eq!(d.max, 42);
    }

    #[test]
    fn run_config_round_trip() {
        let cfg = RunConfig {
            run_id: "abc".into(),
            target: "https://example.com/v1/chat/completions".into(),
            api: ApiKind::Openai,
            model: "gpt-3.5-turbo".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 5.0 },
            target_rps: 5.0,
            max_tokens: 128,
            stream: true,
            duration_secs: 30,
            concurrency: 256,
            tag: Some("smoke".into()),
            api_key: Some("sk-test".into()),
            started_at: chrono::Utc::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: RunConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.target_rps, 5.0);
        assert_eq!(back.api, ApiKind::Openai);
        assert_eq!(back.api_key.as_deref(), Some("sk-test"));
        assert_eq!(back.dataset, DatasetSpec::Literal { text: "hi".into() });
    }
}
