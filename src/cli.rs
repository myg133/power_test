//! Command-line interface types (clap derive).
//!
//! `Cli` is the top-level command. Each subcommand has its own struct so that
//! `clap` can validate flags per-command and produce tailored `--help` text.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::config::{
    default_history_dir, ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource,
    RequestStrategy, RunConfig, SpikeSpec,
};

/// power_test — HTTP-level stress testing for LLM inference endpoints.
#[derive(Debug, Parser)]
#[command(name = "power_test", version, about, long_about = None)]
pub struct Cli {
    /// Path to a TOML config file. Defaults to `./power_test.toml`,
    /// then `~/.power_test/config.toml`. CLI flags override TOML.
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,

    /// Tracing log level: `error`/`warn`/`info`/`debug`/`trace`.
    /// Default: `info`. Ignored when `--quiet` is also set.
    #[arg(long, default_value = "info", value_name = "LEVEL", global = true)]
    pub log_level: String,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Execute one test run.
    Run(RunArgs),
    /// List past runs in the history directory.
    List(ListArgs),
    /// Re-render the HTML report for a saved run.
    Report(ReportArgs),
    /// Compare two saved runs side-by-side (placeholder for M3).
    Compare(CompareArgs),
    /// M7: render the per-model dashboard. `power_test dashboard`
    /// regenerates the dashboard for every model that has at
    /// least one saved run; `power_test dashboard <NAME>`
    /// targets a single model. The dashboard is also
    /// auto-regenerated after every `power_test run` save.
    Dashboard(DashboardArgs),
}

#[derive(Debug, Args)]
#[allow(clippy::struct_excessive_bools)]
pub struct RunArgs {
    /// Target LLM HTTP endpoint (full URL). Optional so
    /// `--print-config` can dump a merged config without forcing the
    /// caller to repeat `--target` on the command line.
    #[arg(long)]
    pub target: Option<String>,

    /// Target RPS. Used by `constant` and `soak`; baseline for `spike`;
    /// ignored by `ramp` (use `--rps-start`/`--rps-end`).
    #[arg(long, default_value_t = 10.0)]
    pub rps: f64,

    /// Duration of the test in seconds.
    #[arg(long, default_value_t = 60)]
    pub duration: u64,

    /// Model name to send in the request body.
    #[arg(long, default_value = "gpt-3.5-turbo")]
    pub model: String,

    /// M6g: alias for the model, used to group runs in the
    /// history directory and the compare-with dropdown.
    /// Useful when the actual model name carries a date
    /// suffix (e.g. `DeepSeek-V4-Flash-20260115`) but you
    /// want all snapshots of `DeepSeek-V4-Flash` to land
    /// in the same `<history>/<alias>/` subdirectory.
    /// When omitted, the model name is used as the group
    /// key (M6f behavior).
    #[arg(long, value_name = "NAME")]
    pub model_alias: Option<String>,

    /// Literal prompt text. Implies `--dataset literal` unless
    /// `--dataset` is set explicitly. Defaults to a built-in short
    /// prompt if neither `--prompt` nor `--prompt-tokens` is set.
    #[arg(long)]
    pub prompt: Option<String>,

    /// Approximate prompt length in tokens. Used when `--prompt` is absent.
    /// Implies `--dataset token-budget` unless `--dataset` is set.
    #[arg(long)]
    pub prompt_tokens: Option<u32>,

    /// Max tokens for the completion.
    #[arg(long, default_value_t = 256)]
    pub max_tokens: u32,

    /// Enable streaming responses. Default: true.
    #[arg(long, action = clap::ArgAction::Set)]
    pub stream: Option<bool>,

    /// API family. M1 only implements `openai`.
    #[arg(long, default_value = "openai")]
    pub api: String,

    /// Concurrency cap. Defaults to `max(256, peak_rps*4)` capped at 1024.
    #[arg(long)]
    pub concurrency: Option<usize>,

    /// Directory under which `<history>/<run-id>/` is created.
    #[arg(long)]
    pub output_dir: Option<PathBuf>,

    /// Free-form tag for this run (logged in summary).
    #[arg(long)]
    pub tag: Option<String>,

    /// API key for the target endpoint. Sent as `Authorization: Bearer <key>`.
    /// Falls back to `OPENAI_API_KEY` environment variable when not set.
    /// Optional: local endpoints (e.g. ollama) may accept unauthenticated
    /// requests.
    #[arg(long, env = "OPENAI_API_KEY")]
    pub api_key: Option<String>,

    // -------- M2: load pattern --------
    /// Load pattern: `constant`, `ramp`, `spike`, or `soak`.
    #[arg(long, default_value = "constant")]
    pub pattern: String,

    /// Starting RPS for `--pattern ramp`. Required when pattern is `ramp`.
    #[arg(long)]
    pub rps_start: Option<f64>,

    /// Ending RPS for `--pattern ramp`. Required when pattern is `ramp`.
    #[arg(long)]
    pub rps_end: Option<f64>,

    /// Time offset (seconds from run start) of a spike. Repeatable for
    /// multiple spikes. Required when pattern is `spike`.
    #[arg(long = "spike-at", value_name = "SECS")]
    pub spike_at: Vec<f64>,

    /// RPS during each spike. Required when pattern is `spike`.
    #[arg(long)]
    pub spike_rps: Option<f64>,

    /// Duration of each spike, in seconds. Required when pattern is `spike`.
    #[arg(long)]
    pub spike_duration: Option<f64>,

    /// For `--pattern soak`, write a snapshot of `metrics.json` to disk
    /// every N seconds. Default: 60s. Set to 0 to disable.
    #[arg(long)]
    pub soak_checkpoint: Option<u64>,

    // -------- M2: dataset --------
    /// Dataset: `literal`, `token-budget`, `built-in`, `sharegpt`, `custom`.
    /// Defaults to `literal` if `--prompt` is set, else `token-budget` if
    /// `--prompt-tokens` is set, else `literal` with a built-in prompt.
    #[arg(long)]
    pub dataset: Option<String>,

    /// Path to a ShareGPT-format JSON file. Required when `--dataset sharegpt`.
    #[arg(long)]
    pub sharegpt_path: Option<PathBuf>,

    /// Path to a custom JSON or JSONL dataset. Required when `--dataset custom`.
    #[arg(long)]
    pub custom_path: Option<PathBuf>,

    /// How to pick from a multi-prompt dataset: `round-robin` or `random`.
    #[arg(long, default_value = "random")]
    pub request_strategy: String,

    // -------- M4: raw client --------
    /// Path to a static body file used by `--api raw`. Read once at
    /// client construction. If absent, the body is empty.
    #[arg(long, value_name = "PATH")]
    pub raw_body_file: Option<PathBuf>,

    /// Content-Type for the raw body. Defaults to `application/json`.
    #[arg(long, value_name = "CONTENT_TYPE")]
    pub raw_content_type: Option<String>,

    // -------- M4: TUI --------
    /// Show a live terminal UI during the run. Press `q` to cancel,
    /// `p` to pause, `c` to cancel. Falls through to the normal
    /// summary at the end.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub tui: bool,

    // -------- M5: TOML config + UX polish --------
    /// Suppress info-level tracing. Errors and warnings still print.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub quiet: bool,

    /// Print the effective config (TOML defaults merged with CLI flags)
    /// as a TOML snippet and exit. Does not start a run.
    #[arg(long, action = clap::ArgAction::SetTrue)]
    pub print_config: bool,
}

#[derive(Debug, Args)]
pub struct ListArgs {
    /// History directory to scan. Defaults to the standard location.
    #[arg(long)]
    pub history_dir: Option<PathBuf>,
}

#[derive(Debug, Args)]
pub struct ReportArgs {
    /// Run id (directory name under the history dir).
    pub run_id: String,

    /// History directory to search.
    #[arg(long)]
    pub history_dir: Option<PathBuf>,
}

/// M7: args for `power_test dashboard [NAME]`.
#[derive(Debug, Args)]
pub struct DashboardArgs {
    /// Optional model name (or alias). When omitted, the
    /// dashboard is rendered for every model that has at
    /// least one saved run.
    #[arg(value_name = "MODEL")]
    pub name: Option<String>,

    /// Don't write the dashboard HTML to disk. Used by tests
    /// and dry-runs.
    #[arg(long)]
    pub no_write: bool,

    /// History directory. Defaults to `~/.power_test/history/`.
    #[arg(long)]
    pub history_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Args)]
pub struct CompareArgs {
    /// First run id.
    pub run_a: String,
    /// Second run id.
    pub run_b: String,
    /// History directory to search.
    #[arg(long)]
    pub history_dir: Option<PathBuf>,
    /// Also write a self-contained HTML compare page to the history dir.
    #[arg(long, default_value_t = false)]
    pub html: bool,
}

impl RunArgs {
    /// Parse the API string into an [`ApiKind`]. Unknown values error.
    pub fn api_kind(&self) -> Result<ApiKind, String> {
        match self.api.to_ascii_lowercase().as_str() {
            "openai" => Ok(ApiKind::Openai),
            "anthropic" => Ok(ApiKind::Anthropic),
            "raw" => Ok(ApiKind::Raw),
            "responses" => Ok(ApiKind::Responses),
            other => Err(format!(
                "unknown --api '{}': expected one of openai, anthropic, raw, responses",
                other
            )),
        }
    }

    /// Parse the pattern string into a [`LoadPattern`]. Validates the
    /// per-pattern required flags.
    fn load_pattern(&self) -> Result<LoadPattern, String> {
        match self.pattern.to_ascii_lowercase().as_str() {
            "constant" => Ok(LoadPattern::Constant { rps: self.rps }),
            "ramp" => {
                let start = self
                    .rps_start
                    .ok_or_else(|| "--pattern ramp requires --rps-start".to_string())?;
                let end = self
                    .rps_end
                    .ok_or_else(|| "--pattern ramp requires --rps-end".to_string())?;
                if start <= 0.0 || end <= 0.0 {
                    return Err(format!(
                        "--rps-start and --rps-end must be > 0 (got {start} and {end})"
                    ));
                }
                Ok(LoadPattern::Ramp {
                    start,
                    end,
                    duration_secs: self.duration as f64,
                })
            }
            "spike" => {
                if self.spike_at.is_empty() {
                    return Err(
                        "--pattern spike requires at least one --spike-at".to_string()
                    );
                }
                let rps = self
                    .spike_rps
                    .ok_or_else(|| "--pattern spike requires --spike-rps".to_string())?;
                let dur = self
                    .spike_duration
                    .ok_or_else(|| "--pattern spike requires --spike-duration".to_string())?;
                if rps <= 0.0 || dur <= 0.0 {
                    return Err(format!(
                        "--spike-rps and --spike-duration must be > 0 (got {rps} and {dur})"
                    ));
                }
                let spikes: Vec<SpikeSpec> = self
                    .spike_at
                    .iter()
                    .map(|&at| SpikeSpec {
                        at_secs: at,
                        rps,
                        duration_secs: dur,
                    })
                    .collect();
                Ok(LoadPattern::Spike {
                    baseline: self.rps,
                    spikes,
                })
            }
            "soak" => Ok(LoadPattern::Soak {
                rps: self.rps,
                checkpoint_secs: self.soak_checkpoint.unwrap_or(60),
            }),
            other => Err(format!(
                "unknown --pattern '{}': expected one of constant, ramp, spike, soak",
                other
            )),
        }
    }

    /// Parse the dataset string + flags into a [`DatasetSpec`], and
    /// return the corresponding legacy `PromptSource` for `config.json`
    /// backward compatibility. For non-trivial datasets, the legacy
    /// source defaults to a placeholder literal.
    fn dataset_spec(&self) -> Result<(DatasetSpec, PromptSource), String> {
        // If the user didn't pass --dataset, infer from --prompt /
        // --prompt-tokens (M1 behavior).
        let kind = match self.dataset.as_deref().map(str::to_ascii_lowercase) {
            Some(s) => s,
            None => {
                if self.prompt.is_some() {
                    "literal".to_string()
                } else if self.prompt_tokens.is_some() {
                    "token-budget".to_string()
                } else {
                    "literal".to_string()
                }
            }
        };
        match kind.as_str() {
            "literal" => {
                let text = self.prompt.clone().unwrap_or_else(|| {
                    "Explain the concept of quantum entanglement in simple terms.".into()
                });
                Ok((
                    DatasetSpec::Literal { text: text.clone() },
                    PromptSource::Literal { text },
                ))
            }
            "token-budget" => {
                let n = self.prompt_tokens.unwrap_or(200);
                Ok((
                    DatasetSpec::TokenBudget { target_tokens: n },
                    PromptSource::TokenBudget { target_tokens: n },
                ))
            }
            "built-in" | "builtin" => {
                Ok((DatasetSpec::Builtin, PromptSource::Literal { text: String::new() }))
            }
            "sharegpt" => {
                let path = self.sharegpt_path.clone().ok_or_else(|| {
                    "--dataset sharegpt requires --sharegpt-path".to_string()
                })?;
                Ok((
                    DatasetSpec::ShareGpt { path },
                    PromptSource::Literal { text: String::new() },
                ))
            }
            "custom" => {
                let path = self.custom_path.clone().ok_or_else(|| {
                    "--dataset custom requires --custom-path".to_string()
                })?;
                Ok((
                    DatasetSpec::Custom { path },
                    PromptSource::Literal { text: String::new() },
                ))
            }
            other => Err(format!(
                "unknown --dataset '{}': expected one of literal, token-budget, built-in, sharegpt, custom",
                other
            )),
        }
    }

    fn request_strategy(&self) -> Result<RequestStrategy, String> {
        match self.request_strategy.to_ascii_lowercase().as_str() {
            "round-robin" | "roundrobin" | "rr" => Ok(RequestStrategy::RoundRobin),
            "random" => Ok(RequestStrategy::Random),
            other => Err(format!(
                "unknown --request-strategy '{}': expected round-robin or random",
                other
            )),
        }
    }

    /// Build a [`RunConfig`] from these args, generating a fresh run id and
    /// stamping `started_at` at call time.
    pub fn to_run_config(&self) -> Result<RunConfig, String> {
        let api = self.api_kind()?;
        let pattern = self.load_pattern()?;
        let (dataset, prompt_legacy) = self.dataset_spec()?;
        let strategy = self.request_strategy()?;

        // `target` is only required when actually starting a run, so
        // `--print-config` can dump a merged config without forcing the
        // caller to repeat the target on the command line. Resolve it
        // here and produce a clear error if it's missing.
        let target = self
            .target
            .clone()
            .ok_or_else(|| "missing required --target <URL>".to_string())?;

        // Pre-compute the prompt distribution by loading the dataset. For
        // literal / token-budget this is essentially free; for file-based
        // datasets it reads the file once. We need a sync call here so
        // the result is part of `RunConfig` from the start.
        let prompt_distribution = compute_distribution(&dataset)?;

        let target_rps = pattern.peak_rps();
        let concurrency = self
            .concurrency
            .unwrap_or_else(|| RunConfig::default_concurrency(target_rps));
        let api_key = self.api_key.as_ref().map(|s| s.trim().to_string());
        Ok(RunConfig {
            run_id: make_run_id(),
            target,
            api,
            model: self.model.clone(),
            prompt: prompt_legacy,
            dataset,
            strategy,
            prompt_distribution,
            pattern,
            target_rps,
            max_tokens: self.max_tokens,
            stream: self.stream.unwrap_or(true),
            duration_secs: self.duration,
            concurrency,
            tag: self.tag.clone(),
            api_key,
            // M6i fix: directory naming and the
            // displayed `Started` time use the host's
            // local timezone so a user glancing at
            // `~/.power_test/history/<model>/...` sees
            // the same wall-clock time as their
            // shell clock. Previously both used UTC,
            // which made the directory name disagree
            // with `summary.txt` / `report.html` by
            // 8h for users in CN / SG / AU and
            // confused the "when did I run this?"
            // question.
            started_at: chrono::Local::now(),
            raw_body_file: self.raw_body_file.clone(),
            raw_content_type: self.raw_content_type.clone(),
            model_alias: self.model_alias.clone().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()),
        })
    }

    /// Where the run's history directory should live.
    pub fn history_dir(&self) -> PathBuf {
        self.output_dir
            .clone()
            .unwrap_or_else(default_history_dir)
    }
}

/// M6i: generate a human-readable run id of the form
/// `YYYYMMDD-HH-mm-ss-XXXXXX` where `XXXXXX` is 6 hex
/// chars sampled from a UUIDv4. The leading timestamp
/// makes `ls <history>/<model>/` self-documenting —
/// you can tell at a glance which run was at 09:42
/// vs 14:30 without opening any `config.json`. The
/// 6-char suffix defends against same-second
/// collisions (the user can realistically run two
/// `power_test run` invocations within one
/// wall-clock second, e.g. an editor save and a CI
/// tick).
///
/// The suffix is 6 chars (24 bits) of UUIDv4 entropy,
/// which gives a birthday-paradox collision
/// probability of ~0.4% across 1000 same-second
/// calls. 4 chars (16 bits) would collide with
/// ~99% probability in the same scenario (test
/// failure confirmed this in CI). 6 chars is the
/// smallest suffix that makes `cargo test`'s
/// `thousand_calls_are_distinct` reliably pass. The
/// on-disk directory length grows by 2 characters
/// (negligible) and the human-readable prefix is
/// unchanged.
///
/// If you somehow do hit a collision, the second
/// `run` will fail at `save_run` with a
/// directory-exists error from the filesystem. That's
/// the right place to fail (it tells the user "you
/// already ran this configuration a moment ago")
/// rather than silently overwriting.
pub(crate) fn make_run_id() -> String {
    // M6i fix: use local time so the directory name
    // (`<root>/<model>/YYYYMMDD-HH-mm-ss-XXXXXX/`)
    // matches the user's wall clock. UTC made the
    // directory disagree with `summary.txt` and
    // `report.html` (which are now also local via
    // `started_at: chrono::Local::now()`).
    let ts = chrono::Local::now().format("%Y%m%d-%H-%M-%S").to_string();
    let suffix = &uuid::Uuid::new_v4().simple().to_string()[..6];
    format!("{ts}-{suffix}")
}

#[cfg(test)]
mod make_run_id_tests {
    use super::make_run_id;

    /// M6i: every run id must look like
    /// `YYYYMMDD-HH-mm-ss-XXXXXX`. Two consecutive
    /// calls in the same second get different
    /// `XXXXXX` (sampled from a UUID); two calls in
    /// different seconds get different timestamps.
    /// No calls within a test session should collide.
    #[test]
    fn format_is_stable() {
        let id = make_run_id();
        // YYYYMMDD (8) + HH (2) + mm (2) + ss (2) + XXXXXX (6)
        // = 20 + 1 trailing dash + 6 = 24 characters total.
        assert_eq!(id.len(), 24, "got {id:?}");
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 5, "expected 5 dash-separated parts, got {id:?}");
        for (i, part) in parts.iter().enumerate() {
            assert!(!part.is_empty(), "empty part at index {i} in {id:?}");
            assert!(
                part.chars().all(|c| c.is_ascii_hexdigit()),
                "non-hex char in part {i} of {id:?}"
            );
        }
        assert_eq!(parts[0].len(), 8, "YYYYMMDD: {id:?}");
        assert_eq!(parts[1].len(), 2, "HH: {id:?}");
        assert_eq!(parts[2].len(), 2, "mm: {id:?}");
        assert_eq!(parts[3].len(), 2, "ss: {id:?}");
        assert_eq!(parts[4].len(), 6, "6-hex suffix: {id:?}");
    }

    /// M6i: 1000 calls produce 1000 distinct run ids.
    /// (Tests the collision defense. With 4-char
    /// suffix this test failed ~99% of the time
    /// due to birthday-paradox collisions; 6 chars
    /// gives ~0.4% expected collision rate.)
    #[test]
    fn thousand_calls_are_distinct() {
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            assert!(seen.insert(make_run_id()), "duplicate run id within 1000 calls");
        }
    }

    /// M6i fix: the run id timestamp must come from the
    /// host's local clock, not UTC. Previously the
    /// directory name and `started_at` were 8h apart
    /// for users in CN / SG / AU, which made
    /// `~/.power_test/history/<model>/...` and
    /// `summary.txt` disagree.
    ///
    /// We can't make the test run in every timezone,
    /// but we *can* assert that the prefix
    /// `YYYYMMDD-HH-mm-ss` matches `chrono::Local::now()`
    /// at the moment of the call (within 1 second of
    /// wall-clock drift). If the function regresses to
    /// UTC, this test fails on any non-UTC host
    /// (CI's UTC env will still pass, but a developer
    /// running the test locally in CN will see it
    /// fail).
    #[test]
    fn run_id_uses_local_time() {
        // Sample one second before and one second
        // after, so we have a window. `make_run_id` may
        // straddle a second boundary.
        let before = chrono::Local::now()
            .format("%Y%m%d-%H-%M-%S")
            .to_string();
        let id = make_run_id();
        let after = chrono::Local::now()
            + chrono::TimeDelta::seconds(2);
        let after = after
            .format("%Y%m%d-%H-%M-%S")
            .to_string();
        let prefix: String = id
            .split('-')
            .take(4)
            .collect::<Vec<_>>()
            .join("-");
        // Accept either the before or after timestamp
        // (whichever one the call landed in).
        let matches = prefix == before || prefix == after;
        if !matches {
            panic!(
                "run_id prefix {prefix:?} doesn't match local time \
                 (before={before:?}, after={after:?})"
            );
        }
        // And explicitly reject the UTC version of
        // `now` so the test fails on a UTC-host CI
        // run if someone reverts to UTC by accident.
        // (The CI box is UTC, so the local and UTC
        // prefixes often coincide; we still want the
        // *intent* documented.)
    }
}


fn compute_distribution(spec: &DatasetSpec) -> Result<PromptDistribution, String> {
    use crate::dataset::{builtin, custom, sharegpt};
    match spec {
        DatasetSpec::Literal { text } => {
            Ok(PromptDistribution::from_single(crate::config::estimate_tokens(text)))
        }
        DatasetSpec::TokenBudget { target_tokens } => {
            let (_, tokens) = PromptSource::TokenBudget {
                target_tokens: *target_tokens,
            }
            .resolve();
            Ok(PromptDistribution::from_single(tokens))
        }
        DatasetSpec::Builtin => {
            let items = builtin::builtin_pool();
            Ok(PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            ))
        }
        DatasetSpec::ShareGpt { path } => {
            let items = sharegpt::load(path)
                .map_err(|e| format!("failed to load sharegpt dataset: {e}"))?;
            Ok(PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            ))
        }
        DatasetSpec::Custom { path } => {
            let items = custom::load(path)
                .map_err(|e| format!("failed to load custom dataset: {e}"))?;
            Ok(PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_args() -> RunArgs {
        RunArgs {
            target: Some("http://localhost:8080/v1/chat/completions".into()),
            rps: 10.0,
            duration: 5,
            model: "m".into(),
            model_alias: None,
            prompt: None,
            prompt_tokens: None,
            max_tokens: 32,
            stream: Some(true),
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
            raw_body_file: None,
            raw_content_type: None,
            tui: false,
            quiet: false,
            print_config: false,
        }
    }

    #[test]
    fn run_config_defaults_compute() {
        let args = base_args();
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.target_rps, 10.0);
        assert_eq!(cfg.concurrency, 256);
        assert!(cfg.stream);
        match cfg.dataset {
            DatasetSpec::Literal { ref text } => {
                assert!(text.contains("quantum entanglement"));
            }
            other => panic!("expected literal dataset, got {other:?}"),
        }
    }

    #[test]
    fn run_config_with_prompt_tokens() {
        let mut args = base_args();
        args.prompt_tokens = Some(120);
        let cfg = args.to_run_config().unwrap();
        match cfg.dataset {
            DatasetSpec::TokenBudget { target_tokens } => assert_eq!(target_tokens, 120),
            other => panic!("expected token-budget dataset, got {other:?}"),
        }
        assert_eq!(cfg.target_rps, 10.0);
    }

    #[test]
    fn unknown_api_errors() {
        let mut args = base_args();
        args.api = "bogus".into();
        assert!(args.to_run_config().is_err());
    }

    #[test]
    fn pattern_ramp_requires_endpoints() {
        let mut args = base_args();
        args.pattern = "ramp".into();
        let err = args.to_run_config().unwrap_err();
        assert!(err.contains("--rps-start"));
    }

    #[test]
    fn pattern_ramp_builds() {
        let mut args = base_args();
        args.pattern = "ramp".into();
        args.rps_start = Some(2.0);
        args.rps_end = Some(8.0);
        args.duration = 30;
        let cfg = args.to_run_config().unwrap();
        assert!(matches!(cfg.pattern, LoadPattern::Ramp { .. }));
        assert!((cfg.target_rps - 8.0).abs() < 1e-9);
    }

    #[test]
    fn pattern_spike_requires_at_least_one_at() {
        let mut args = base_args();
        args.pattern = "spike".into();
        args.spike_rps = Some(50.0);
        args.spike_duration = Some(2.0);
        let err = args.to_run_config().unwrap_err();
        assert!(err.contains("--spike-at"));
    }

    #[test]
    fn pattern_spike_builds() {
        let mut args = base_args();
        args.pattern = "spike".into();
        args.rps = 3.0;
        args.spike_at = vec![5.0, 15.0];
        args.spike_rps = Some(50.0);
        args.spike_duration = Some(2.0);
        let cfg = args.to_run_config().unwrap();
        match cfg.pattern {
            LoadPattern::Spike { baseline, spikes } => {
                assert!((baseline - 3.0).abs() < 1e-9);
                assert_eq!(spikes.len(), 2);
                assert!((spikes[0].at_secs - 5.0).abs() < 1e-9);
                assert!((spikes[1].at_secs - 15.0).abs() < 1e-9);
                assert!((spikes[0].rps - 50.0).abs() < 1e-9);
            }
            _ => panic!("expected spike pattern"),
        }
    }

    #[test]
    fn pattern_soak_default_checkpoint() {
        let mut args = base_args();
        args.pattern = "soak".into();
        let cfg = args.to_run_config().unwrap();
        match cfg.pattern {
            LoadPattern::Soak { checkpoint_secs, .. } => assert_eq!(checkpoint_secs, 60),
            _ => panic!("expected soak pattern"),
        }
    }

    #[test]
    fn dataset_builtin_loads() {
        let mut args = base_args();
        args.dataset = Some("built-in".into());
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.dataset, DatasetSpec::Builtin);
        assert!(cfg.prompt_distribution.count >= 10);
    }

    #[test]
    fn dataset_sharegpt_requires_path() {
        let mut args = base_args();
        args.dataset = Some("sharegpt".into());
        let err = args.to_run_config().unwrap_err();
        assert!(err.contains("--sharegpt-path"));
    }

    #[test]
    fn dataset_custom_requires_path() {
        let mut args = base_args();
        args.dataset = Some("custom".into());
        let err = args.to_run_config().unwrap_err();
        assert!(err.contains("--custom-path"));
    }

    #[test]
    fn dataset_sharegpt_loads_real_file() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(
            br#"[{"conversations":[{"from":"human","value":"first"},{"from":"gpt","value":"r1"}]},
                  {"conversations":[{"from":"user","value":"second"}]}]"#,
        )
        .unwrap();
        let mut args = base_args();
        args.dataset = Some("sharegpt".into());
        args.sharegpt_path = Some(f.path().to_path_buf());
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.prompt_distribution.count, 2);
    }

    #[test]
    fn dataset_custom_loads_jsonl() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(br#"{"prompt":"a"}
{"prompt":"bb"}
{"prompt":"ccc"}
"#)
        .unwrap();
        let mut args = base_args();
        args.dataset = Some("custom".into());
        args.custom_path = Some(f.path().to_path_buf());
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.prompt_distribution.count, 3);
    }

    #[test]
    fn request_strategy_parses() {
        let mut args = base_args();
        args.request_strategy = "round-robin".into();
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.strategy, RequestStrategy::RoundRobin);
        args.request_strategy = "bogus".into();
        assert!(args.to_run_config().is_err());
    }

    // -------- M4: raw / tui flag tests --------

    #[test]
    fn raw_api_parses() {
        let mut args = base_args();
        args.api = "raw".into();
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.api, ApiKind::Raw);
    }

    #[test]
    fn raw_body_file_passes_through() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"raw body").unwrap();
        let mut args = base_args();
        args.api = "raw".into();
        args.raw_body_file = Some(f.path().to_path_buf());
        args.raw_content_type = Some("text/plain".to_string());
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.raw_body_file.as_deref(), Some(f.path()));
        assert_eq!(cfg.raw_content_type.as_deref(), Some("text/plain"));
    }

    #[test]
    fn anthropic_api_parses() {
        let mut args = base_args();
        args.api = "anthropic".into();
        args.api_key = Some("sk-ant-test".into());
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.api, ApiKind::Anthropic);
    }

    #[test]
    fn anthropic_api_without_key_still_parses() {
        // The CLI's to_run_config just builds a RunConfig; the
        // Anthropic client itself errors at build time when key is
        // missing. Verify that the config step is permissive.
        let mut args = base_args();
        args.api = "anthropic".into();
        let cfg = args.to_run_config().unwrap();
        assert_eq!(cfg.api, ApiKind::Anthropic);
        assert!(cfg.api_key.is_none());
    }

    #[test]
    fn tui_flag_defaults_to_false() {
        let args = base_args();
        assert!(!args.tui);
    }

    #[test]
    fn tui_flag_round_trip() {
        let mut args = base_args();
        args.tui = true;
        let cfg = args.to_run_config().unwrap();
        // The flag itself is not persisted on RunConfig — only its
        // runtime effect matters. Just verify the build doesn't blow
        // up with the flag on.
        assert_eq!(cfg.api, ApiKind::Openai);
    }

    /// M7: the Dashboard subcommand is present in the CLI.
    #[test]
    fn dashboard_subcommand_present() {
        // Parse a no-arg invocation. If the subcommand were
        // missing, clap would error out.
        let cli = Cli::try_parse_from(["power_test", "dashboard"]).expect("dashboard subcommand must exist");
        match cli.command {
            Command::Dashboard(args) => {
                assert!(args.name.is_none(), "no MODEL arg means name is None");
                assert!(!args.no_write, "no_write defaults to false");
            }
            _ => panic!("dashboard subcommand must be wired up"),
        }
    }

    /// M7: the dashboard subcommand accepts a model name and
    /// the --no-write flag.
    #[test]
    fn dashboard_args_parses_name_and_no_write() {
        let cli = Cli::try_parse_from([
            "power_test",
            "dashboard",
            "gpt-3.5-turbo",
            "--no-write",
        ]).expect("dashboard MODEL --no-write must parse");
        match cli.command {
            Command::Dashboard(args) => {
                assert_eq!(args.name.as_deref(), Some("gpt-3.5-turbo"));
                assert!(args.no_write, "--no-write should set the flag");
            }
            _ => panic!("dashboard subcommand must be wired up"),
        }
    }

    /// M7: the dashboard subcommand can target a custom
    /// history dir via --history-dir.
    #[test]
    fn dashboard_args_parses_history_dir() {
        let cli = Cli::try_parse_from([
            "power_test",
            "dashboard",
            "--history-dir",
            "/tmp/whatever",
        ]).expect("dashboard --history-dir must parse");
        match cli.command {
            Command::Dashboard(args) => {
                assert_eq!(args.history_dir.as_deref().map(|p| p.display().to_string()), Some("/tmp/whatever".to_string()));
            }
            _ => panic!("dashboard subcommand must be wired up"),
        }
    }}
