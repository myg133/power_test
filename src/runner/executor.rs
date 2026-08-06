//! The test executor. Owns the semaphore, the load pattern, and the metrics
//! aggregator. Spawns one worker task per acquired permit.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, Semaphore};
use tokio::time::sleep;

use super::metrics::MetricsAggregator;
use super::pattern::LoadPattern as DynLoadPattern;
use crate::client::{self, LlmClient};
use crate::config::{LoadPattern, RunConfig};
use crate::dataset::{self, Dataset};
use crate::error::{Error, Result};

/// Inputs to [`run`].
pub struct RunOptions {
    pub config: RunConfig,
    /// Where the run's history directory lives. The runner does not create
    /// the directory; that's the caller's job (see `storage::history`).
    pub history_dir: PathBuf,
    /// Optional external aggregator. When `Some`, the runner writes its
    /// metrics into this `Arc<Mutex<...>>` instead of creating one
    /// internally. This lets the TUI share the same live counters.
    /// When `None`, the runner allocates its own aggregator (M1/M2/M3
    /// behavior).
    #[allow(dead_code)]
    pub shared_aggregator: Option<Arc<Mutex<MetricsAggregator>>>,
}

impl RunOptions {
    /// Convenience constructor that doesn't share the aggregator.
    pub fn new(config: RunConfig, history_dir: PathBuf) -> Self {
        Self {
            config,
            history_dir,
            shared_aggregator: None,
        }
    }
}

/// What [`run`] returns.
pub struct RunOutput {
    pub run_id: String,
    pub history_dir: PathBuf,
    pub aggregator: MetricsAggregator,
    pub config: RunConfig,
    /// `true` if the run was stopped early by ctrl-c.
    pub interrupted: bool,
}

/// Like [`run_with_cancel`], but with no external cancel signal — the run
/// stops only when the configured duration elapses.
#[allow(dead_code)]
pub async fn run(opts: RunOptions) -> Result<RunOutput> {
    run_with_cancel(opts, Arc::new(Notify::new())).await
}

/// Run one test to completion (or until cancelled). Never panics on
/// per-request errors — those are recorded into the aggregator.
pub async fn run_with_cancel(opts: RunOptions, cancel: Arc<Notify>) -> Result<RunOutput> {
    let cfg = opts.config.clone();
    let history_dir = opts.history_dir.join(&cfg.run_id);
    std::fs::create_dir_all(&history_dir)
        .map_err(|e| Error::io_at(&history_dir, e))?;

    // Build the dataset and client.
    let dataset: Arc<dyn Dataset> = Arc::from(dataset::build(&cfg.dataset, cfg.strategy)?);
    let client: Arc<dyn LlmClient> = Arc::from(client::build(&cfg)?);

    let semaphore = Arc::new(Semaphore::new(cfg.concurrency));
    let agg = opts
        .shared_aggregator
        .clone()
        .unwrap_or_else(|| Arc::new(Mutex::new(MetricsAggregator::new())));
    let start = Arc::new(Instant::now());
    let run_started = chrono::Utc::now();
    {
        let mut g = agg.lock().unwrap();
        g.set_run_started_at(run_started);
    }

    // Build the pattern. The trait object hides whether we're running
    // constant, ramp, spike, or soak.
    let pattern: Box<dyn DynLoadPattern> = super::pattern::from_config(&cfg.pattern);
    let pattern = Arc::new(tokio::sync::Mutex::new(pattern));

    // Spawn the scheduler.
    let scheduler_cancel = cancel.clone();
    let scheduler_handle = {
        let cfg = cfg.clone();
        let semaphore = semaphore.clone();
        let agg = agg.clone();
        let start = start.clone();
        let client = client.clone();
        let dataset = dataset.clone();
        let pattern = pattern.clone();
        tokio::spawn(async move {
            scheduler_loop(
                pattern,
                semaphore,
                cfg,
                agg,
                start,
                client,
                dataset,
                scheduler_cancel,
            )
            .await;
        })
    };

    // For the soak pattern, spawn a checkpoint task that flushes the
    // aggregator to `metrics.json` every `checkpoint_secs`. A crashed
    // long run is then partially analyzable.
    let checkpoint_handle = if let LoadPattern::Soak { checkpoint_secs, .. } = cfg.pattern {
        if checkpoint_secs > 0 {
            let cancel_c = cancel.clone();
            let agg_c = agg.clone();
            let dir_c = history_dir.clone();
            let path = dir_c.join("metrics.json");
            Some(tokio::spawn(async move {
                soak_checkpoint_loop(agg_c, path, Duration::from_secs(checkpoint_secs), cancel_c)
                    .await;
            }))
        } else {
            None
        }
    } else {
        None
    };

    // Main wait: duration or cancel.
    let interrupted = tokio::select! {
        _ = sleep(Duration::from_secs(cfg.duration_secs)) => false,
        _ = cancel.notified() => true,
    };

    if interrupted {
        tracing::warn!("interrupted: draining in-flight requests (10s timeout)");
    }

    // Tell scheduler to stop and wait for it.
    cancel.notify_waiters();
    let _ = scheduler_handle.await;
    if let Some(h) = checkpoint_handle {
        let _ = h.await;
    }

    // Drain in-flight workers (up to 10s).
    let drain_deadline = Instant::now() + Duration::from_secs(10);
    while semaphore.available_permits() < cfg.concurrency {
        if Instant::now() >= drain_deadline {
            tracing::warn!(
                "drain timeout: {} permits still in use",
                cfg.concurrency - semaphore.available_permits()
            );
            break;
        }
        sleep(Duration::from_millis(50)).await;
    }

    // Extract the aggregator. There should be no other references at this
    // point (scheduler joined, all workers done). Use `try_unwrap` and fall
    // back to a clone if necessary.
    let aggregator = match Arc::try_unwrap(agg) {
        Ok(mutex) => mutex.into_inner().unwrap_or_else(|_| MetricsAggregator::new()),
        Err(arc) => {
            let guard = arc.lock().unwrap();
            let mut fresh = MetricsAggregator::new();
            fresh.set_run_started_at(guard.run_started_at());
            for r in guard.per_request() {
                fresh.push_record_clone(r);
            }
            fresh
        }
    };

    Ok(RunOutput {
        run_id: cfg.run_id.clone(),
        history_dir,
        aggregator,
        config: cfg,
        interrupted,
    })
}

#[allow(clippy::too_many_arguments)]
async fn scheduler_loop(
    pattern: Arc<tokio::sync::Mutex<Box<dyn DynLoadPattern>>>,
    semaphore: Arc<Semaphore>,
    cfg: RunConfig,
    agg: Arc<Mutex<MetricsAggregator>>,
    start: Arc<Instant>,
    client: Arc<dyn LlmClient>,
    dataset: Arc<dyn Dataset>,
    cancel: Arc<Notify>,
) {
    let duration = Duration::from_secs(cfg.duration_secs);
    let mut guard = pattern.lock().await;
    loop {
        // Belt-and-suspenders: even if `cancel.notified()` was missed
        // because we were blocked inside `guard.tick()` (Notify has no
        // stored-permit semantics, so notify_waiters fires only on
        // currently-registered waiters), check the wall clock here so
        // the scheduler still exits when the configured duration
        // elapses.
        if start.elapsed() >= duration {
            break;
        }
        tokio::select! {
            biased;
            _ = cancel.notified() => break,
            _ = guard.tick() => {}
        }
        // Try to grab a permit non-blockingly.
        match Arc::clone(&semaphore).try_acquire_owned() {
            Ok(permit) => {
                let second_offset = start.elapsed().as_secs();
                {
                    let mut g = agg.lock().unwrap();
                    g.record_scheduled();
                    g.record_started_at(second_offset);
                }
                let client_c = client.clone();
                let dataset_c = dataset.clone();
                let agg_c = agg.clone();
                let start_c = start.clone();
                tokio::spawn(async move {
                    let item = dataset_c.next().await;
                    let second_offset = start_c.elapsed().as_secs();
                    let m = client_c
                        .send(&item.prompt, item.estimated_prompt_tokens)
                        .await;
                    let mut g = agg_c.lock().unwrap();
                    g.record_completed(&m, second_offset);
                    drop(g);
                    drop(permit);
                });
            }
            Err(_) => {
                let mut g = agg.lock().unwrap();
                g.record_skipped();
            }
        }
    }
}

/// Periodically snapshot the aggregator and write it to `path`. Only used
/// by the soak pattern. The task exits promptly when `cancel` fires.
async fn soak_checkpoint_loop(
    agg: Arc<Mutex<MetricsAggregator>>,
    path: PathBuf,
    interval: Duration,
    cancel: Arc<Notify>,
) {
    loop {
        tokio::select! {
            biased;
            _ = cancel.notified() => break,
            _ = sleep(interval) => {}
        }
        // Snapshot under the lock, then release before writing so we
        // don't hold the mutex across an I/O call.
        let snapshot = {
            let g = agg.lock().unwrap();
            crate::runner::aggregator_to_json(&g)
        };
        if let Ok(text) = serde_json::to_string_pretty(&snapshot) {
            if let Err(e) = std::fs::write(&path, &text) {
                tracing::warn!(
                    "soak checkpoint: failed to write {}: {e}",
                    path.display()
                );
            } else {
                tracing::info!("soak checkpoint: wrote {}", path.display());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration as StdDuration;
    use tempfile::TempDir;

    /// The soak checkpoint loop must (a) exit promptly when cancelled, and
    /// (b) write at least one snapshot to `path` while running.
    #[tokio::test(start_paused = false)]
    async fn soak_checkpoint_writes_file_and_exits_on_cancel() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("metrics.json");
        let agg: Arc<Mutex<MetricsAggregator>> = Arc::new(Mutex::new(MetricsAggregator::new()));
        let cancel = Arc::new(Notify::new());

        let h_cancel = cancel.clone();
        let h_path = path.clone();
        let h_agg = agg.clone();
        let handle = tokio::spawn(async move {
            soak_checkpoint_loop(h_agg, h_path, StdDuration::from_millis(20), h_cancel).await;
        });

        // Let the loop write a couple of checkpoints.
        tokio::time::sleep(StdDuration::from_millis(75)).await;
        cancel.notify_waiters();
        let _ = tokio::time::timeout(StdDuration::from_secs(1), handle).await;

        assert!(path.exists(), "checkpoint file should exist at {}", path.display());
        let text = std::fs::read_to_string(&path).unwrap();
        let v: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        // The empty aggregator still has a well-formed shape.
        assert!(v.get("total_requests").is_some());
        assert!(v.get("scheduled").is_some());
    }
}
