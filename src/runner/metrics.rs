//! Per-request metrics records and an aggregator that turns them into
//! run-level statistics.

use std::collections::HashMap;

use hdrhistogram::Histogram;
use serde::{Deserialize, Serialize};

use crate::client::RequestMetrics;

/// Context for a completed request. The session fields are `None` for
/// single-turn runs and for the first turn of a multi-turn session.
#[derive(Debug, Clone, Default)]
pub struct CompletionContext {
    /// Stable session id (UUID). `None` for single-turn requests.
    pub session_id: Option<String>,
    /// 1-indexed turn number within the session. `None` for
    /// single-turn requests.
    pub session_turn: Option<u32>,
    /// `true` when this turn's request was preceded by at least one
    /// assistant turn on the same session. The seed turn and
    /// single-turn requests are `false`.
    pub session_continuation: bool,
}

impl CompletionContext {
    pub fn none() -> Self {
        Self::default()
    }

    pub fn turn(
        session_id: impl Into<String>,
        turn: u32,
        continuation: bool,
    ) -> Self {
        Self {
            session_id: Some(session_id.into()),
            session_turn: Some(turn),
            session_continuation: continuation,
        }
    }
}

/// One row in `metrics.json`. Fields are stored in microseconds for
/// resolution and as plain `u32`/`u64` for JSON friendliness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestRecord {
    pub started_at: chrono::DateTime<chrono::Utc>,
    pub finished_at: chrono::DateTime<chrono::Utc>,
    pub status: u16,
    pub error: Option<String>,
    /// End-to-end latency in microseconds. Always set.
    pub total_duration_us: u64,
    /// Time to first content token in microseconds. `None` for non-streaming
    /// or if no tokens were produced.
    pub ttft_us: Option<u64>,
    /// Inter-token latencies in microseconds.
    pub itl_samples_us: Vec<u64>,
    pub completion_tokens: u32,
    pub prompt_tokens: u32,
    /// `true` when `completion_tokens` was chunk-count estimated.
    pub estimated: bool,
    /// M6 session bookkeeping. `None` / `false` for single-turn runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_turn: Option<u32>,
    #[serde(default)]
    pub session_continuation: bool,
    /// M6e: tokens the model wrote to its prefix cache on this
    /// request. Anthropic only; OpenAI leaves 0. `#[serde(default)]`
    /// so old metrics.json files (pre-M6e) still deserialize.
    #[serde(default)]
    pub cache_creation_input_tokens: u32,
    /// M6e: prompt tokens served from the prefix cache. Anthropic
    /// reads this from `usage.cache_read_input_tokens`; OpenAI
    /// reads it from `usage.prompt_tokens_details.cached_tokens`.
    #[serde(default)]
    pub cache_hit_input_tokens: u32,
    /// M8: total tokens decoded (including speculatively rejected
    /// ones) in this request. Zero unless the server exposes
    /// speculative decoding stats (e.g. vLLM's
    /// usage.completion_tokens_details.accepted_prediction_tokens
    /// or Anthropic's usage.cache_creation).
    #[serde(default)]
    pub spec_decoded_tok: u32,
    /// M8: tokens actually accepted from the speculative draft.
    /// Used to compute the speculative acceptance rate.
    #[serde(default)]
    pub spec_accepted_tok: u32,
    /// M8: number of decode iterations (forward passes) used
    /// to produce the response. Used to compute decoded-tokens
    /// per iteration.
    #[serde(default)]
    pub spec_iterations: u32,
}

impl RequestRecord {
    pub fn from_metrics(m: &RequestMetrics, ctx: &CompletionContext) -> Self {
        Self {
            started_at: m.started_at,
            finished_at: m.finished_at,
            status: m.status,
            error: m.error.clone(),
            total_duration_us: m.total_duration.as_micros() as u64,
            ttft_us: m.ttft.map(|d| d.as_micros() as u64),
            itl_samples_us: m
                .itl_samples
                .iter()
                .map(|d| d.as_micros() as u64)
                .collect(),
            completion_tokens: m.completion_tokens,
            prompt_tokens: m.prompt_tokens,
            estimated: m.estimated,
            session_id: ctx.session_id.clone(),
            session_turn: ctx.session_turn,
            session_continuation: ctx.session_continuation,
            cache_creation_input_tokens: m.cache_creation_input_tokens,
            cache_hit_input_tokens: m.cache_hit_input_tokens,
            spec_decoded_tok: m.spec_decoded_tok,
            spec_accepted_tok: m.spec_accepted_tok,
            spec_iterations: m.spec_iterations,
        }
    }

    pub fn tps(&self) -> f64 {
        let secs = self.total_duration_us as f64 / 1_000_000.0;
        if secs > 0.0 && self.completion_tokens > 0 {
            self.completion_tokens as f64 / secs
        } else {
            0.0
        }
    }
}

/// Aggregator for a run. Histograms are configured for microsecond values
/// from 1µs to 60s with 3 significant digits of precision (≈0.1% error).
pub struct MetricsAggregator {
    latency: Histogram<u64>,
    ttft: Histogram<u64>,
    itl: Histogram<u64>,
    tps_values: Vec<f64>,
    success_count: u64,
    error_count: u64,
    status_codes: HashMap<u16, u64>,
    /// Number of requests started in each wall-clock second since run start.
    per_second_started: HashMap<u64, u64>,
    /// Number of requests completed in each wall-clock second since run start.
    per_second_completed: HashMap<u64, u64>,
    /// Unique error messages with counts.
    error_messages: HashMap<String, u64>,
    /// Per-request records, in completion order.
    per_request: Vec<RequestRecord>,
    /// Wall-clock start of the run.
    run_started_at: chrono::DateTime<chrono::Utc>,
    /// Total scheduled (target) requests.
    scheduled: u64,
    /// Total skipped ticks (semaphore exhausted).
    skipped: u64,
    /// M6 session bookkeeping. `session_count` is the number of
    /// distinct sessions that completed at least one turn.
    session_count: u64,
    /// `session_turn_total` is the total number of turn-completions
    /// across the run. Each `record_completed` call counts as 1
    /// turn, so a 2-turn multi-turn session contributes 2 and a
    /// single-turn request contributes 1. The M8 metric
    /// `avg_turns_per_request` divides this by `total_requests`,
    /// which is why a single-turn run reads as 1.0 and a 2-turn
    /// run reads as 2.0.
    session_turn_total: u64,
    /// `session_dropped` is the number of sessions that bailed out
    /// early because a turn returned non-2xx or empty assistant text.
    session_dropped: u64,
    /// M6e: prompt-cache accounting. We track totals across the
    /// whole run plus per-turn buckets (turn 1 vs turn 2+). The
    /// per-turn split is what makes a multi-turn session useful:
    /// turn 1 is always a miss (it pays the cache_creation cost or
    /// is fully uncached), turn 2+ is the interesting one.
    cache_creation_total: u64,
    cache_hit_total: u64,
    prompt_for_cache_rate: u64,
    cache_hit_turn1: u64,
    prompt_turn1: u64,
    cache_hit_turn2plus: u64,
    prompt_turn2plus: u64,
    cache_creation_turn1: u64,
    cache_creation_turn2plus: u64,
}

impl MetricsAggregator {
    pub fn new() -> Self {
        Self {
            latency: Histogram::new_with_bounds(1, 60_000_000, 3)
                .expect("latency histogram bounds"),
            ttft: Histogram::new_with_bounds(1, 60_000_000, 3)
                .expect("ttft histogram bounds"),
            itl: Histogram::new_with_bounds(1, 60_000_000, 3)
                .expect("itl histogram bounds"),
            tps_values: Vec::new(),
            success_count: 0,
            error_count: 0,
            status_codes: HashMap::new(),
            per_second_started: HashMap::new(),
            per_second_completed: HashMap::new(),
            error_messages: HashMap::new(),
            per_request: Vec::new(),
            run_started_at: chrono::Utc::now(),
            scheduled: 0,
            skipped: 0,
            session_count: 0,
            session_turn_total: 0,
            session_dropped: 0,
            cache_creation_total: 0,
            cache_hit_total: 0,
            prompt_for_cache_rate: 0,
            cache_hit_turn1: 0,
            prompt_turn1: 0,
            cache_hit_turn2plus: 0,
            prompt_turn2plus: 0,
            cache_creation_turn1: 0,
            cache_creation_turn2plus: 0,
        }
    }

    pub fn run_started_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.run_started_at
    }

    /// Set the wall-clock start time. Called by the executor before any
    /// requests are launched.
    pub fn set_run_started_at(&mut self, dt: chrono::DateTime<chrono::Utc>) {
        self.run_started_at = dt;
    }

    /// Build a fresh aggregator from a list of per-request records. Used by
    /// the `report` command to re-render from saved `metrics.json`.
    pub fn from_records(records: &[RequestRecord]) -> Self {
        let mut agg = Self::new();
        for r in records {
            agg.push_record_clone(r);
        }
        agg
    }

    /// Append a previously-built record. Used to clone data out of another
    /// aggregator (see executor drain).
    pub fn push_record_clone(&mut self, rec: &RequestRecord) {
        let _ = self.latency.record(rec.total_duration_us);
        if let Some(ttft) = rec.ttft_us {
            let _ = self.ttft.record(ttft);
        }
        for s in &rec.itl_samples_us {
            let _ = self.itl.record(*s);
        }
        let tps = if rec.total_duration_us > 0 && rec.completion_tokens > 0 {
            rec.completion_tokens as f64 / (rec.total_duration_us as f64 / 1_000_000.0)
        } else {
            0.0
        };
        if tps > 0.0 {
            self.tps_values.push(tps);
        }
        if rec.error.is_some() || rec.status == 0 || rec.status >= 400 {
            self.error_count += 1;
            if let Some(err) = &rec.error {
                *self
                    .error_messages
                    .entry(truncate_msg(err, 200))
                    .or_insert(0) += 1;
            } else if rec.status >= 400 {
                *self
                    .error_messages
                    .entry(format!("HTTP {}", rec.status))
                    .or_insert(0) += 1;
            }
        } else {
            self.success_count += 1;
        }
        *self.status_codes.entry(rec.status).or_insert(0) += 1;
        // M6 session bookkeeping: every record counts as 1
        // turn. Single-turn requests contribute 1 turn, each
        // turn of a multi-turn session contributes 1, and
        // `avg_turns_per_request` divides by `total_requests`.
        self.session_turn_total += 1;
        // M6e: cache aggregation. Single-turn records land in the
        // turn-1 bucket by default (no per-turn distinction in
        // single-turn mode; the renderer treats the overall rate
        // as the headline number and turn 1 as a fallback).
        self.accumulate_cache(rec, rec.session_turn.unwrap_or(1));
        self.per_request.push(rec.clone());
    }

    /// Record a session transitioning to terminal state. Called by
    /// the executor when a session is done (last turn) or dropped
    /// mid-way. Per-turn counts are accumulated separately by
    /// [`Self::record_completed`] when the `session_id` field is
    /// set, so this method only tracks session-level events.
    pub fn record_session_finished(&mut self, dropped: bool) {
        self.session_count += 1;
        if dropped {
            self.session_dropped += 1;
        }
    }

    pub fn record_scheduled(&mut self) {
        self.scheduled += 1;
    }

    pub fn record_skipped(&mut self) {
        self.skipped += 1;
    }

    pub fn record_started_at(&mut self, second_offset: u64) {
        *self.per_second_started.entry(second_offset).or_insert(0) += 1;
    }

    pub fn record_completed(
        &mut self,
        metrics: &RequestMetrics,
        second_offset: u64,
        ctx: &CompletionContext,
    ) {
        let rec = RequestRecord::from_metrics(metrics, ctx);
        let _ = self.latency.record(rec.total_duration_us);
        if let Some(ttft) = rec.ttft_us {
            let _ = self.ttft.record(ttft);
        }
        for s in &rec.itl_samples_us {
            let _ = self.itl.record(*s);
        }
        let tps = rec.tps();
        if tps > 0.0 {
            self.tps_values.push(tps);
        }
        if metrics.error.is_some() || metrics.status == 0 || metrics.status >= 400 {
            self.error_count += 1;
            if let Some(err) = &metrics.error {
                *self.error_messages.entry(truncate_msg(err, 200)).or_insert(0) += 1;
            } else if metrics.status >= 400 {
                *self
                    .error_messages
                    .entry(format!("HTTP {}", metrics.status))
                    .or_insert(0) += 1;
            }
        } else {
            self.success_count += 1;
        }
        *self.status_codes.entry(metrics.status).or_insert(0) += 1;
        *self.per_second_completed.entry(second_offset).or_insert(0) += 1;
        // M6 session bookkeeping: every record counts as 1
        // turn (see the same comment on the `push_record_clone`
        // branch above).
        self.session_turn_total += 1;
        // M6e: cache aggregation. `turn_for_cache` is the turn
        // number (1 for seed, 2+ for continuations) or 1 for any
        // single-turn request.
        let turn_for_cache = rec.session_turn.unwrap_or(1);
        self.accumulate_cache(&rec, turn_for_cache);
        self.per_request.push(rec);
    }

    /// M6e: accumulate per-request cache stats into the right
    /// buckets. `turn` is 1 for the first turn of a session (or
    /// any single-turn request) and 2+ for continuations. Prompt
    /// tokens count toward the rate denominator only when the
    /// request actually saw prompt tokens (errors, empty
    /// bodies, and 0-token estimates don't pollute the rate).
    fn accumulate_cache(&mut self, rec: &RequestRecord, turn: u32) {
        self.cache_creation_total += rec.cache_creation_input_tokens as u64;
        self.cache_hit_total += rec.cache_hit_input_tokens as u64;
        if rec.prompt_tokens > 0 {
            self.prompt_for_cache_rate += rec.prompt_tokens as u64;
            if turn <= 1 {
                self.cache_hit_turn1 += rec.cache_hit_input_tokens as u64;
                self.prompt_turn1 += rec.prompt_tokens as u64;
                self.cache_creation_turn1 +=
                    rec.cache_creation_input_tokens as u64;
            } else {
                self.cache_hit_turn2plus += rec.cache_hit_input_tokens as u64;
                self.prompt_turn2plus += rec.prompt_tokens as u64;
                self.cache_creation_turn2plus +=
                    rec.cache_creation_input_tokens as u64;
            }
        }
    }

    pub fn per_request(&self) -> &[RequestRecord] {
        &self.per_request
    }

    pub fn percentile(&self, which: HistKind, p: f64) -> Option<u64> {
        let h = match which {
            HistKind::Latency => &self.latency,
            HistKind::Ttft => &self.ttft,
            HistKind::Itl => &self.itl,
        };
        if h.is_empty() {
            None
        } else {
            h.value_at_percentile(p).into()
        }
    }

    pub fn mean(&self, which: HistKind) -> Option<f64> {
        let h = match which {
            HistKind::Latency => &self.latency,
            HistKind::Ttft => &self.ttft,
            HistKind::Itl => &self.itl,
        };
        if h.is_empty() {
            None
        } else {
            Some(h.mean())
        }
    }

    pub fn tps_mean(&self) -> f64 {
        if self.tps_values.is_empty() {
            0.0
        } else {
            self.tps_values.iter().sum::<f64>() / self.tps_values.len() as f64
        }
    }

    pub fn tps_percentile(&self, p: f64) -> f64 {
        if self.tps_values.is_empty() {
            return 0.0;
        }
        let mut sorted = self.tps_values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((p / 100.0) * (sorted.len() as f64 - 1.0)).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    }

    pub fn success_count(&self) -> u64 {
        self.success_count
    }

    pub fn error_count(&self) -> u64 {
        self.error_count
    }

    pub fn total_requests(&self) -> u64 {
        self.success_count + self.error_count
    }

    pub fn scheduled(&self) -> u64 {
        self.scheduled
    }

    pub fn skipped(&self) -> u64 {
        self.skipped
    }

    pub fn status_codes(&self) -> &HashMap<u16, u64> {
        &self.status_codes
    }

    pub fn error_messages(&self) -> &HashMap<String, u64> {
        &self.error_messages
    }

    pub fn per_second_completed(&self) -> &HashMap<u64, u64> {
        &self.per_second_completed
    }

    pub fn per_second_started(&self) -> &HashMap<u64, u64> {
        &self.per_second_started
    }

    /// M6 session stats. Returns `(session_count, session_turn_total,
    /// session_dropped)`. `session_count` is the number of distinct
    /// sessions that completed at least one turn. `session_turn_total`
    /// is the total number of turn-completions across the run —
    /// every `record_completed` call counts as 1 turn (single-turn
    /// or multi-turn), so the value equals `total_requests` for a
    /// single-turn run and grows with multi-turn continuation
    /// turns. `session_dropped` is the number of sessions that
    /// bailed out early because a turn failed.
    pub fn session_stats(&self) -> (u64, u64, u64) {
        (self.session_count, self.session_turn_total, self.session_dropped)
    }

    /// M6e: prompt-cache stats. All rates are `hit / prompt`,
    /// expressed as percentages (0.0–100.0). When no cache data
    /// was seen, all rates are 0.0 and `cache_creation_total` /
    /// `cache_hit_total` are both 0. Callers can detect "no data"
    /// by checking `cache_creation_total + cache_hit_total == 0`.
    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            cache_creation_total: self.cache_creation_total,
            cache_hit_total: self.cache_hit_total,
            rate_overall: rate_pct(self.cache_hit_total, self.prompt_for_cache_rate),
            rate_turn1: rate_pct(self.cache_hit_turn1, self.prompt_turn1),
            rate_turn2plus: rate_pct(self.cache_hit_turn2plus, self.prompt_turn2plus),
            cache_creation_turn1: self.cache_creation_turn1,
            cache_creation_turn2plus: self.cache_creation_turn2plus,
            prompt_turn1: self.prompt_turn1,
            prompt_turn2plus: self.prompt_turn2plus,
            cache_hit_turn1: self.cache_hit_turn1,
            cache_hit_turn2plus: self.cache_hit_turn2plus,
        }
    }

    pub fn total_completion_tokens(&self) -> u64 {
        self.per_request
            .iter()
            .map(|r| r.completion_tokens as u64)
            .sum()
    }

    pub fn total_prompt_tokens(&self) -> u64 {
        self.per_request.iter().map(|r| r.prompt_tokens as u64).sum()
    }

    /// M8: total streaming time across successful requests,
    /// measured in seconds. The streaming time is the
    /// total_duration_us of each request (which already
    /// includes the TTFT and all inter-token latencies).
    pub fn total_streaming_time_secs(&self) -> f64 {
        self.per_request
            .iter()
            .filter(|r| r.status >= 200 && r.status < 300)
            .map(|r| r.total_duration_us as f64 / 1_000_000.0)
            .sum()
    }

    /// M8: TPOT (Time Per Output Token) in milliseconds, averaged
    /// across all requests. Computed as
    /// (sum of (latency - ttft) for requests with >= 2 tokens) /
    /// (sum of (tokens - 1) for those requests). The -1
    /// accounts for the first token being already accounted
    /// for by the TTFT.
    pub fn tpot_ms(&self) -> f64 {
        let mut total_gen_us: u64 = 0;
        let mut total_step_tokens: u64 = 0;
        for r in &self.per_request {
            if r.completion_tokens < 2 {
                continue;
            }
            let ttft = r.ttft_us.unwrap_or(0);
            if (r.total_duration_us as u128) < (ttft as u128) {
                continue;
            }
            total_gen_us = total_gen_us.saturating_add(
                r.total_duration_us.saturating_sub(ttft),
            );
            total_step_tokens = total_step_tokens
                .saturating_add(r.completion_tokens.saturating_sub(1) as u64);
        }
        if total_step_tokens == 0 {
            0.0
        } else {
            (total_gen_us as f64 / 1000.0) / total_step_tokens as f64
        }
    }

    /// M8: system-wide output throughput in tokens per second.
    /// total_completion_tokens / total_streaming_time.
    /// Returns 0 if there was no streaming time.
    pub fn output_throughput_tps(&self) -> f64 {
        let secs = self.total_streaming_time_secs();
        if secs > 0.0 {
            self.total_completion_tokens() as f64 / secs
        } else {
            0.0
        }
    }

    /// M8: system-wide total throughput (input + output) in
    /// tokens per second.
    pub fn total_throughput_tps(&self) -> f64 {
        let secs = self.total_streaming_time_secs();
        if secs > 0.0 {
            (self.total_completion_tokens() + self.total_prompt_tokens()) as f64
                / secs
        } else {
            0.0
        }
    }

    /// M8: average number of session turns per request. For
    /// single-turn runs this is 1.0 (every request has one
    /// turn). For multi-turn it can be > 1.
    pub fn avg_turns_per_request(&self) -> f64 {
        let total = self.total_requests() as f64;
        if total > 0.0 {
            self.session_turn_total as f64 / total
        } else {
            0.0
        }
    }

    /// M8: speculative decoding stats. Sum across all requests,
    /// exposed as (decoded_per_iter, accept_rate).
    /// decoded_per_iter is the average number of tokens produced
    /// per decode iteration (sum_spec_decoded / sum_spec_iterations).
    /// accept_rate is the fraction of drafted tokens that were
    /// accepted (sum_spec_accepted / sum_spec_decoded).
    /// Returns (0.0, 0.0) when no speculative fields are populated.
    pub fn speculative_stats(&self) -> (f64, f64) {
        let mut total_decoded: u64 = 0;
        let mut total_iterations: u64 = 0;
        let mut total_accepted: u64 = 0;
        for r in &self.per_request {
            total_decoded = total_decoded
                .saturating_add(r.spec_decoded_tok as u64);
            total_iterations = total_iterations
                .saturating_add(r.spec_iterations as u64);
            total_accepted = total_accepted
                .saturating_add(r.spec_accepted_tok as u64);
        }
        let decoded_per_iter = if total_iterations > 0 {
            total_decoded as f64 / total_iterations as f64
        } else {
            0.0
        };
        let accept_rate = if total_decoded > 0 {
            100.0 * total_accepted as f64 / total_decoded as f64
        } else {
            0.0
        };
        (decoded_per_iter, accept_rate)
    }

    /// M6h: copy the run-level counters that
    /// `push_record_clone` does NOT already cover, from
    /// `other` into `self`. Used by the executor's
    /// `Arc::try_unwrap` fallback path: when the executor
    /// can't take ownership of the aggregator (e.g. the TUI
    /// holds a shared reference), it builds a fresh
    /// aggregator and replays per-request records via
    /// `push_record_clone`. That replay covers latency /
    /// TTFT / ITL / TPS / cache / status_codes /
    /// `session_turn_total` (via `accumulate_cache` for the
    /// cache fields and the per-record branch for
    /// `session_turn_total`).
    ///
    /// What `push_record_clone` does NOT cover, and what
    /// `merge_counters_from` must therefore add:
    ///
    /// - `scheduled` — incremented by `record_scheduled` (one
    ///   per scheduler tick), never by `push_record_clone`.
    /// - `skipped` — incremented by `record_skipped` when
    ///   the semaphore is exhausted; not per-request.
    /// - `session_count` / `session_dropped` — incremented
    ///   by `record_session_finished` (one per session
    ///   lifetime, not per turn).
    ///
    /// Before this helper existed, a real run reported
    /// `scheduled: 0` / `session_count: 0` in
    /// `metrics.json` even when the run actually scheduled 60
    /// ticks and completed 8 sessions.
    pub fn merge_counters_from(&mut self, other: &Self) {
        self.scheduled += other.scheduled;
        self.skipped += other.skipped;
        self.session_count += other.session_count;
        self.session_dropped += other.session_dropped;
    }
}

impl Default for MetricsAggregator {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy)]
pub enum HistKind {
    Latency,
    Ttft,
    Itl,
}

/// M6e: prompt-cache hit rate snapshot, computed once per report.
/// `rate_overall` is the headline number; `rate_turn1` and
/// `rate_turn2plus` split by session turn so multi-turn runs
/// surface "the seed request misses but the continuation
/// requests hit at 99%" without further work by the reader.
#[derive(Debug, Clone, Copy)]
pub struct CacheStats {
    /// Total prompt tokens the model wrote to its prefix cache
    /// across the whole run.
    pub cache_creation_total: u64,
    /// Total prompt tokens served from the prefix cache.
    pub cache_hit_total: u64,
    /// `100 * cache_hit_total / total_prompt_tokens`. 0.0 when
    /// no prompt tokens were seen.
    pub rate_overall: f64,
    /// Cache hit rate on the first turn of a session (or any
    /// single-turn request).
    pub rate_turn1: f64,
    /// Cache hit rate on continuation turns (turn 2+). For pure
    /// single-turn runs, this is 0.0 because no continuations
    /// were observed.
    pub rate_turn2plus: f64,
    /// Tokens the model wrote to cache on turn 1. Almost always
    /// positive on the first turn of a long-prefix session.
    pub cache_creation_turn1: u64,
    pub cache_creation_turn2plus: u64,
    /// Prompt tokens (denominator) seen on turn 1. Useful for
    /// the summary line "5 / 100 prompt tokens".
    pub prompt_turn1: u64,
    /// Prompt tokens (denominator) seen on turn 2+.
    pub prompt_turn2plus: u64,
    /// Cache hit tokens (numerator) on turn 1.
    pub cache_hit_turn1: u64,
    /// Cache hit tokens (numerator) on turn 2+.
    pub cache_hit_turn2plus: u64,
}

fn truncate_msg(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

fn rate_pct(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        100.0 * numerator as f64 / denominator as f64
    }
}

/// Serialize an aggregator to the JSON shape used in `metrics.json`.
pub fn aggregator_to_json(agg: &MetricsAggregator) -> serde_json::Value {
    let per_request: Vec<&RequestRecord> = agg.per_request().iter().collect();
    let (session_count, session_turn_total, session_dropped) = agg.session_stats();
    let cache = agg.cache_stats();
    let (spec_decoded_per_iter, spec_accept_rate) = agg.speculative_stats();
    serde_json::json!({
        "run_started_at": agg.run_started_at(),
        "scheduled": agg.scheduled(),
        "skipped": agg.skipped(),
        "total_requests": agg.total_requests(),
        "success_count": agg.success_count(),
        "error_count": agg.error_count(),
        "total_completion_tokens": agg.total_completion_tokens(),
        "total_prompt_tokens": agg.total_prompt_tokens(),
        "status_codes": agg.status_codes(),
        "error_messages": agg.error_messages(),
        "per_second_completed": agg.per_second_completed(),
        "per_second_started": agg.per_second_started(),
        "session_count": session_count,
        "session_turn_total": session_turn_total,
        "session_dropped": session_dropped,
        // M8: derived metrics
        "avg_latency_ms": if agg.total_requests() > 0 {
            agg.per_request.iter().map(|r| r.total_duration_us as f64).sum::<f64>()
                / (agg.total_requests() as f64 * 1000.0)
        } else { 0.0 },
        "tpot_ms": agg.tpot_ms(),
        "output_throughput_tps": agg.output_throughput_tps(),
        "total_throughput_tps": agg.total_throughput_tps(),
        "avg_turns_per_request": agg.avg_turns_per_request(),
        "spec_decoded_per_iter": spec_decoded_per_iter,
        "spec_accept_rate": spec_accept_rate,
        "cache": {
            "creation_total": cache.cache_creation_total,
            "hit_total": cache.cache_hit_total,
            "rate_overall_pct": cache.rate_overall,
            "rate_turn1_pct": cache.rate_turn1,
            "rate_turn2plus_pct": cache.rate_turn2plus,
            "creation_turn1": cache.cache_creation_turn1,
            "creation_turn2plus": cache.cache_creation_turn2plus,
        },
        "per_request": per_request,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn dummy_metrics(us: u64, ttft: Option<u64>, itl: Vec<u64>, tokens: u32) -> RequestMetrics {
        RequestMetrics {
            status: 200,
            error: None,
            ttft: ttft.map(Duration::from_micros),
            itl_samples: itl.into_iter().map(Duration::from_micros).collect(),
            completion_tokens: tokens,
            prompt_tokens: 10,
            total_duration: Duration::from_micros(us),
            estimated: false,
            response_text: String::new(),
            cache_creation_input_tokens: 0,
            cache_hit_input_tokens: 0,
            spec_decoded_tok: 0,
            spec_accepted_tok: 0,
            spec_iterations: 0,
            response_id: None,
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn histogram_percentile_math() {
        let mut agg = MetricsAggregator::new();
        for i in 1..=100 {
            let m = dummy_metrics(i * 1000, Some(i * 1000), vec![], 10);
            agg.record_completed(&m, 0, &CompletionContext::none());
        }
        let p50 = agg.percentile(HistKind::Latency, 50.0).unwrap();
        let p99 = agg.percentile(HistKind::Latency, 99.0).unwrap();
        // hdrhistogram has ~0.1% precision, allow 10% tolerance
        assert!((50_000..=60_000).contains(&p50), "p50 = {}", p50);
        assert!((95_000..=110_000).contains(&p99), "p99 = {}", p99);
    }

    #[test]
    fn empty_histogram_percentile_is_none() {
        let agg = MetricsAggregator::new();
        assert_eq!(agg.percentile(HistKind::Latency, 50.0), None);
    }

    #[test]
    fn error_count_and_messages() {
        let mut agg = MetricsAggregator::new();
        let mut err = RequestMetrics::default();
        err.status = 500;
        err.error = Some("oops".into());
        agg.record_completed(&err, 0, &CompletionContext::none());
        let mut ok = RequestMetrics::default();
        ok.status = 200;
        agg.record_completed(&ok, 0, &CompletionContext::none());
        assert_eq!(agg.error_count(), 1);
        assert_eq!(agg.success_count(), 1);
        assert_eq!(agg.error_messages().get("oops"), Some(&1));
    }

    #[test]
    fn tps_calculation() {
        let rec = RequestRecord {
            started_at: chrono::Utc::now(),
            finished_at: chrono::Utc::now(),
            status: 200,
            error: None,
            total_duration_us: 1_000_000, // 1 second
            ttft_us: Some(100_000),
            itl_samples_us: vec![],
            completion_tokens: 20,
            prompt_tokens: 5,
            estimated: false,
            session_id: None,
            session_turn: None,
            session_continuation: false,
            cache_creation_input_tokens: 0,
            cache_hit_input_tokens: 0,
            spec_decoded_tok: 0,
            spec_accepted_tok: 0,
            spec_iterations: 0,
        };
        assert!((rec.tps() - 20.0).abs() < 0.01);
    }

    /// M6e: an empty aggregator has zero rates and zero totals.
    #[test]
    fn cache_stats_empty_aggregator_is_zero() {
        let agg = MetricsAggregator::new();
        let c = agg.cache_stats();
        assert_eq!(c.cache_creation_total, 0);
        assert_eq!(c.cache_hit_total, 0);
        assert_eq!(c.rate_overall, 0.0);
        assert_eq!(c.rate_turn1, 0.0);
        assert_eq!(c.rate_turn2plus, 0.0);
    }

    /// M6e: a single-turn request with cache data lands in the
    /// overall and turn-1 buckets but NOT turn-2+.
    #[test]
    fn cache_stats_single_turn_lands_in_turn1_bucket() {
        let mut agg = MetricsAggregator::new();
        let mut m = RequestMetrics {
            prompt_tokens: 100,
            cache_creation_input_tokens: 80,
            cache_hit_input_tokens: 0,
            ..RequestMetrics::default()
        };
        m.status = 200;
        agg.record_completed(&m, 0, &CompletionContext::none());
        let c = agg.cache_stats();
        assert_eq!(c.cache_creation_total, 80);
        assert_eq!(c.cache_hit_total, 0);
        // turn 1 sees all 100 prompt tokens but 0 hits → 0%
        assert_eq!(c.rate_turn1, 0.0);
        // No turn-2+ requests were observed → 0%
        assert_eq!(c.rate_turn2plus, 0.0);
        assert_eq!(c.rate_overall, 0.0);
    }

    /// M6e: a 2-turn session with turn 1 = miss and turn 2 = hit
    /// should produce ~50% overall, 0% turn 1, 100% turn 2+.
    #[test]
    fn cache_stats_two_turn_session_splits_by_turn() {
        let mut agg = MetricsAggregator::new();
        // Turn 1: 100 prompt tokens, 0 cache hit, 100 cache_creation.
        let mut m1 = RequestMetrics {
            prompt_tokens: 100,
            cache_creation_input_tokens: 100,
            cache_hit_input_tokens: 0,
            ..RequestMetrics::default()
        };
        m1.status = 200;
        let ctx1 = CompletionContext::turn("s1", 1, false);
        agg.record_completed(&m1, 0, &ctx1);
        // Turn 2: 100 prompt tokens, 100 cache hit, 0 cache_creation.
        let mut m2 = RequestMetrics {
            prompt_tokens: 100,
            cache_creation_input_tokens: 0,
            cache_hit_input_tokens: 100,
            ..RequestMetrics::default()
        };
        m2.status = 200;
        let ctx2 = CompletionContext::turn("s1", 2, true);
        agg.record_completed(&m2, 0, &ctx2);
        // Mark the session as finished so session_count reflects it
        // (cache tests don't care, but we don't want to leak).
        agg.record_session_finished(false);

        let c = agg.cache_stats();
        assert_eq!(c.cache_creation_total, 100);
        assert_eq!(c.cache_hit_total, 100);
        // 100 / 200 = 50%
        assert!((c.rate_overall - 50.0).abs() < 1e-6, "got {}", c.rate_overall);
        assert_eq!(c.rate_turn1, 0.0);
        // 100 / 100 = 100%
        assert!((c.rate_turn2plus - 100.0).abs() < 1e-6, "got {}", c.rate_turn2plus);
        // creation split: 100 on turn 1, 0 on turn 2+
        assert_eq!(c.cache_creation_turn1, 100);
        assert_eq!(c.cache_creation_turn2plus, 0);
        // hit split: 0 on turn 1, 100 on turn 2+
        assert_eq!(c.cache_hit_turn1, 0);
        assert_eq!(c.cache_hit_turn2plus, 100);
    }

    /// M6e: a request with `prompt_tokens == 0` (e.g. an error
    /// or an HTTP failure) must NOT contribute to the rate
    /// denominator. Otherwise a flood of 0-token errors would
    /// skew the cache hit rate.
    #[test]
    fn cache_stats_zero_prompt_tokens_does_not_pollute_rate() {
        let mut agg = MetricsAggregator::new();
        let mut m = RequestMetrics {
            prompt_tokens: 0,
            cache_hit_input_tokens: 0,
            ..RequestMetrics::default()
        };
        m.status = 500;
        m.error = Some("oops".into());
        agg.record_completed(&m, 0, &CompletionContext::none());
        let c = agg.cache_stats();
        // The error request has 0 prompt and 0 hit; it should not
        // contribute to the denominator.
        assert_eq!(c.rate_overall, 0.0);
        assert_eq!(c.prompt_turn1, 0);
        assert_eq!(c.prompt_turn2plus, 0);
    }

    /// M6e: reloading from a `RequestRecord` set (the
    /// `from_records` path used by `compare` and `report` from
    /// disk) must re-aggregate cache stats correctly. This is
    /// the only path that calls `push_record_clone`.
    #[test]
    fn cache_stats_from_records_replays_per_turn_buckets() {
        // Simulate "save then reload": build the records, then
        // rebuild an aggregator from them.
        let mut saved = Vec::new();
        for turn in 1u32..=2 {
            let m = RequestMetrics {
                prompt_tokens: 100,
                cache_creation_input_tokens: if turn == 1 { 100 } else { 0 },
                cache_hit_input_tokens: if turn == 1 { 0 } else { 100 },
                status: 200,
                ..RequestMetrics::default()
            };
            let rec = RequestRecord::from_metrics(
                &m,
                &CompletionContext::turn("s1", turn, turn > 1),
            );
            saved.push(rec);
        }
        let reloaded = MetricsAggregator::from_records(&saved);
        let c = reloaded.cache_stats();
        assert_eq!(c.cache_creation_total, 100);
        assert_eq!(c.cache_hit_total, 100);
        assert_eq!(c.cache_creation_turn1, 100);
        assert_eq!(c.cache_creation_turn2plus, 0);
        assert_eq!(c.cache_hit_turn1, 0);
        assert_eq!(c.cache_hit_turn2plus, 100);
    }

    /// M6h: `merge_counters_from` is the executor's fallback
    /// path. Without it, a real run reported
    /// `scheduled: 0` / `session_count: 0` in `metrics.json`
    /// even when the run actually scheduled 60 ticks and
    /// completed 8 sessions. The fallback builds a fresh
    /// aggregator from per-request records (which already
    /// cover latency / TTFT / ITL / TPS / cache / status_codes
    /// via `push_record_clone`) and then has to layer the
    /// bookkeeping counters on top.
    #[test]
    fn merge_counters_from_carries_over_bookkeeping() {
        // Build a "real" aggregator with non-zero counters.
        let mut src = MetricsAggregator::new();
        src.record_scheduled();
        src.record_scheduled();
        src.record_skipped();
        src.record_session_finished(false);
        src.record_session_finished(true);
        let mut m = dummy_metrics(500_000, Some(100_000), vec![], 5);
        m.prompt_tokens = 100;
        m.cache_creation_input_tokens = 50;
        m.cache_hit_input_tokens = 30;
        src.record_completed(&m, 0, &CompletionContext::turn("s1", 1, false));
        let mut m2 = dummy_metrics(600_000, Some(110_000), vec![], 6);
        m2.prompt_tokens = 200;
        m2.cache_hit_input_tokens = 180;
        src.record_completed(&m2, 0, &CompletionContext::turn("s1", 2, true));

        // Build a fresh aggregator from the same per-request
        // records (this is the executor's fallback path).
        let mut fresh = MetricsAggregator::new();
        for r in src.per_request() {
            fresh.push_record_clone(r);
        }
        // Before merge: counters are all 0, but per-request
        // data is intact.
        assert_eq!(fresh.scheduled, 0);
        assert_eq!(fresh.session_count, 0);
        assert_eq!(fresh.cache_creation_total, 50);
        assert_eq!(fresh.cache_hit_total, 210);

        // After merge: the bookkeeping counters that
        // `push_record_clone` does NOT cover are carried over.
        fresh.merge_counters_from(&src);
        assert_eq!(fresh.scheduled, 2);
        assert_eq!(fresh.skipped, 1);
        assert_eq!(fresh.session_count, 2);
        assert_eq!(fresh.session_dropped, 1);
        // session_turn_total and cache_* are already at the
        // correct values from push_record_clone — the merge
        // must not double-count them.
        assert_eq!(fresh.session_turn_total, 2);
        assert_eq!(fresh.cache_creation_total, 50);
        assert_eq!(fresh.cache_hit_total, 210);
    }

    // ---- M8 tests ----

    /// M8: TPOT for 2 requests, 100 output tokens each, 200ms
    /// total latency each with 50ms TTFT. Streaming time per
    /// request is 150ms. Per request TPOT = 150ms / 99 step
    /// tokens = ~1.515ms. Average over 2 requests = same.
    #[test]
    fn tpot_ms_average_over_requests() {
        let mut agg = MetricsAggregator::new();
        for _ in 0..2 {
            let m = RequestMetrics {
                status: 200,
                total_duration: Duration::from_millis(200),
                ttft: Some(Duration::from_millis(50)),
                completion_tokens: 100,
                prompt_tokens: 10,
                ..RequestMetrics::default()
            };
            agg.record_completed(&m, 0, &CompletionContext::none());
        }
        let tpot = agg.tpot_ms();
        assert!(tpot > 1.4 && tpot < 1.6, "tpot = {} (expected ~1.5)", tpot);
    }

    /// M8: TPOT is 0 when no request has 2+ completion tokens.
    #[test]
    fn tpot_ms_zero_for_short_responses() {
        let mut agg = MetricsAggregator::new();
        for _ in 0..3 {
            let m = RequestMetrics {
                status: 200,
                total_duration: Duration::from_millis(100),
                ttft: Some(Duration::from_millis(50)),
                completion_tokens: 1,
                ..RequestMetrics::default()
            };
            agg.record_completed(&m, 0, &CompletionContext::none());
        }
        assert_eq!(agg.tpot_ms(), 0.0);
    }

    /// M8: output_throughput = total_completion_tokens / time.
    #[test]
    fn output_throughput_matches_total_per_sec() {
        let mut agg = MetricsAggregator::new();
        let m = RequestMetrics {
            status: 200,
            total_duration: Duration::from_millis(1000),
            ttft: Some(Duration::from_millis(100)),
            completion_tokens: 1000,
            prompt_tokens: 100,
            ..RequestMetrics::default()
        };
        agg.record_completed(&m, 0, &CompletionContext::none());
        let t = agg.output_throughput_tps();
        assert!((t - 1000.0).abs() < 0.1, "got {} tok/s", t);
    }

    /// M8: total_throughput = (prompt + completion) / time.
    #[test]
    fn total_throughput_includes_prompt() {
        let mut agg = MetricsAggregator::new();
        let m = RequestMetrics {
            status: 200,
            total_duration: Duration::from_millis(1000),
            ttft: Some(Duration::from_millis(100)),
            completion_tokens: 500,
            prompt_tokens: 500,
            ..RequestMetrics::default()
        };
        agg.record_completed(&m, 0, &CompletionContext::none());
        let t = agg.total_throughput_tps();
        assert!((t - 1000.0).abs() < 0.1, "got {} tok/s", t);
    }

    /// M8: avg_turns_per_request = session_turn_total / total.
    #[test]
    fn avg_turns_per_request_basic() {
        let mut agg = MetricsAggregator::new();
        for _ in 0..3 {
            let m = RequestMetrics {
                status: 200,
                total_duration: Duration::from_millis(100),
                completion_tokens: 1,
                ..RequestMetrics::default()
            };
            agg.record_completed(&m, 0, &CompletionContext::none());
        }
        assert!((agg.avg_turns_per_request() - 1.0).abs() < 1e-9);

        for t in 1u32..=3 {
            let m = RequestMetrics {
                status: 200,
                total_duration: Duration::from_millis(100),
                completion_tokens: 5,
                ..RequestMetrics::default()
            };
            agg.record_completed(&m, 0, &CompletionContext::turn("s1", t, t > 1));
        }
        // 3 single + 3 multi = 6 reqs, 6 turns total => 1.0
        assert!((agg.avg_turns_per_request() - 1.0).abs() < 1e-9);
    }

    /// M8: speculative_stats returns (0, 0) when no spec data.
    #[test]
    fn speculative_stats_zero_by_default() {
        let agg = MetricsAggregator::new();
        let (d, a) = agg.speculative_stats();
        assert_eq!(d, 0.0);
        assert_eq!(a, 0.0);
    }

    /// M8: speculative_stats computes decoded/iter and accept rate.
    #[test]
    fn speculative_stats_with_data() {
        let mut agg = MetricsAggregator::new();
        let m = RequestMetrics {
            status: 200,
            total_duration: Duration::from_millis(100),
            completion_tokens: 10,
            spec_decoded_tok: 20,
            spec_accepted_tok: 8,
            spec_iterations: 4,
            ..RequestMetrics::default()
        };
        agg.record_completed(&m, 0, &CompletionContext::none());
        let (d, a) = agg.speculative_stats();
        assert!((d - 5.0).abs() < 1e-9, "got {}", d);
        assert!((a - 40.0).abs() < 1e-9, "got {}", a);
    }

}
