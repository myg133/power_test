//! Entry point and CLI dispatch. All real logic lives in the library
//! crate; this file is a thin shim that wires clap, tracing, and ctrl-c
//! into the runner.

#![deny(warnings)]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use clap::Parser;
use tokio::sync::Notify;
use tracing_subscriber::{fmt, EnvFilter};

use power_test::cli::{Cli, Command, CompareArgs, ListArgs, ReportArgs, RunArgs};
use power_test::config::RunStatus;
use power_test::error::{Error, Result};
use power_test::runner::MetricsAggregator;
use power_test::{compare, config, config_io, report, runner, storage, tui};

fn main() {
    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("error: failed to build tokio runtime: {e}");
            std::process::exit(1);
        }
    };
    if let Err(e) = rt.block_on(run()) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let cli = Cli::parse();
    // Tracing is initialised from `--log-level` / `--quiet` before we
    // dispatch, so the cmd_* functions inherit the correct filter.
    init_tracing(cli.log_level.as_str(), false);
    match cli.command {
        Command::Run(args) => cmd_run(args, cli.config).await,
        Command::List(args) => cmd_list(args),
        Command::Report(args) => cmd_report(args).await,
        Command::Compare(args) => cmd_compare(args),
    }
}

fn init_tracing(log_level: &str, quiet: bool) {
    let level = if quiet { "warn" } else { log_level };
    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(level));
    let _ = fmt().with_env_filter(filter).with_target(false).try_init();
}

async fn cmd_run(mut args: RunArgs, config_path: Option<PathBuf>) -> Result<()> {
    // Re-initialise tracing for `--quiet` (overrides the earlier init).
    if args.quiet {
        init_tracing("warn", true);
    }

    // Load TOML config (--config wins over the default lookup).
    let resolved_path = config_path
        .or_else(config_io::find_default);
    if let Some(path) = resolved_path.as_ref() {
        let toml_cfg = config_io::load(path)?;
        config_io::merge_into_run_args(toml_cfg, &mut args);
    }

    // `--print-config` short-circuits: print the effective merged config
    // as a TOML snippet and exit cleanly.
    if args.print_config {
        let snippet = config_io::print_config(&args);
        if let Some(path) = resolved_path {
            println!("# merged from: {}", path.display());
        }
        print!("{snippet}");
        return Ok(());
    }

    let cfg = args.to_run_config().map_err(Error::InvalidConfig)?;
    let history_dir = args.history_dir();
    storage::ensure_history_dir(&history_dir)?;

    tracing::info!(
        "starting run {} -> {} (rps={}, duration={}s, concurrency={})",
        cfg.run_id,
        cfg.target,
        cfg.target_rps,
        cfg.duration_secs,
        cfg.concurrency
    );

    let cancel = Arc::new(Notify::new());
    let cancel_for_signal = cancel.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            tracing::warn!("ctrl-c received, stopping...");
            cancel_for_signal.notify_waiters();
        }
    });

    // Optional TUI: spawn a blocking thread that owns the terminal,
    // watches the shared aggregator, and signals cancel on `q`/`Esc`.
    // The runner writes its metrics into the same Arc<Mutex<...>>
    // the TUI is reading from.
    let tui_handle = if args.tui {
        let cancel_t = cancel.clone();
        let cfg_t = cfg.clone();
        let paused = Arc::new(AtomicBool::new(false));
        let agg_for_tui: Arc<Mutex<MetricsAggregator>> =
            Arc::new(Mutex::new(MetricsAggregator::new()));
        let agg_for_runner = agg_for_tui.clone();
        let duration = cfg.duration_secs;
        Some((
            tokio::task::spawn_blocking(move || {
                tui::run_tui(agg_for_tui, &cfg_t, duration, cancel_t, Some(paused))
            }),
            agg_for_runner,
        ))
    } else {
        None
    };

    let (shared_agg, tui_handle) = match tui_handle {
        Some((h, agg)) => (Some(agg), Some(h)),
        None => (None, None),
    };

    let output = runner::run_with_cancel(
        runner::RunOptions {
            config: cfg,
            history_dir: history_dir.clone(),
            shared_aggregator: shared_agg,
        },
        cancel,
    )
    .await?;

    if let Some(h) = tui_handle {
        // The TUI thread is keyed on the cancel Notify; once the
        // run is done, the helper should exit. Give it a moment.
        let _ = tokio::time::timeout(std::time::Duration::from_millis(200), h).await;
    }

    let summary_text =
        report::render_summary(&output.config, &output.aggregator, output.interrupted);
    let report_html = report::render_html(&output.config, &output.aggregator, output.interrupted);
    let status = if output.interrupted {
        RunStatus::Interrupted
    } else {
        RunStatus::Completed
    };

    let entry = storage::save_run(
        &history_dir,
        &output.run_id,
        &output.config,
        &output.aggregator,
        &summary_text,
        &report_html,
        status,
    )?;

    println!();
    print!("{summary_text}");
    println!("artifacts");
    println!("---------");
    println!("  run id:      {}", entry.run_id);
    println!("  history dir: {}", output.history_dir.display());
    println!("  report:      {}/report.html", output.history_dir.display());
    println!("  summary:     {}/summary.txt", output.history_dir.display());

    Ok(())
}

fn cmd_list(args: ListArgs) -> Result<()> {
    let dir = args.history_dir.unwrap_or_else(config::default_history_dir);
    let runs = storage::list_runs(&dir)?;
    if runs.is_empty() {
        println!("(no runs)");
        return Ok(());
    }
    println!(
        "{:<36}  {:<23}  {:>6}  {:<11}  {:>8}  {}",
        "RUN ID", "TIMESTAMP", "RPS", "STATUS", "REQUESTS", "TARGET"
    );
    for r in &runs {
        let target_short = if r.target.chars().count() > 60 {
            let truncated: String = r.target.chars().take(57).collect();
            format!("{truncated}…")
        } else {
            r.target.clone()
        };
        println!(
            "{:<36}  {:<23}  {:>6.2}  {:<11}  {:>8}  {}",
            r.run_id,
            r.timestamp.format("%Y-%m-%dT%H:%M:%SZ"),
            r.rps,
            r.status.as_str(),
            r.total_requests,
            target_short
        );
    }
    Ok(())
}

async fn cmd_report(args: ReportArgs) -> Result<()> {
    let history_dir = args.history_dir.unwrap_or_else(config::default_history_dir);
    let (config, metrics_json) = storage::load_run(&history_dir, &args.run_id)?;

    let records: Vec<runner::RequestRecord> = match metrics_json.get("per_request") {
        Some(v) => serde_json::from_value(v.clone())?,
        None => Vec::new(),
    };
    let aggregator = runner::MetricsAggregator::from_records(&records);
    let interrupted = false; // not persisted in metrics.json in M1

    // M6g: compare-with dropdown lists runs of the same GROUP
    // KEY (alias if set, else model). This means
    // `DeepSeek-V4-Flash-20260115` and
    // `DeepSeek-V4-Flash-20260201` both with
    // `--model-alias DeepSeek-V4-Flash` show up in each
    // other's compare dropdown even though their actual
    // model strings differ.
    let group_key = storage::effective_group_key(
        &config.model,
        config.model_alias.as_deref(),
    );
    let compare_links =
        discover_compare_links(&history_dir, &args.run_id, group_key);

    let summary_text = report::render_summary(&config, &aggregator, interrupted);
    let report_html =
        report::render_html_with_compare(&config, &aggregator, interrupted, &compare_links);

    // M6g: write back into the same group-keyed directory
    // (alias when set, else model). Matches the original
    // save path so re-rendering doesn't move the run.
    let dir = storage::run_dir(&history_dir, group_key, &args.run_id);
    std::fs::write(dir.join("summary.txt"), &summary_text)
        .map_err(|e| Error::io_at(dir.join("summary.txt"), e))?;
    std::fs::write(dir.join("report.html"), &report_html)
        .map_err(|e| Error::io_at(dir.join("report.html"), e))?;

    println!("re-rendered report for run {}", args.run_id);
    println!("  report.html: {}/report.html", dir.display());
    println!("  summary.txt: {}/summary.txt", dir.display());
    println!("  records:     {}", records.len());

    Ok(())
}

/// Scan the history dir for runs sharing the same group key
/// (alias if set, else model) as `self_id` and return a list
/// of `(other_id, compare_filename)` pairs, newest first.
/// Each `compare_filename` is the path that `power_test
/// compare --html` would write; the report's inline JS
/// greys the link out if the file isn't actually present.
/// We don't filter by existence here so the dropdown
/// always shows the candidates.
fn discover_compare_links(
    history_dir: &std::path::Path,
    self_id: &str,
    group_key: &str,
) -> Vec<(String, String)> {
    let Ok(others) = storage::list_runs_by_alias(history_dir, group_key, Some(self_id)) else {
        return Vec::new();
    };
    others
        .into_iter()
        .take(10)
        .map(|e| {
            let filename = format!(
                "compare-{}-vs-{}.html",
                short_id(self_id),
                short_id(&e.run_id)
            );
            (e.run_id, filename)
        })
        .collect()
}

/// Compare two historical runs side-by-side. Prints a text table of
/// metric deltas (with ANSI color when stdout is a TTY) and optionally
/// writes a self-contained HTML page to the history dir.
fn cmd_compare(args: CompareArgs) -> Result<()> {
    let history_dir = args.history_dir.unwrap_or_else(config::default_history_dir);

    let (cfg_a, records_a, status_a) = storage::load_compare_data(&history_dir, &args.run_a)?;
    let (cfg_b, records_b, status_b) = storage::load_compare_data(&history_dir, &args.run_b)?;

    let inputs = compare::CompareInputs {
        cfg_a,
        records_a,
        status_a,
        cfg_b,
        records_b,
        status_b,
    };

    let use_color = compare::stdout_supports_color();
    let text = compare::render_text(&inputs, use_color);
    print!("{text}");

    if args.html {
        let html = compare::render_html(&inputs);
        let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
        let filename = format!(
            "compare-{}-vs-{}-{}.html",
            short_id(&args.run_a),
            short_id(&args.run_b),
            ts
        );
        let path = history_dir.join(&filename);
        std::fs::write(&path, html).map_err(|e| Error::io_at(&path, e))?;
        println!();
        println!("html report: {}", path.display());
    }

    Ok(())
}

/// Short, filesystem-friendly identifier for a run id (UUIDs are
/// already short, but a user-typed string could be anything).
fn short_id(s: &str) -> String {
    // Keep alphanumerics, dashes, and underscores; replace anything
    // else with underscore. Cap at 12 chars to keep the filename sane.
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .take(12)
        .collect();
    if cleaned.is_empty() {
        "run".to_string()
    } else {
        cleaned
    }
}
