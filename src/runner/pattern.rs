//! Load patterns. M1 only had constant RPS; M2 adds ramp, spike, and soak.
//!
//! All patterns implement [`LoadPattern`]. The executor holds the pattern
//! behind an `Arc<tokio::sync::Mutex<Box<dyn LoadPattern>>>` and calls
//! `tick().await` to wait for the next request.

use std::time::Duration;

use async_trait::async_trait;
use tokio::time::{sleep, Instant as TokioInstant, MissedTickBehavior};

use crate::config::{LoadPattern as ConfigPattern, SpikeSpec};

/// Common interface for every M1 / M2 load pattern.
#[async_trait]
pub trait LoadPattern: Send {
    /// Block until the next request should be issued. Implementations are
    /// free to use a fixed period, a recomputed period, or any other
    /// scheduling strategy.
    async fn tick(&mut self);

    /// Current target RPS at the moment `tick` returned. For patterns
    /// with a variable rate (ramp / spike), this changes over time.
    fn current_rps(&self) -> f64;

    /// Peak RPS this pattern will ever produce. Used for default
    /// concurrency sizing and reporting.
    fn peak_rps(&self) -> f64;
}

// ---------------------------------------------------------------------------
// Constant
// ---------------------------------------------------------------------------

/// Constant-RPS pattern. Backed by `tokio::time::interval` with
/// `MissedTickBehavior::Skip` so a slow tick doesn't queue up work.
pub struct ConstantRps {
    interval: tokio::time::Interval,
    rps: f64,
}

impl ConstantRps {
    pub fn new(rps: f64) -> Self {
        let period = if rps > 0.0 {
            Duration::from_secs_f64(1.0 / rps)
        } else {
            Duration::from_secs(1)
        };
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(MissedTickBehavior::Skip);
        Self { interval, rps }
    }
}

#[async_trait]
impl LoadPattern for ConstantRps {
    async fn tick(&mut self) {
        self.interval.tick().await;
    }
    fn current_rps(&self) -> f64 {
        self.rps
    }
    fn peak_rps(&self) -> f64 {
        self.rps
    }
}

// ---------------------------------------------------------------------------
// Ramp
// ---------------------------------------------------------------------------

/// Linearly increase RPS from `start` to `end` over `duration`.
///
/// `current_rps = start + (end - start) * (elapsed / duration)`.
/// Each tick sleeps for `1 / current_rps` seconds, recomputed at every
/// tick so the curve stays smooth even as the rate changes.
pub struct RampPattern {
    start: f64,
    end: f64,
    duration: Duration,
    /// The instant the run started, captured when the pattern is created.
    started_at: TokioInstant,
    /// The instant the previous tick fired — used to compute the gap
    /// we need to sleep until the next one.
    next_due: TokioInstant,
    /// First tick fires immediately (mirrors `tokio::time::interval`).
    fired_first: bool,
}

impl RampPattern {
    pub fn new(start: f64, end: f64, duration_secs: f64) -> Self {
        let now = TokioInstant::now();
        Self {
            start,
            end,
            duration: Duration::from_secs_f64(duration_secs.max(0.0)),
            started_at: now,
            next_due: now,
            fired_first: false,
        }
    }

    fn current_rps_at(&self, now: TokioInstant) -> f64 {
        if self.duration.as_secs_f64() <= 0.0 {
            return self.end;
        }
        let elapsed = now.saturating_duration_since(self.started_at).as_secs_f64();
        let frac = (elapsed / self.duration.as_secs_f64()).clamp(0.0, 1.0);
        self.start + (self.end - self.start) * frac
    }
}

#[async_trait]
impl LoadPattern for RampPattern {
    async fn tick(&mut self) {
        let now = TokioInstant::now();
        if !self.fired_first {
            // Fire immediately on the first call (matches ConstantRps).
            self.fired_first = true;
            self.next_due = now;
            return;
        }
        // Sleep until next_due, then schedule the next one based on the
        // current RPS at the moment of firing.
        if now < self.next_due {
            sleep(self.next_due - now).await;
        }
        let fire_at = TokioInstant::now();
        let rps = self.current_rps_at(fire_at).max(0.001);
        let period = Duration::from_secs_f64(1.0 / rps);
        self.next_due = fire_at + period;
    }
    fn current_rps(&self) -> f64 {
        self.current_rps_at(TokioInstant::now())
    }
    fn peak_rps(&self) -> f64 {
        self.start.max(self.end)
    }
}

// ---------------------------------------------------------------------------
// Spike
// ---------------------------------------------------------------------------

/// Hold RPS at `baseline`, with one or more short bursts at the configured
/// `spikes`. Outside any spike the rate is `baseline`; inside a spike it
/// is the spike's RPS for the spike's duration.
pub struct SpikePattern {
    baseline: f64,
    spikes: Vec<SpikeSpec>,
    started_at: TokioInstant,
    next_due: TokioInstant,
    fired_first: bool,
}

impl SpikePattern {
    pub fn new(baseline: f64, spikes: Vec<SpikeSpec>) -> Self {
        let now = TokioInstant::now();
        Self {
            baseline,
            spikes,
            started_at: now,
            next_due: now,
            fired_first: false,
        }
    }

    fn current_rps_at(&self, now: TokioInstant) -> f64 {
        let elapsed = now.saturating_duration_since(self.started_at).as_secs_f64();
        for s in &self.spikes {
            if elapsed >= s.at_secs && elapsed < s.at_secs + s.duration_secs {
                return s.rps;
            }
        }
        self.baseline
    }
}

#[async_trait]
impl LoadPattern for SpikePattern {
    async fn tick(&mut self) {
        let now = TokioInstant::now();
        if !self.fired_first {
            self.fired_first = true;
            self.next_due = now;
            return;
        }
        if now < self.next_due {
            sleep(self.next_due - now).await;
        }
        let fire_at = TokioInstant::now();
        let rps = self.current_rps_at(fire_at).max(0.001);
        let period = Duration::from_secs_f64(1.0 / rps);
        self.next_due = fire_at + period;
    }
    fn current_rps(&self) -> f64 {
        self.current_rps_at(TokioInstant::now())
    }
    fn peak_rps(&self) -> f64 {
        self.spikes
            .iter()
            .map(|s| s.rps)
            .fold(self.baseline, f64::max)
    }
}

// ---------------------------------------------------------------------------
// Soak
// ---------------------------------------------------------------------------

/// Long-running constant-RPS pattern. The "soak" behavior — periodic
/// flushing of metrics to disk — is implemented in the executor, not
/// here; the pattern itself is just constant RPS.
pub struct SoakPattern {
    inner: ConstantRps,
}

impl SoakPattern {
    pub fn new(rps: f64) -> Self {
        Self {
            inner: ConstantRps::new(rps),
        }
    }
}

#[async_trait]
impl LoadPattern for SoakPattern {
    async fn tick(&mut self) {
        self.inner.tick().await;
    }
    fn current_rps(&self) -> f64 {
        self.inner.current_rps()
    }
    fn peak_rps(&self) -> f64 {
        self.inner.peak_rps()
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build the right pattern for a config [`ConfigPattern`].
pub fn from_config(cfg: &ConfigPattern) -> Box<dyn LoadPattern> {
    match cfg {
        ConfigPattern::Constant { rps } => Box::new(ConstantRps::new(*rps)),
        ConfigPattern::Ramp {
            start,
            end,
            duration_secs,
        } => Box::new(RampPattern::new(*start, *end, *duration_secs)),
        ConfigPattern::Spike { baseline, spikes } => {
            Box::new(SpikePattern::new(*baseline, spikes.clone()))
        }
        ConfigPattern::Soak { rps, .. } => Box::new(SoakPattern::new(*rps)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::time::Duration as TokioDuration;

    #[tokio::test(start_paused = true)]
    async fn constant_rps_ticks_at_expected_rate() {
        let mut p = ConstantRps::new(10.0); // 100ms period
        // First tick fires immediately.
        p.tick().await;
        let start = tokio::time::Instant::now();
        for _ in 0..9 {
            p.tick().await;
        }
        let elapsed = start.elapsed();
        assert!(
            (850..=1000).contains(&elapsed.as_millis()),
            "elapsed = {}ms",
            elapsed.as_millis()
        );
    }

    #[tokio::test(start_paused = true)]
    async fn rps_1_period_is_1s() {
        let mut p = ConstantRps::new(1.0);
        p.tick().await;
        let start = tokio::time::Instant::now();
        p.tick().await;
        let elapsed = start.elapsed();
        assert!(
            (900..=1100).contains(&elapsed.as_millis()),
            "elapsed = {}ms",
            elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn ramp_current_rps_increases_monotonically() {
        // Use a long duration so the rate changes slowly. We can sample
        // `current_rps` at a few `Instant`s without sleeping because the
        // math is deterministic.
        let p = RampPattern::new(2.0, 10.0, 10.0);
        let start = p.started_at;
        let samples: Vec<f64> = [0.0, 0.25, 0.5, 0.75, 1.0]
            .iter()
            .map(|f| p.current_rps_at(start + TokioDuration::from_secs_f64(10.0 * f)))
            .collect();
        // Strictly non-decreasing.
        for w in samples.windows(2) {
            assert!(
                w[1] >= w[0],
                "rps should be non-decreasing across the ramp; got {samples:?}"
            );
        }
        // Endpoints match.
        assert!((samples[0] - 2.0).abs() < 1e-6);
        assert!((samples[4] - 10.0).abs() < 1e-6);
        // Midpoint ≈ 6.0.
        assert!((samples[2] - 6.0).abs() < 1e-6, "midpoint = {}", samples[2]);
    }

    #[tokio::test]
    async fn ramp_peak_is_max_of_endpoints() {
        let p = RampPattern::new(2.0, 10.0, 30.0);
        assert!((p.peak_rps() - 10.0).abs() < 1e-9);
        let p = RampPattern::new(10.0, 2.0, 30.0);
        assert!((p.peak_rps() - 10.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn spike_current_rps_reflects_phase() {
        let p = SpikePattern::new(
            3.0,
            vec![SpikeSpec {
                at_secs: 5.0,
                rps: 50.0,
                duration_secs: 2.0,
            }],
        );
        let start = p.started_at;
        let baseline = p.current_rps_at(start);
        let mid_spike = p.current_rps_at(start + TokioDuration::from_secs_f64(6.0));
        let after_spike = p.current_rps_at(start + TokioDuration::from_secs_f64(8.0));
        assert!((baseline - 3.0).abs() < 1e-9, "baseline = {baseline}");
        assert!((mid_spike - 50.0).abs() < 1e-9, "mid_spike = {mid_spike}");
        assert!((after_spike - 3.0).abs() < 1e-9, "after_spike = {after_spike}");
    }

    #[tokio::test]
    async fn spike_peak_is_max_of_burst_and_baseline() {
        let p = SpikePattern::new(
            3.0,
            vec![SpikeSpec {
                at_secs: 0.0,
                rps: 20.0,
                duration_secs: 1.0,
            }],
        );
        assert!((p.peak_rps() - 20.0).abs() < 1e-9);

        // Baseline-only (no spikes) is still legal; peak = baseline.
        let p = SpikePattern::new(7.0, vec![]);
        assert!((p.peak_rps() - 7.0).abs() < 1e-9);
    }

    #[tokio::test(start_paused = true)]
    async fn ramp_tick_count_matches_elapsed_time() {
        // 2 rps for 5 seconds = 10 ticks (with the immediate first one).
        let mut p = RampPattern::new(2.0, 2.0, 5.0);
        p.tick().await; // immediate
        let start = tokio::time::Instant::now();
        let mut count = 1;
        loop {
            p.tick().await;
            count += 1;
            if start.elapsed() >= TokioDuration::from_millis(4500) {
                break;
            }
        }
        // We should have observed roughly 10 ticks (1 per 500ms).
        assert!(
            (8..=12).contains(&count),
            "expected ~10 ticks in 4.5s @ 2rps, got {count}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn soak_pattern_is_just_constant() {
        let mut p = SoakPattern::new(4.0); // 250ms period
        p.tick().await;
        let start = tokio::time::Instant::now();
        for _ in 0..3 {
            p.tick().await;
        }
        let elapsed = start.elapsed();
        // 3 ticks of 250ms = 750ms ± 20%.
        assert!(
            (600..=900).contains(&elapsed.as_millis()),
            "elapsed = {}ms",
            elapsed.as_millis()
        );
    }
}
