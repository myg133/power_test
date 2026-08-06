//! Chart data preparation. The actual rendering is done in the browser
//! via Chart.js; this module just shapes the metrics into JSON-friendly
//! values for the HTML report.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::runner::{HistKind, MetricsAggregator};

#[derive(Debug, Serialize)]
pub struct LatencyPercentiles {
    pub labels: Vec<String>,
    pub values_ms: Vec<f64>,
}

#[derive(Debug, Serialize)]
pub struct StatusBreakdown {
    pub labels: Vec<String>,
    pub counts: Vec<u64>,
}

#[derive(Debug, Serialize)]
pub struct RpsTimeline {
    pub seconds: Vec<u64>,
    pub started: Vec<u64>,
    pub completed: Vec<u64>,
}

pub fn latency_percentiles(agg: &MetricsAggregator) -> LatencyPercentiles {
    let pcts = [50.0, 90.0, 99.0, 99.9];
    let labels: Vec<String> = pcts.iter().map(|p| format!("p{p}")).collect();
    let values_ms: Vec<f64> = pcts
        .iter()
        .map(|p| {
            agg.percentile(HistKind::Latency, *p)
                .map(|us| us as f64 / 1000.0)
                .unwrap_or(0.0)
        })
        .collect();
    LatencyPercentiles { labels, values_ms }
}

pub fn status_breakdown(agg: &MetricsAggregator) -> StatusBreakdown {
    let mut map: BTreeMap<u16, u64> = BTreeMap::new();
    for (k, v) in agg.status_codes() {
        map.insert(*k, *v);
    }
    let labels: Vec<String> = map.keys().map(|c| c.to_string()).collect();
    let counts: Vec<u64> = map.values().copied().collect();
    StatusBreakdown { labels, counts }
}

pub fn rps_timeline(agg: &MetricsAggregator) -> RpsTimeline {
    let mut keys: Vec<u64> = agg
        .per_second_started()
        .keys()
        .chain(agg.per_second_completed().keys())
        .copied()
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let started: Vec<u64> = keys
        .iter()
        .map(|k| *agg.per_second_started().get(k).unwrap_or(&0))
        .collect();
    let completed: Vec<u64> = keys
        .iter()
        .map(|k| *agg.per_second_completed().get(k).unwrap_or(&0))
        .collect();
    RpsTimeline {
        seconds: keys,
        started,
        completed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn empty_charts() {
        let agg = MetricsAggregator::new();
        let lp = latency_percentiles(&agg);
        assert_eq!(lp.labels.len(), 4);
        assert_eq!(lp.values_ms.iter().all(|v| *v == 0.0), true);
        let sb = status_breakdown(&agg);
        assert!(sb.labels.is_empty());
        let rt = rps_timeline(&agg);
        assert!(rt.seconds.is_empty());
    }

    #[test]
    fn populated_charts() {
        let mut agg = MetricsAggregator::new();
        let mut m = crate::client::RequestMetrics::default();
        m.status = 200;
        m.total_duration = Duration::from_millis(50);
        m.completion_tokens = 4;
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let lp = latency_percentiles(&agg);
        assert_eq!(lp.values_ms.len(), 4);
        let sb = status_breakdown(&agg);
        assert_eq!(sb.labels, vec!["200"]);
        assert_eq!(sb.counts, vec![1]);
    }
}
