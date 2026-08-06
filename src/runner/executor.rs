//! The test executor. Owns the semaphore, the load pattern, and the metrics
//! aggregator. Spawns one worker task per acquired permit.
//!
//! M6 adds a mode-aware dispatch on top of the existing single-turn
//! path. The dispatch is keyed on `dataset.mode()`:
//!
//! - `Single`        → `client.send(prompt, …)` (M1-M4 behavior).
//! - `StaticMulti`   → `client.send_messages(seed_messages, …)` once
//!                     per item. No session.
//! - `DynamicMulti`  → session pool. Each item is a chain of
//!                     serial turns inside its own session. K =
//!                     `--concurrency`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, Semaphore};
use tokio::time::sleep;

use super::metrics::{CompletionContext, MetricsAggregator};
use super::pattern::LoadPattern as DynLoadPattern;
use super::session::{SessionPool, TurnAction};
use crate::client::{self, LlmClient};
use crate::config::{LoadPattern, RunConfig};
use crate::dataset::{self, Dataset, DatasetMode, OwnedChatMessage};
use crate::error::{Error, Result};
use crate::storage::run_dir;

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
    // M6f: history is grouped by model so the directory listing
    // is self-documenting. `run_dir` sanitizes the model name
    // for filesystem use; the original string lives in the
    // saved config.json for accurate reporting.
    let history_dir = run_dir(&opts.history_dir, &cfg.model, &cfg.run_id);
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

    // M6: pick the dispatch mode up front. The dataset's `mode()`
    // is determined at load time (TOML profile fail-fast ensures
    // one file = one mode). The session pool is only created for
    // `DynamicMulti`; for the other two modes, no extra state.
    let mode = dataset.mode();
    let session_pool: Option<Arc<SessionPool>> = match mode {
        DatasetMode::DynamicMulti => Some(Arc::new(SessionPool::new(cfg.concurrency))),
        _ => None,
    };

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
        let session_pool = session_pool.clone();
        tokio::spawn(async move {
            scheduler_loop(
                pattern,
                semaphore,
                cfg,
                agg,
                start,
                client,
                dataset,
                mode,
                session_pool,
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
    mode: DatasetMode,
    session_pool: Option<Arc<SessionPool>>,
    cancel: Arc<Notify>,
) {
    let duration = Duration::from_secs(cfg.duration_secs);
    let mut guard = pattern.lock().await;
    loop {
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
                let pool_c = session_pool.clone();
                tokio::spawn(async move {
                    let item = dataset_c.next().await;
                    let second_offset = start_c.elapsed().as_secs();
                    match mode {
                        DatasetMode::Single => {
                            let m = client_c
                                .send(&item.prompt, item.estimated_prompt_tokens)
                                .await;
                            let mut g = agg_c.lock().unwrap();
                            g.record_completed(&m, second_offset, &CompletionContext::none());
                            drop(g);
                        }
                        DatasetMode::StaticMulti => {
                            // M6: send the full messages array once. The
                            // dataset loader guarantees item.messages is
                            // Some(_).
                            let messages = item.messages.clone().unwrap_or_default();
                            let m = client_c
                                .send_messages(&messages, item.estimated_prompt_tokens)
                                .await;
                            let mut g = agg_c.lock().unwrap();
                            g.record_completed(&m, second_offset, &CompletionContext::none());
                            drop(g);
                        }
                        DatasetMode::DynamicMulti => {
                            // M6: serial-turn session. We hold the
                            // semaphore permit for the whole session so
                            // concurrency caps both the number of
                            // in-flight HTTP calls AND the number of
                            // parallel sessions — they're 1:1 here.
                            let pool = pool_c.expect(
                                "DynamicMulti mode requires a SessionPool",
                            );
                            run_dynamic_session(client_c, item, pool, agg_c, second_offset).await;
                        }
                    }
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

/// Run a single `DynamicMulti` item: serial turns, with the
/// session kept in the pool between turns. The semaphore permit
/// is held for the whole session (so concurrent sessions =
/// `cfg.concurrency`).
async fn run_dynamic_session(
    client: Arc<dyn LlmClient>,
    item: crate::dataset::DatasetItem,
    pool: Arc<SessionPool>,
    agg: Arc<Mutex<MetricsAggregator>>,
    first_second_offset: u64,
) {
    let Some(handle) = pool.acquire_evict_lru(&item) else {
        // Pool full and LRU eviction didn't fit (shouldn't happen
        // with `acquire_evict_lru` which always returns Some if
        // max_sessions > 0). Treat as skipped.
        let mut g = agg.lock().unwrap();
        g.record_skipped();
        return;
    };
    let session_id = {
        let snap = pool.snapshot();
        snap.last()
            .map(|s| s.id.clone())
            .unwrap_or_default()
    };
    let mut turn: u32 = 1;
    let second_offset = first_second_offset;
    let total_turns = item.follow_ups.len() + 1;
    loop {
        // Build the messages we're about to send. On turn 1 it's
        // the seed; on later turns, append the next follow_up.
        let mut messages = handle.messages();
        if turn > 1 {
            // follow_ups is 0-indexed; turn 2 uses follow_ups[0].
            let idx = (turn - 2) as usize;
            if let Some(next) = item.follow_ups.get(idx) {
                messages.push(OwnedChatMessage::new("user", next.clone()));
            }
        }
        let continuation = turn > 1;
        let m = client
            .send_messages(&messages, item.estimated_prompt_tokens)
            .await;
        let ctx = CompletionContext::turn(session_id.clone(), turn, continuation);
        // We just need a `u64` second offset for the aggregator;
        // reuse the first_second_offset captured before the
        // session loop. (Per-second bucketing across turns within
        // a session is not informative; the run-level metrics are
        // the focus.)
        {
            let mut g = agg.lock().unwrap();
            g.record_completed(&m, second_offset, &ctx);
        }
        if !m.is_ok() {
            // Drop the session and stop the run. The pool's
            // `acquire_evict_lru` policy means the slot is freed
            // and could be reused by a new item — but for this
            // item, the conversation is over.
            let mut g = agg.lock().unwrap();
            g.record_session_finished(true);
            handle.drop_session();
            return;
        }
        // M6d: pull the joined assistant text out of the request
        // metrics. For OpenAI/Anthropic streaming, this is the
        // concatenation of all `delta.content` (or `text_delta.text`)
        // chunks. For non-streaming, it's the body's content[0].text
        // (Anthropic) or choices[0].message.content (OpenAI). For
        // `RawClient`, the field stays empty — raw HTTP has no JSON
        // to read and that mode is single-turn anyway.
        let assistant = m.response_text.clone();
        let follow_ups_remaining = (turn as usize) < total_turns;
        let result = handle.complete(assistant, follow_ups_remaining);
        if result.action == TurnAction::Done {
            let mut g = agg.lock().unwrap();
            g.record_session_finished(false);
            return;
        }
        turn += 1;
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
