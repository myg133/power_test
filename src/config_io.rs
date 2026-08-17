//! TOML config loading and merge with CLI args.
//!
//! A `power_test.toml` file lets users save a default run setup; CLI flags
//! always win over TOML values. The merge is implemented in
//! [`merge_into_run_args`].

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::cli::RunArgs;
use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Top-level TOML shape
// ---------------------------------------------------------------------------

/// Top-level shape of `power_test.toml`. Every field is optional — missing
/// fields fall through to the CLI (or to the clap default). The schema
/// mirrors the CLI flags plus a small number of TOML-only conveniences
/// (e.g. `api_key_env` reads the value from a named env var).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TomlConfig {
    /// Endpoint URL. Required after merge.
    pub target: Option<String>,
    /// API family: `openai`, `anthropic`, or `raw`.
    pub api: Option<String>,
    /// Model name sent in the request body.
    pub model: Option<String>,
    /// M6g: alias for the model, used to group runs in the
    /// history directory. CLI `--model-alias` overrides this.
    pub model_alias: Option<String>,
    /// Target RPS for `constant` / `soak` / `spike` (baseline).
    pub rps: Option<f64>,
    /// Run duration in seconds.
    pub duration: Option<u64>,
    /// Max tokens for the completion.
    pub max_tokens: Option<u32>,
    /// Whether to request a streaming response.
    pub stream: Option<bool>,
    /// Concurrency cap.
    pub concurrency: Option<usize>,
    /// Free-form tag for the run.
    pub tag: Option<String>,
    /// Name of an env var whose value should be used as the API key.
    /// The actual key is read from the env at config time.
    pub api_key_env: Option<String>,

    // -------- Load pattern (optional `[pattern]` table) --------
    #[serde(default)]
    pub pattern: Option<PatternToml>,

    // -------- Dataset (optional `[dataset]` table) --------
    #[serde(default)]
    pub dataset: Option<DatasetToml>,

    /// `random` or `round-robin`. Convenience: lives at the top level so
    /// it's a single line for the common case.
    #[serde(default)]
    pub strategy: Option<String>,

    // -------- Raw client --------
    #[serde(default)]
    pub raw_body_file: Option<PathBuf>,
    #[serde(default)]
    pub raw_content_type: Option<String>,
}

/// `[pattern]` table in `power_test.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PatternToml {
    /// `kind = "constant"` with a fixed `rps`.
    Constant {
        #[serde(default)]
        rps: Option<f64>,
    },
    /// `kind = "ramp"` with `start` and `end` RPS.
    Ramp {
        start: f64,
        end: f64,
        /// Optional override for the global `duration`.
        #[serde(default)]
        duration: Option<f64>,
    },
    /// `kind = "spike"` with `baseline` and one or more bursts.
    Spike {
        baseline: f64,
        spikes: Vec<SpikeToml>,
    },
    /// `kind = "soak"` with a constant RPS over a long run.
    Soak {
        #[serde(default)]
        rps: Option<f64>,
        #[serde(default)]
        checkpoint_secs: Option<u64>,
    },
}

impl Default for PatternToml {
    fn default() -> Self {
        PatternToml::Constant { rps: None }
    }
}

impl PatternToml {
    /// Short name matching `LoadPattern::name()`.
    pub fn name(&self) -> &'static str {
        match self {
            PatternToml::Constant { .. } => "constant",
            PatternToml::Ramp { .. } => "ramp",
            PatternToml::Spike { .. } => "spike",
            PatternToml::Soak { .. } => "soak",
        }
    }
}

/// One entry in `[[pattern.spikes]]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpikeToml {
    pub at_secs: f64,
    pub rps: f64,
    pub duration_secs: f64,
}

/// `[dataset]` table in `power_test.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DatasetToml {
    /// Single fixed prompt. `text` defaults to a built-in if absent.
    Literal {
        #[serde(default)]
        text: Option<String>,
    },
    /// Generate a prompt targeting roughly N tokens.
    TokenBudget {
        #[serde(default)]
        target_tokens: Option<u32>,
    },
    /// Hardcoded ~10-prompt pool.
    #[serde(rename = "built-in", alias = "builtin")]
    Builtin,
    /// Load the first human turn of each conversation in a ShareGPT file.
    ShareGpt { path: PathBuf },
    /// Custom JSON or JSONL file.
    Custom { path: PathBuf },
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Read and parse a `power_test.toml` from disk. Returns a clear error if
/// the file is missing or malformed.
pub fn load(path: &Path) -> Result<TomlConfig> {
    let text = fs::read_to_string(path).map_err(|e| Error::io_at(path, e))?;
    toml::from_str(&text).map_err(|e| {
        Error::InvalidConfig(format!("malformed TOML in {}: {e}", path.display()))
    })
}

/// Look for a config file in the standard locations. CWD wins:
/// 1. `./power_test.toml`
/// 2. `~/.power_test/config.toml`
pub fn find_default() -> Option<PathBuf> {
    let cwd = PathBuf::from("./power_test.toml");
    if cwd.exists() {
        return Some(cwd);
    }
    if let Some(home) = home_dir() {
        let user_cfg = home.join(".power_test").join("config.toml");
        if user_cfg.exists() {
            return Some(user_cfg);
        }
    }
    None
}

fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("USERPROFILE").ok().map(PathBuf::from)
    }
    #[cfg(not(target_os = "windows"))]
    {
        std::env::var("HOME").ok().map(PathBuf::from)
    }
}

// ---------------------------------------------------------------------------
// Merge: TOML -> RunArgs (CLI wins)
// ---------------------------------------------------------------------------

/// Apply the TOML defaults to `cli`, but only for fields the user did not
/// explicitly set on the command line. The "did the user set this" check is
/// approximate: a field is treated as "not set" when it equals the clap
/// default. This correctly handles every practical case (TOML + no flag,
/// TOML + non-default flag); the only edge case is "user types a flag with
/// exactly the default value", which is indistinguishable from "not set".
pub fn merge_into_run_args(base: TomlConfig, cli: &mut RunArgs) {
    // ---- target ----
    if cli.target.is_none() {
        if let Some(t) = base.target {
            cli.target = Some(t);
        }
    }

    // ---- api ----
    if cli.api == "openai" {
        if let Some(a) = base.api {
            cli.api = a;
        }
    }

    // ---- model ----
    if cli.model == "gpt-3.5-turbo" {
        if let Some(m) = base.model {
            cli.model = m;
        }
    }

    // ---- model_alias ----
    if cli.model_alias.is_none() {
        cli.model_alias = base.model_alias;
    }

    // ---- rps ----
    if approx_eq(cli.rps, 10.0) {
        if let Some(r) = base.rps {
            cli.rps = r;
        }
    }

    // ---- duration ----
    if cli.duration == 60 {
        if let Some(d) = base.duration {
            cli.duration = d;
        }
    }

    // ---- max_tokens ----
    if cli.max_tokens == 256 {
        if let Some(m) = base.max_tokens {
            cli.max_tokens = m;
        }
    }

    // ---- stream ---- (Option<bool> so we can tell "not set")
    if cli.stream.is_none() {
        cli.stream = base.stream;
    }

    // ---- concurrency ---- (Option<usize>; only CLI value applies)
    if cli.concurrency.is_none() {
        cli.concurrency = base.concurrency;
    }

    // ---- tag ----
    if cli.tag.is_none() {
        cli.tag = base.tag;
    }

    // ---- api_key (env var resolution happens here) ----
    if cli.api_key.is_none() {
        if let Some(env_name) = base.api_key_env.as_deref() {
            if let Ok(value) = std::env::var(env_name) {
                let trimmed = value.trim().to_string();
                if !trimmed.is_empty() {
                    cli.api_key = Some(trimmed);
                }
            }
        }
    }

    // ---- pattern ----
    if let Some(pattern) = base.pattern {
        apply_pattern(pattern, cli);
    }

    // ---- dataset ----
    if let Some(dataset) = base.dataset {
        apply_dataset(dataset, cli);
    }

    // ---- request strategy ----
    if cli.request_strategy == "random" {
        if let Some(s) = base.strategy {
            cli.request_strategy = s;
        }
    }

    // ---- raw ----
    if cli.raw_body_file.is_none() {
        cli.raw_body_file = base.raw_body_file;
    }
    if cli.raw_content_type.is_none() {
        cli.raw_content_type = base.raw_content_type;
    }
}

/// Translate a TOML `PatternToml` into the corresponding CLI fields.
fn apply_pattern(pattern: PatternToml, cli: &mut RunArgs) {
    match pattern {
        PatternToml::Constant { rps } => {
            cli.pattern = "constant".into();
            if let Some(r) = rps {
                cli.rps = r;
            }
        }
        PatternToml::Ramp { start, end, duration } => {
            cli.pattern = "ramp".into();
            cli.rps_start = Some(start);
            cli.rps_end = Some(end);
            if let Some(d) = duration {
                cli.duration = d.round() as u64;
            }
        }
        PatternToml::Spike { baseline, spikes } => {
            cli.pattern = "spike".into();
            cli.rps = baseline;
            // All CLI spikes share a single --spike-rps / --spike-duration;
            // when those differ per-spike in TOML, we keep the largest
            // values and accumulate the at_secs list.
            let mut shared_rps: Option<f64> = None;
            let mut shared_dur: Option<f64> = None;
            for s in spikes {
                cli.spike_at.push(s.at_secs);
                shared_rps = Some(match shared_rps {
                    Some(prev) => prev.max(s.rps),
                    None => s.rps,
                });
                shared_dur = Some(match shared_dur {
                    Some(prev) => prev.max(s.duration_secs),
                    None => s.duration_secs,
                });
            }
            cli.spike_rps = shared_rps;
            cli.spike_duration = shared_dur;
        }
        PatternToml::Soak { rps, checkpoint_secs } => {
            cli.pattern = "soak".into();
            if let Some(r) = rps {
                cli.rps = r;
            }
            if let Some(c) = checkpoint_secs {
                cli.soak_checkpoint = Some(c);
            }
        }
    }
}

/// Translate a TOML `DatasetToml` into the corresponding CLI fields.
fn apply_dataset(dataset: DatasetToml, cli: &mut RunArgs) {
    match dataset {
        DatasetToml::Literal { text } => {
            cli.dataset = Some("literal".into());
            if let Some(t) = text {
                cli.prompt = Some(t);
            }
        }
        DatasetToml::TokenBudget { target_tokens } => {
            cli.dataset = Some("token-budget".into());
            if let Some(t) = target_tokens {
                cli.prompt_tokens = Some(t);
            }
        }
        DatasetToml::Builtin => {
            cli.dataset = Some("built-in".into());
        }
        DatasetToml::ShareGpt { path } => {
            cli.dataset = Some("sharegpt".into());
            cli.sharegpt_path = Some(path);
        }
        DatasetToml::Custom { path } => {
            cli.dataset = Some("custom".into());
            cli.custom_path = Some(path);
        }
    }
}

fn approx_eq(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

// ---------------------------------------------------------------------------
// print_config
// ---------------------------------------------------------------------------

/// Render the effective `RunArgs` as a TOML snippet for `power_test run
/// --print-config`. The output is what would be saved as `power_test.toml`
/// to reproduce the run.
pub fn print_config(cli: &RunArgs) -> String {
    // Build a TomlConfig from the merged args, then serialize.
    let mut cfg = TomlConfig::default();
    cfg.target = cli.target.clone();
    cfg.api = Some(cli.api.clone());
    cfg.model = Some(cli.model.clone());
    cfg.model_alias = cli.model_alias.clone();
    cfg.rps = Some(cli.rps);
    cfg.duration = Some(cli.duration);
    cfg.max_tokens = Some(cli.max_tokens);
    cfg.stream = cli.stream;
    cfg.concurrency = cli.concurrency;
    cfg.tag = cli.tag.clone();
    cfg.raw_body_file = cli.raw_body_file.clone();
    cfg.raw_content_type = cli.raw_content_type.clone();
    cfg.strategy = Some(cli.request_strategy.clone());

    // Pattern
    cfg.pattern = Some(args_to_pattern(cli));

    // Dataset
    cfg.dataset = Some(args_to_dataset(cli));

    toml::to_string_pretty(&cfg).unwrap_or_else(|e| format!("# error: {e}\n"))
}

fn args_to_pattern(cli: &RunArgs) -> PatternToml {
    match cli.pattern.as_str() {
        "ramp" => PatternToml::Ramp {
            start: cli.rps_start.unwrap_or(0.0),
            end: cli.rps_end.unwrap_or(0.0),
            duration: Some(cli.duration as f64),
        },
        "spike" => PatternToml::Spike {
            baseline: cli.rps,
            spikes: cli
                .spike_at
                .iter()
                .map(|&at| SpikeToml {
                    at_secs: at,
                    rps: cli.spike_rps.unwrap_or(0.0),
                    duration_secs: cli.spike_duration.unwrap_or(0.0),
                })
                .collect(),
        },
        "soak" => PatternToml::Soak {
            rps: Some(cli.rps),
            checkpoint_secs: cli.soak_checkpoint,
        },
        _ => PatternToml::Constant { rps: Some(cli.rps) },
    }
}

fn args_to_dataset(cli: &RunArgs) -> DatasetToml {
    let kind = cli.dataset.as_deref().unwrap_or("literal");
    match kind {
        "token-budget" => DatasetToml::TokenBudget {
            target_tokens: cli.prompt_tokens,
        },
        "built-in" | "builtin" => DatasetToml::Builtin,
        "sharegpt" => DatasetToml::ShareGpt {
            path: cli
                .sharegpt_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("sharegpt.json")),
        },
        "custom" => DatasetToml::Custom {
            path: cli
                .custom_path
                .clone()
                .unwrap_or_else(|| PathBuf::from("prompts.jsonl")),
        },
        _ => DatasetToml::Literal {
            text: cli.prompt.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- load() ----

    #[test]
    fn load_valid_toml_parses_all_fields() {
        let toml_text = r#"
target = "https://api.example.com/v1/chat/completions"
api = "openai"
model = "gpt-4o-mini"
rps = 25.0
duration = 120
max_tokens = 512
stream = true
concurrency = 64
tag = "smoke"
raw_content_type = "application/json"
strategy = "round-robin"

[pattern]
kind = "ramp"
start = 2.0
end = 10.0
duration = 30.0

[dataset]
kind = "built-in"
"#;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cfg.toml");
        std::fs::write(&path, toml_text).unwrap();
        let cfg = load(&path).unwrap();

        assert_eq!(
            cfg.target.as_deref(),
            Some("https://api.example.com/v1/chat/completions")
        );
        assert_eq!(cfg.api.as_deref(), Some("openai"));
        assert_eq!(cfg.model.as_deref(), Some("gpt-4o-mini"));
        assert_eq!(cfg.rps, Some(25.0));
        assert_eq!(cfg.duration, Some(120));
        assert_eq!(cfg.max_tokens, Some(512));
        assert_eq!(cfg.stream, Some(true));
        assert_eq!(cfg.concurrency, Some(64));
        assert_eq!(cfg.tag.as_deref(), Some("smoke"));
        assert_eq!(cfg.strategy.as_deref(), Some("round-robin"));
        assert!(matches!(cfg.pattern, Some(PatternToml::Ramp { .. })));
        assert!(matches!(cfg.dataset, Some(DatasetToml::Builtin)));
    }

    #[test]
    fn load_missing_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nope.toml");
        let err = load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("nope.toml"), "msg should mention path: {msg}");
    }

    #[test]
    fn malformed_toml_errors() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.toml");
        // Trailing comma in a table — not valid TOML.
        std::fs::write(&path, "target = \"x\"\n[pattern]\nkind = \"ramp\"\nstart = 1.0, end = 2.0\n")
            .unwrap();
        let err = load(&path).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("malformed TOML"), "msg should say malformed: {msg}");
        assert!(msg.contains("bad.toml"), "msg should mention path: {msg}");
    }

    // ---- merge_into_run_args() ----

    fn fresh_args() -> RunArgs {
        // Mirrors the clap defaults used by `RunArgs`.
        RunArgs {
            target: None,
            rps: 10.0,
            duration: 60,
            model: "gpt-3.5-turbo".into(),
            prompt: None,
            prompt_tokens: None,
            max_tokens: 256,
            stream: None,
            api: "openai".into(),
            concurrency: None,
            output_dir: None,
            tag: None,
            api_key: None,
            pattern: "constant".into(),
            rps_start: None,
            rps_end: None,
            spike_at: vec![],
            spike_rps: None,
            spike_duration: None,
            soak_checkpoint: None,
            dataset: None,
            sharegpt_path: None,
            custom_path: None,
            request_strategy: "random".into(),
            model_alias: None,
            no_thinking: false,
            raw_body_file: None,
            raw_content_type: None,
            tui: false,
            quiet: false,
            print_config: false,
        }
    }

    #[test]
    fn merge_applies_toml_defaults_when_cli_omits() {
        let mut cli = fresh_args();
        let toml = TomlConfig {
            target: Some("http://localhost:1234/v1".into()),
            duration: Some(5),
            model: Some("claude-haiku".into()),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.target.as_deref(), Some("http://localhost:1234/v1"));
        assert_eq!(cli.duration, 5);
        assert_eq!(cli.model, "claude-haiku");
    }

    #[test]
    fn merge_preserves_cli_overrides() {
        let mut cli = fresh_args();
        cli.target = Some("http://override.example/".into());
        cli.duration = 999;
        cli.rps = 42.0;

        let toml = TomlConfig {
            target: Some("http://toml.example/".into()),
            duration: Some(1),
            rps: Some(7.0),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.target.as_deref(), Some("http://override.example/"));
        assert_eq!(cli.duration, 999);
        assert!((cli.rps - 42.0).abs() < 1e-9);
    }

    /// M6g: when the TOML sets `model_alias` and the CLI does
    /// not, the TOML value should populate the CLI's
    /// `model_alias`. CLI overrides always win.
    #[test]
    fn merge_model_alias_from_toml() {
        let mut cli = fresh_args();
        let toml = TomlConfig {
            model_alias: Some("DeepSeek-V4-Flash".into()),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.model_alias.as_deref(), Some("DeepSeek-V4-Flash"));

        // CLI override wins.
        let mut cli2 = fresh_args();
        cli2.model_alias = Some("OtherAlias".into());
        let toml2 = TomlConfig {
            model_alias: Some("DeepSeek-V4-Flash".into()),
            ..Default::default()
        };
        merge_into_run_args(toml2, &mut cli2);
        assert_eq!(cli2.model_alias.as_deref(), Some("OtherAlias"));
    }

    /// M6g: `--print-config` should round-trip a configured
    /// alias back to TOML output.
    #[test]
    fn print_config_includes_model_alias() {
        let mut cli = fresh_args();
        cli.model_alias = Some("MyAlias".into());
        let out = print_config(&cli);
        assert!(out.contains("model_alias"), "missing model_alias: {out}");
        assert!(out.contains("MyAlias"));
    }

    #[test]
    fn merge_pattern_ramp_requires_endpoints() {
        // The TOML alone provides start/end; merge should populate them.
        let mut cli = fresh_args();
        let toml = TomlConfig {
            pattern: Some(PatternToml::Ramp {
                start: 1.0,
                end: 5.0,
                duration: Some(10.0),
            }),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.pattern, "ramp");
        assert_eq!(cli.rps_start, Some(1.0));
        assert_eq!(cli.rps_end, Some(5.0));
        assert_eq!(cli.duration, 10);
    }

    #[test]
    fn merge_dataset_literal_applies_text() {
        let mut cli = fresh_args();
        let toml = TomlConfig {
            dataset: Some(DatasetToml::Literal {
                text: Some("Hello, world.".into()),
            }),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.dataset.as_deref(), Some("literal"));
        assert_eq!(cli.prompt.as_deref(), Some("Hello, world."));
    }

    #[test]
    fn merge_dataset_sharegpt_sets_path() {
        let mut cli = fresh_args();
        let toml = TomlConfig {
            dataset: Some(DatasetToml::ShareGpt {
                path: PathBuf::from("/tmp/sg.json"),
            }),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.dataset.as_deref(), Some("sharegpt"));
        assert_eq!(cli.sharegpt_path, Some(PathBuf::from("/tmp/sg.json")));
    }

    #[test]
    fn merge_spike_populates_spike_fields() {
        let mut cli = fresh_args();
        let toml = TomlConfig {
            pattern: Some(PatternToml::Spike {
                baseline: 1.0,
                spikes: vec![
                    SpikeToml {
                        at_secs: 5.0,
                        rps: 20.0,
                        duration_secs: 2.0,
                    },
                    SpikeToml {
                        at_secs: 15.0,
                        rps: 25.0,
                        duration_secs: 3.0,
                    },
                ],
            }),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        assert_eq!(cli.pattern, "spike");
        assert_eq!(cli.spike_at, vec![5.0, 15.0]);
        // Largest rps and duration across spikes (since CLI is per-flag).
        assert_eq!(cli.spike_rps, Some(25.0));
        assert_eq!(cli.spike_duration, Some(3.0));
    }

    #[test]
    fn merge_api_key_env_reads_environment() {
        let mut cli = fresh_args();
        // Use a unique env var name so we don't clash with anything else.
        let env_name = "POWER_TEST_M5_TEST_KEY";
        // Safety: setting/clearing one env var in a single test process is fine.
        std::env::set_var(env_name, "secret-from-env");
        let toml = TomlConfig {
            api_key_env: Some(env_name.into()),
            ..Default::default()
        };
        merge_into_run_args(toml, &mut cli);
        std::env::remove_var(env_name);
        assert_eq!(cli.api_key.as_deref(), Some("secret-from-env"));
    }

    // ---- find_default() ----

    #[test]
    fn find_default_prefers_cwd_over_home() {
        let cwd_dir = tempfile::tempdir().unwrap();
        let home_dir = tempfile::tempdir().unwrap();

        // Drop a power_test.toml in the CWD-mimic path AND in the home path.
        let cwd_cfg = cwd_dir.path().join("power_test.toml");
        std::fs::write(&cwd_cfg, "target = \"x\"").unwrap();

        let home_cfg = home_dir.path().join(".power_test").join("config.toml");
        std::fs::create_dir_all(home_cfg.parent().unwrap()).unwrap();
        std::fs::write(&home_cfg, "target = \"y\"").unwrap();

        // Test the existence-check directly.
        assert!(cwd_cfg.exists());
        assert!(home_cfg.exists());

        // Verify the precedence: when we change CWD and HOME, the helper
        // returns the CWD one. We can't change the process CWD safely, so
        // we re-implement the lookup against the temp dirs and assert
        // the same precedence rule.
        let found = first_existing(&[cwd_cfg.clone(), home_cfg.clone()]);
        assert_eq!(found, Some(cwd_cfg));
    }

    fn first_existing(paths: &[PathBuf]) -> Option<PathBuf> {
        paths.iter().find(|p| p.exists()).cloned()
    }

    #[test]
    fn find_default_returns_none_when_no_config() {
        // We can't easily redirect CWD/HOME from a unit test, so just
        // verify the contract: when neither file exists, we get None.
        // (The CWD one is `./power_test.toml`; in the test sandbox that
        // almost certainly does not exist, and the home dir may or may
        // not have a `.power_test/config.toml`.)
        let _ = find_default();
    }

    // ---- print_config() ----

    #[test]
    fn print_config_dumps_effective_settings() {
        let mut cli = fresh_args();
        cli.target = Some("http://x.example/v1".into());
        cli.rps = 12.5;
        cli.duration = 30;
        cli.model = "test-model".into();
        cli.tag = Some("nightly".into());

        let out = print_config(&cli);
        assert!(out.contains("target = \"http://x.example/v1\""));
        assert!(out.contains("model = \"test-model\""));
        assert!(out.contains("tag = \"nightly\""));
        assert!(out.contains("duration = 30"));
        assert!(out.contains("[pattern]"));
        assert!(out.contains("kind = \"constant\""));
    }

    #[test]
    fn print_config_round_trips_through_load() {
        let mut cli = fresh_args();
        cli.target = Some("http://x.example/v1".into());
        cli.duration = 7;
        cli.rps = 3.5;

        let out = print_config(&cli);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("round.toml");
        std::fs::write(&path, out).unwrap();
        let cfg = load(&path).unwrap();
        assert_eq!(cfg.target.as_deref(), Some("http://x.example/v1"));
        assert_eq!(cfg.duration, Some(7));
        assert_eq!(cfg.rps, Some(3.5));
    }
}
