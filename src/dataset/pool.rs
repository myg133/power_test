//! Multi-prompt dataset with random or round-robin selection over a
//! pre-loaded `Vec<DatasetItem>`. Wraps [`crate::dataset::builtin`],
//! [`crate::dataset::sharegpt`], and [`crate::dataset::custom`].
//!
//! Selection is purely about which prompt to send next — the dataset does
//! not know or care about the RPS, the pattern, or the duration. The
//! `mode` field is forwarded from the loader so the executor can pick
//! the right dispatch path (single / static_multi / dynamic_multi).

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use async_trait::async_trait;

use crate::config::RequestStrategy;

use super::{Dataset, DatasetItem, DatasetMode};

/// A tiny xorshift64 PRNG. Avoids pulling in the `rand` crate for one
/// method. The state is `u64` and we never need cryptographic strength
/// here — load test random selection is the opposite of adversarial.
#[derive(Debug)]
struct XorShift64(u64);

impl XorShift64 {
    fn new(seed: u64) -> Self {
        // Avoid the zero fixed point.
        let seed = if seed == 0 { 0x9E3779B97F4A7C15 } else { seed };
        Self(seed)
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// Returns a value in `[0, n)`. `n` must be > 0.
    fn next_below(&mut self, n: usize) -> usize {
        debug_assert!(n > 0);
        (self.next_u64() as usize) % n
    }
}

pub struct PoolDataset {
    items: Vec<DatasetItem>,
    strategy: RequestStrategy,
    mode: DatasetMode,
    counter: AtomicUsize,
    rng: Mutex<XorShift64>,
}

impl PoolDataset {
    pub fn new(items: Vec<DatasetItem>, strategy: RequestStrategy, mode: DatasetMode) -> Self {
        // Seed from current nanoseconds so successive runs vary.
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0xDEADBEEF);
        Self {
            items,
            strategy,
            mode,
            counter: AtomicUsize::new(0),
            rng: Mutex::new(XorShift64::new(seed)),
        }
    }
}

#[async_trait]
impl Dataset for PoolDataset {
    async fn next(&self) -> DatasetItem {
        let n = self.items.len();
        if n == 0 {
            // Defensive fallback — a populated pool should never be empty,
            // but if some upstream filter drops everything, return a
            // benign placeholder rather than panic.
            return DatasetItem {
                prompt: "hello".into(),
                estimated_prompt_tokens: 1,
                weight: None,
                tags: Vec::new(),
                name: None,
                messages: None,
                follow_ups: Vec::new(),
            };
        }
        let idx = match self.strategy {
            RequestStrategy::RoundRobin => {
                let i = self.counter.fetch_add(1, Ordering::Relaxed) % n;
                i
            }
            RequestStrategy::Random => {
                let mut g = self.rng.lock().expect("rng mutex poisoned");
                g.next_below(n)
            }
        };
        self.items[idx].clone()
    }

    fn mode(&self) -> DatasetMode {
        self.mode
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::estimate_tokens;

    fn items(n: usize) -> Vec<DatasetItem> {
        (0..n)
            .map(|i| DatasetItem {
                prompt: format!("prompt-{i}"),
                estimated_prompt_tokens: estimate_tokens(&format!("prompt-{i}")),
                weight: None,
                tags: Vec::new(),
                name: None,
                messages: None,
                follow_ups: Vec::new(),
            })
            .collect()
    }

    #[tokio::test]
    async fn round_robin_visits_all_in_order() {
        let d = PoolDataset::new(items(3), RequestStrategy::RoundRobin, DatasetMode::Single);
        let a = d.next().await.prompt;
        let b = d.next().await.prompt;
        let c = d.next().await.prompt;
        let e = d.next().await.prompt; // wrap
        assert_eq!(a, "prompt-0");
        assert_eq!(b, "prompt-1");
        assert_eq!(c, "prompt-2");
        assert_eq!(e, "prompt-0");
    }

    #[tokio::test]
    async fn random_eventually_visits_all() {
        let d = PoolDataset::new(items(4), RequestStrategy::Random, DatasetMode::Single);
        let mut seen = [false; 4];
        for _ in 0..200 {
            let p = d.next().await.prompt;
            let idx: usize = p.trim_start_matches("prompt-").parse().unwrap();
            seen[idx] = true;
        }
        assert!(seen.iter().all(|x| *x), "expected all 4 items in 200 draws, got {seen:?}");
    }

    #[tokio::test]
    async fn empty_pool_returns_fallback() {
        let d = PoolDataset::new(vec![], RequestStrategy::Random, DatasetMode::Single);
        let item = d.next().await;
        assert!(!item.prompt.is_empty());
    }

    #[test]
    fn mode_is_forwarded() {
        let d = PoolDataset::new(items(1), RequestStrategy::RoundRobin, DatasetMode::DynamicMulti);
        assert_eq!(d.mode(), DatasetMode::DynamicMulti);
    }
}
