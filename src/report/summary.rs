//! Plain-text summary, suitable for stdout or the `summary.txt` artifact.

use std::fmt::Write as _;

use crate::config::RunConfig;
use crate::runner::{HistKind, MetricsAggregator};

/// Format a text summary for the run. Multi-section, human-readable.
pub fn render_summary(cfg: &RunConfig, agg: &MetricsAggregator, interrupted: bool) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "power_test summary — run {}", cfg.run_id);
    let _ = writeln!(s, "{}", "=".repeat(60));
    let _ = writeln!(
        s,
        "ttft-includes-thinking: true   # reasoning/thinking deltas count as tokens"
    );
    if let Some(tag) = &cfg.tag {
        let _ = writeln!(s, "tag:     {}", tag);
    }
    let _ = writeln!(s, "started: {}", cfg.started_at.to_rfc3339());
    let _ = writeln!(s, "target:  {}", cfg.target);
    let _ = writeln!(s, "model:   {}", cfg.model);
    if let Some(alias) = &cfg.model_alias {
        let _ = writeln!(s, "alias:   {}", alias);
    }
    let _ = writeln!(s, "api:     {}", cfg.api.as_str());
    let _ = writeln!(
        s,
        "load:    rps={} duration={}s concurrency={} stream={}",
        cfg.target_rps, cfg.duration_secs, cfg.concurrency, cfg.stream
    );
    let _ = writeln!(s, "pattern: {} ({})", cfg.pattern.name(), format_pattern_detail(cfg));
    let _ = writeln!(
        s,
        "dataset: {} ({}) · strategy={} · {} prompts (min {} / mean {:.1} / max {})",
        cfg.dataset.name(),
        format_dataset_detail(cfg),
        cfg.strategy.as_str(),
        cfg.prompt_distribution.count,
        cfg.prompt_distribution.min,
        cfg.prompt_distribution.mean,
        cfg.prompt_distribution.max,
    );
    if let Some(t) = &cfg.tag {
        let _ = writeln!(s, "tag:     {}", t);
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "results");
    let _ = writeln!(s, "-------");
    let _ = writeln!(s, "scheduled:        {}", agg.scheduled());
    let _ = writeln!(s, "completed:        {}", agg.total_requests());
    let _ = writeln!(s, "  success:       {}", agg.success_count());
    let _ = writeln!(s, "  error:         {}", agg.error_count());
    let _ = writeln!(s, "skipped ticks:    {}", agg.skipped());
    let achieved = achieved_rps(agg, cfg.duration_secs);
    let _ = writeln!(
        s,
        "achieved rps:     {:.2} (target {:.2})",
        achieved, cfg.target_rps
    );
    let _ = writeln!(s, "total tokens:     {}", agg.total_completion_tokens());
    if interrupted {
        let _ = writeln!(s, "interrupted:      yes");
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "latency (ms)");
    let _ = writeln!(s, "-----------");
    for p in [50.0, 90.0, 99.0, 99.9] {
        if let Some(us) = agg.percentile(HistKind::Latency, p) {
            let _ = writeln!(s, "  p{:>4}: {:>10.2}", format!("{p}"), us as f64 / 1000.0);
        } else {
            let _ = writeln!(s, "  p{:>4}:   (no data)", format!("{p}"));
        }
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "ttft (ms)");
    let _ = writeln!(s, "---------");
    if let Some(us) = agg.percentile(HistKind::Ttft, 50.0) {
        let _ = writeln!(s, "  p50:  {:>10.2}", us as f64 / 1000.0);
    } else {
        let _ = writeln!(s, "  p50:    (no data)");
    }
    if let Some(us) = agg.percentile(HistKind::Ttft, 99.0) {
        let _ = writeln!(s, "  p99:  {:>10.2}", us as f64 / 1000.0);
    } else {
        let _ = writeln!(s, "  p99:    (no data)");
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "itl (ms)");
    let _ = writeln!(s, "--------");
    if let Some(mean) = agg.mean(HistKind::Itl) {
        let _ = writeln!(s, "  mean: {:>10.2}", mean / 1000.0);
    } else {
        let _ = writeln!(s, "  mean:   (no data)");
    }
    if let Some(us) = agg.percentile(HistKind::Itl, 99.0) {
        let _ = writeln!(s, "  p99:  {:>10.2}", us as f64 / 1000.0);
    } else {
        let _ = writeln!(s, "  p99:    (no data)");
    }
    let _ = writeln!(s);

    let _ = writeln!(s, "tps (tokens/sec)");
    let _ = writeln!(s, "----------------");
    let _ = writeln!(s, "  mean: {:>10.2}", agg.tps_mean());
    let _ = writeln!(s, "  p99:  {:>10.2}", agg.tps_percentile(99.0));
    let _ = writeln!(s);

    if !agg.status_codes().is_empty() {
        let _ = writeln!(s, "status codes");
        let _ = writeln!(s, "------------");
        let mut codes: Vec<_> = agg.status_codes().iter().collect();
        codes.sort_by_key(|(c, _)| *c);
        for (code, count) in codes {
            let _ = writeln!(s, "  {}: {}", code, count);
        }
        let _ = writeln!(s);
    }

    if !agg.error_messages().is_empty() {
        let _ = writeln!(s, "errors");
        let _ = writeln!(s, "------");
        let mut errs: Vec<_> = agg.error_messages().iter().collect();
        errs.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        for (msg, count) in errs {
            let _ = writeln!(s, "  [{}] {}", count, msg);
        }
        let _ = writeln!(s);
    }

    // M6e: prompt-cache section. Only emitted when the run saw
    // any cache data — otherwise a single-turn run with no
    // prompt-cache support would just print "0%" everywhere and
    // confuse the reader.
    let cache = agg.cache_stats();
    if cache.cache_creation_total > 0 || cache.cache_hit_total > 0 {
        let _ = writeln!(s, "prompt cache");
        let _ = writeln!(s, "------------");
        let _ = writeln!(
            s,
            "  hit rate:        {:>6.2}%   ({} / {} prompt tokens)",
            cache.rate_overall,
            cache.cache_hit_total,
            cache.prompt_turn1 + cache.prompt_turn2plus
        );
        let _ = writeln!(
            s,
            "    turn 1:        {:>6.2}%   ({} / {} prompt tokens)",
            cache.rate_turn1, cache.cache_hit_turn1, cache.prompt_turn1
        );
        let _ = writeln!(
            s,
            "    turn 2+:       {:>6.2}%   ({} / {} prompt tokens)",
            cache.rate_turn2plus, cache.cache_hit_turn2plus, cache.prompt_turn2plus
        );
        let _ = writeln!(
            s,
            "  cache creation:  {} tokens (turn 1: {} · turn 2+: {})",
            cache.cache_creation_total,
            cache.cache_creation_turn1,
            cache.cache_creation_turn2plus
        );
        let _ = writeln!(s);
    }

    s
}

fn achieved_rps(agg: &MetricsAggregator, duration_secs: u64) -> f64 {
    if duration_secs == 0 {
        return 0.0;
    }
    agg.total_requests() as f64 / duration_secs as f64
}

fn format_pattern_detail(cfg: &crate::config::RunConfig) -> String {
    use crate::config::LoadPattern;
    match &cfg.pattern {
        LoadPattern::Constant { rps } => format!("{rps:.2} rps"),
        LoadPattern::Ramp { start, end, duration_secs } => {
            format!("{start:.2} → {end:.2} rps over {duration_secs:.0}s")
        }
        LoadPattern::Spike { baseline, spikes } => {
            let mut s = format!("baseline {baseline:.2} rps, spikes:");
            for sp in spikes {
                let _ = std::fmt::Write::write_fmt(
                    &mut s,
                    format_args!(" t={:.0}s@{:.0}rps/{}s", sp.at_secs, sp.rps, sp.duration_secs),
                );
            }
            s
        }
        LoadPattern::Soak {
            rps,
            checkpoint_secs,
        } => format!("{rps:.2} rps · checkpoint every {checkpoint_secs}s"),
    }
}

fn format_dataset_detail(cfg: &crate::config::RunConfig) -> String {
    use crate::config::DatasetSpec;
    match &cfg.dataset {
        DatasetSpec::Literal { text } => {
            let preview: String = text.chars().take(40).collect();
            let suffix = if text.chars().count() > 40 { "..." } else { "" };
            format!("\"{preview}{suffix}\"")
        }
        DatasetSpec::TokenBudget { target_tokens } => format!("~{target_tokens} tokens"),
        DatasetSpec::Builtin => "hardcoded pool (12 prompts)".into(),
        DatasetSpec::ShareGpt { path } => path.display().to_string(),
        DatasetSpec::Custom { path } => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
        RunConfig,
    };
    use crate::runner::MetricsAggregator;
    use std::time::Duration;

    fn cfg() -> RunConfig {
        RunConfig {
            run_id: "r".into(),
            target: "http://x".into(),
            api: ApiKind::Openai,
            model: "m".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 1.0 },
            max_tokens: 8,
            stream: true,
            target_rps: 1.0,
            duration_secs: 2,
            concurrency: 4,
            tag: None,
            api_key: None,
            started_at: chrono::Local::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
        }
    }

    #[test]
    fn summary_contains_key_sections() {
        let agg = MetricsAggregator::new();
        let text = render_summary(&cfg(), &agg, false);
        assert!(text.contains("power_test summary"));
        assert!(text.contains("latency (ms)"));
        assert!(text.contains("tps (tokens/sec)"));
    }

    #[test]
    fn summary_with_no_data_doesnt_panic() {
        let agg = MetricsAggregator::new();
        let _ = render_summary(&cfg(), &agg, true); // interrupted = true
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn summary_with_data() {
        let mut agg = MetricsAggregator::new();
        let mut m = crate::client::RequestMetrics::default();
        m.status = 200;
        m.total_duration = Duration::from_millis(50);
        m.ttft = Some(Duration::from_millis(20));
        m.completion_tokens = 5;
        agg.record_completed(&m, 0, &crate::runner::metrics::CompletionContext::none());
        let text = render_summary(&cfg(), &agg, false);
        assert!(text.contains("p50"));
        assert!(text.contains("tps"));
    }

    #[test]
    fn summary_shows_pattern_and_dataset_lines() {
        let mut c = cfg();
        c.pattern = LoadPattern::Spike {
            baseline: 3.0,
            spikes: vec![crate::config::SpikeSpec {
                at_secs: 5.0,
                rps: 50.0,
                duration_secs: 2.0,
            }],
        };
        c.dataset = DatasetSpec::Builtin;
        c.strategy = RequestStrategy::RoundRobin;
        c.prompt_distribution = PromptDistribution {
            count: 12,
            min: 1,
            max: 200,
            mean: 50.0,
        };
        let text = render_summary(&c, &MetricsAggregator::new(), false);
        assert!(text.contains("pattern: spike"));
        assert!(text.contains("dataset: built-in"));
        assert!(text.contains("strategy=round-robin"));
        assert!(text.contains("12 prompts"));
    }

    /// M6e: a single-turn run with no cache data must NOT emit
    /// a "prompt cache" section. Otherwise every run would
    /// print a confusing 0% line.
    #[test]
    fn summary_omits_cache_section_when_no_data() {
        let text = render_summary(&cfg(), &MetricsAggregator::new(), false);
        assert!(
            !text.contains("prompt cache"),
            "summary should omit cache section when no cache data was observed"
        );
    }

    /// M6e: a run with cache data should emit the cache section
    /// with overall / turn 1 / turn 2+ rates and totals.
    #[test]
    fn summary_shows_cache_section_when_data_present() {
        let mut agg = MetricsAggregator::new();
        // Two-turn session: turn 1 misses, turn 2 hits.
        for turn in 1u32..=2 {
            let m = crate::client::RequestMetrics {
                status: 200,
                prompt_tokens: 100,
                cache_creation_input_tokens: if turn == 1 { 100 } else { 0 },
                cache_hit_input_tokens: if turn == 1 { 0 } else { 100 },
                ..Default::default()
            };
            agg.record_completed(
                &m,
                0,
                &crate::runner::CompletionContext::turn("s", turn, turn > 1),
            );
        }
        let text = render_summary(&cfg(), &agg, false);
        assert!(text.contains("prompt cache"), "section header missing");
        assert!(text.contains("hit rate:"), "overall rate line missing");
        assert!(text.contains("turn 1:"), "turn-1 line missing");
        assert!(text.contains("turn 2+:"), "turn-2+ line missing");
        assert!(text.contains("cache creation:"), "cache creation line missing");
    }
}
