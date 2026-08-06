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
    /// `session_turn_total` is the sum of per-session turn counts.
    session_turn_total: u64,
    /// `session_dropped` is the number of sessions that bailed out
    /// early because a turn returned non-2xx or empty assistant text.
    session_dropped: u64,
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
        // M6 session bookkeeping.
        if rec.session_id.is_some() {
            self.session_turn_total += 1;
        }
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
        if rec.session_id.is_some() {
            self.session_turn_total += 1;
        }
        self.per_request.push(rec);
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
    /// is the sum of per-session turn counts (excluding dropped
    /// sessions). `session_dropped` is the number of sessions that
    /// bailed out early because a turn failed.
    pub fn session_stats(&self) -> (u64, u64, u64) {
        (self.session_count, self.session_turn_total, self.session_dropped)
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

/// Serialize an aggregator to the JSON shape used in `metrics.json`.
pub fn aggregator_to_json(agg: &MetricsAggregator) -> serde_json::Value {
    let per_request: Vec<&RequestRecord> = agg.per_request().iter().collect();
    let (session_count, session_turn_total, session_dropped) = agg.session_stats();
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
        };
        assert!((rec.tps() - 20.0).abs() < 0.01);
    }
}
