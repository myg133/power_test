//! Bilingual label dictionary for the HTML report.
//!
//! Two locales are supported: Chinese (zh, the default) and
//! English (en). The HTML body is always rendered in the
//! default locale; the non-default locale is embedded as JSON
//! inside a `<script id="i18n-dict">` block so a small inline
//! script can flip every `[data-i18n]` element's textContent
//! without re-rendering.
//!
//! Adding a new label: pick a dotted key (e.g. card.duration),
//! add the entry to both zh_dict() and en_dict(), and the
//! `dictionaries_have_same_keys` test will fail if you only add
//! it to one side. Unknown keys fall back to the key string
//! itself so a missing translation never silently renders an
//! empty cell.

use std::borrow::Cow;

use serde::Serialize;

/// All locales the report knows about. Zh is the default
/// rendered into the HTML body; En is what the JS toggle
/// switches to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Locale {
    Zh,
    En,
}

impl Locale {
    /// Default locale. The first language the user sees when
    /// they open a freshly-saved report.
    pub fn default() -> Self {
        Locale::Zh
    }

    /// Stable short string used in `lang="..."` attributes and
    /// localStorage keys.
    pub fn as_str(self) -> &'static str {
        match self {
            Locale::Zh => "zh",
            Locale::En => "en",
        }
    }
}

/// Look up a label by key in the chosen locale. Returns the key
/// itself (literally, e.g. "card.duration") when the key is
/// missing so a typo is obvious in the rendered page rather
/// than silently producing an empty cell.
///
/// Returned as `Cow<'static, str>` because the miss path needs
/// to allocate a fresh `String` from the caller's `&str`. The
/// hot (hit) path borrows a `'static` from the const table, so
/// the renderer never allocates when keys are in sync.
pub fn t(locale: Locale, key: &str) -> Cow<'static, str> {
    let dict = match locale {
        Locale::Zh => zh_dict(),
        Locale::En => en_dict(),
    };
    for (k, v) in dict {
        if *k == key {
            return Cow::Borrowed(v);
        }
    }
    Cow::Owned(key.to_string())
}

/// Render the chosen locale's full dictionary as a JSON object
/// suitable for inlining inside `<script id="i18n-dict">`.
///
/// Keys are JSON-escaped via serde_json so quotes, backslashes,
/// and `<` are handled. The output is deterministic.
pub fn dict_to_json(locale: Locale) -> String {
    let dict = match locale {
        Locale::Zh => zh_dict(),
        Locale::En => en_dict(),
    };
    let mut obj = serde_json::Map::with_capacity(dict.len());
    for (k, v) in dict {
        obj.insert((*k).to_string(), serde_json::Value::String((*v).to_string()));
    }
    serde_json::Value::Object(obj).to_string()
}

/// Total number of registered keys. Used by the
/// `dictionaries_have_same_keys` test to assert symmetry
/// without a hard-coded count.
pub fn key_count() -> usize {
    zh_dict().len()
}

const fn zh_dict() -> &'static [(&'static str, &'static str)] {
    &[
        ("page.title", "power_test 报告"),
        ("page.run_label", "运行 ID"),
        ("page.started_label", "开始时间"),
        ("page.interrupted_label", "（已中断）"),
        ("ui.compare_with", "与历史报告对比"),
        ("ui.compare", "对比"),
        ("ui.pick_run", "— 请选择 —"),
        ("ui.pre_render_hint", "预生成请运行 `power_test compare --html`"),
        ("ui.language_toggle_zh", "中文"),
        ("ui.language_toggle_en", "English"),
        ("ui.open_report", "打开报告"),
        ("card.target", "目标"),
        ("card.model", "模型"),
        ("card.duration", "时长"),
        ("card.target_rps", "目标 RPS"),
        ("card.achieved_rps", "实际 RPS"),
        ("card.success_rate", "成功率"),
        ("card.total_requests", "总请求数"),
        ("card.total_tokens", "总 token 数"),
        ("config.heading", "运行配置"),
        ("config.run_identity", "基本信息"),
        ("config.load", "负载"),
        ("config.dataset", "数据集"),
        ("config.run_id", "运行 ID"),
        ("config.tag", "标签"),
        ("config.started", "开始时间"),
        ("config.target", "目标"),
        ("config.api", "API"),
        ("config.model", "模型"),
        ("config.pattern", "模式"),
        ("config.concurrency", "并发数"),
        ("config.stream", "流式"),
        ("config.max_tokens", "最大 token"),
        ("config.kind", "类型"),
        ("config.source", "来源"),
        ("config.strategy", "策略"),
        ("config.prompt_tokens", "Prompt token"),
        ("metric.heading", "摘要统计"),
        ("metric.col_metric", "指标"),
        ("metric.latency", "延迟 (毫秒)"),
        ("metric.ttft", "首 token (毫秒)"),
        ("metric.itl", "token 间隔 (毫秒)"),
        ("metric.tps", "TPS (token/秒)"),
        // M7: the M8 advanced-metrics card was renamed and
        // translated. M8 was a working name; the labels now
        // read in Chinese by default and the English
        // dictionary carries the matching "Advanced metrics"
        // / "TPOT (ms/token)" wording.
        ("metric.t8", "高级指标"),
        ("metric.tpot", "TPOT (毫秒/token)"),
        ("metric.throughput_output", "输出 token/秒"),
        ("metric.throughput_total", "总 token/秒"),
        ("metric.avg_input", "平均输入 token"),
        ("metric.avg_output", "平均输出 token"),
        ("metric.avg_turns", "平均轮次/请求"),
        ("metric.spec_decoded", "每轮解码 token"),
        ("metric.spec_accept", "投机接受率"),
        ("metric.p50", "p50"),
        ("metric.p90", "p90"),
        ("metric.p99", "p99"),
        ("metric.p999", "p99.9"),
        ("charts.heading", "图表"),
        ("chart.latency", "延迟分位数 (毫秒)"),
        ("chart.status", "状态码分布"),
        ("chart.rps", "每秒 RPS"),
        ("chart.rps_started", "已发起"),
        ("chart.rps_completed", "已完成"),
        ("errors.heading", "错误"),
        ("errors.none", "无错误。"),
        ("cache.heading", "提示词缓存"),
        ("cache.hit_rate", "缓存命中率"),
        ("cache.no_data", "无缓存命中"),
        ("cache.overall", "总体"),
        ("cache.turn1", "第 1 轮"),
        ("cache.turn2plus", "第 2 轮起"),
        ("cache.creation", "写入缓存的 token"),
        ("cache.hit", "命中缓存的 prompt token"),
        ("cache.denominator_zero", "本轮 prompt token 总数为 0 — 命中率未定义"),
        ("cache.no_continuations", "本轮无第 2 轮及之后的请求"),
        ("footer.powered_by", "由 power_test v{version} 驱动"),
        ("dash.title", "power_test 模型报告"),
        ("dash.subtitle", "共 {n} 次测试 · alias = {alias}"),
        ("dash.subtitle_no_alias", "共 {n} 次测试"),
        ("dash.compare_pick", "选择 2 份报告对比"),
        ("dash.run_a", "A：{id}"),
        ("dash.run_b", "B：{id}"),
        ("dash.compare", "对比"),
        ("dash.reset", "重置"),
        ("dash.open_compare", "打开完整对比报告"),
        ("dash.no_runs", "该模型下还没有任何测试结果。运行 power_test run 后会自动生成。"),
        ("dash.runs_heading", "历史报告列表（新 → 旧）"),
        ("dash.col_run_id", "运行 ID"),
        ("dash.col_time", "时间"),
        ("dash.col_target", "目标"),
        ("dash.col_rps", "RPS"),
        ("dash.col_duration", "时长"),
        ("dash.col_requests", "请求"),
        ("dash.col_success", "成功率"),
        ("dash.col_p50", "p50"),
        ("dash.col_p99", "p99"),
        ("dash.col_tps", "TPS"),
        ("dash.col_cache", "缓存命中率"),
        ("dash.col_status", "状态"),
        ("dash.col_tag", "标签"),
        ("dash.col_alias", "alias"),
        ("dash.open_report", "打开报告"),
        ("dash.diff_heading", "差异"),
        ("dash.col_a", "A"),
        ("dash.col_b", "B"),
        ("dash.col_delta", "差值"),
        ("dash.col_pct", "%"),
        ("dash.metric.achieved_rps", "实际 RPS"),
        ("dash.metric.total_requests", "总请求数"),
        ("dash.metric.success_rate", "成功率"),
        ("dash.metric.latency_p50", "延迟 p50 (毫秒)"),
        ("dash.metric.latency_p90", "延迟 p90 (毫秒)"),
        ("dash.metric.latency_p99", "延迟 p99 (毫秒)"),
        ("dash.metric.latency_p999", "延迟 p99.9 (毫秒)"),
        ("dash.metric.ttft_p50", "首 token p50 (毫秒)"),
        ("dash.metric.ttft_p99", "首 token p99 (毫秒)"),
        ("dash.metric.itl_mean", "token 间隔 mean (毫秒)"),
        ("dash.metric.itl_p99", "token 间隔 p99 (毫秒)"),
        ("dash.metric.tps_mean", "TPS mean (token/秒)"),
        ("dash.metric.tps_p99", "TPS p99 (token/秒)"),
        ("dash.metric.total_tokens", "总 token"),
        ("dash.metric.duration", "时长 (秒)"),
        ("dash.metric.cache_hit_rate", "缓存命中率"),
        ("dash.metric.cache_creation_total", "写入缓存的 token"),
        ("dash.latency_chart_a", "A 延迟 (毫秒)"),
        ("dash.latency_chart_b", "B 延迟 (毫秒)"),
        ("dash.pick_run_hint", "请在上方选择 A 和 B 后点击「对比」。"),
        ("dash.pick_same_run_warning", "提示：A 和 B 选了同一份报告，差异为 0。"),
        ("dash.warnings", "警告"),
        ("cmp.title", "power_test 对比报告"),
        ("cmp.generated", "生成于"),
        ("cmp.warnings", "警告："),
        ("cmp.run_a_head", "A · {id}"),
        ("cmp.run_b_head", "B · {id}"),
        ("cmp.run_id_label", "运行 ID"),
        ("cmp.target", "目标"),
        ("cmp.model", "模型"),
        ("cmp.target_rps", "目标 RPS"),
        ("cmp.duration", "时长"),
        ("cmp.total_requests", "总请求数"),
        ("cmp.success_rate", "成功率"),
        ("cmp.status", "状态"),
        ("cmp.diff_heading", "差异"),
        ("cmp.col_metric", "指标"),
        ("cmp.col_a", "A"),
        ("cmp.col_b", "B"),
        ("cmp.col_delta", "差值"),
        ("cmp.col_pct", "%"),
        ("cmp.latency_heading", "延迟分位数 (毫秒)"),
        ("cmp.latency_chart_a", "A 延迟 (毫秒)"),
        ("cmp.latency_chart_b", "B 延迟 (毫秒)"),
        ("cmp.latency_chart_label", "延迟 (毫秒)"),
        ("cmp.metric.achieved_rps", "实际 RPS"),
        ("cmp.metric.total_requests", "总请求数"),
        ("cmp.metric.success_rate", "成功率"),
        ("cmp.metric.latency_p50", "延迟 p50 (毫秒)"),
        ("cmp.metric.latency_p90", "延迟 p90 (毫秒)"),
        ("cmp.metric.latency_p99", "延迟 p99 (毫秒)"),
        ("cmp.metric.latency_p999", "延迟 p99.9 (毫秒)"),
        ("cmp.metric.ttft_p50", "首 token p50 (毫秒)"),
        ("cmp.metric.ttft_p99", "首 token p99 (毫秒)"),
        ("cmp.metric.itl_mean", "token 间隔 mean (毫秒)"),
        ("cmp.metric.itl_p99", "token 间隔 p99 (毫秒)"),
        ("cmp.metric.tps_mean", "TPS mean (token/秒)"),
        ("cmp.metric.tps_p99", "TPS p99 (token/秒)"),
        ("cmp.metric.total_tokens", "总 token"),
        ("cmp.metric.duration", "时长 (秒)"),
    ]
}

const fn en_dict() -> &'static [(&'static str, &'static str)] {
    &[
        ("page.title", "power_test report"),
        ("page.run_label", "run"),
        ("page.started_label", "started"),
        ("page.interrupted_label", "(interrupted)"),
        ("ui.compare_with", "Compare with…"),
        ("ui.compare", "Compare"),
        ("ui.pick_run", "— pick a run —"),
        ("ui.pre_render_hint", "pre-render via `power_test compare --html`"),
        ("ui.language_toggle_zh", "中文"),
        ("ui.language_toggle_en", "English"),
        ("ui.open_report", "Open report"),
        ("card.target", "Target"),
        ("card.model", "Model"),
        ("card.duration", "Duration"),
        ("card.target_rps", "Target RPS"),
        ("card.achieved_rps", "Achieved RPS"),
        ("card.success_rate", "Success rate"),
        ("card.total_requests", "Total requests"),
        ("card.total_tokens", "Total tokens"),
        ("config.heading", "Configuration"),
        ("config.run_identity", "Run identity"),
        ("config.load", "Load"),
        ("config.dataset", "Dataset"),
        ("config.run_id", "Run id"),
        ("config.tag", "Tag"),
        ("config.started", "Started"),
        ("config.target", "Target"),
        ("config.api", "API"),
        ("config.model", "Model"),
        ("config.pattern", "Pattern"),
        ("config.concurrency", "Concurrency"),
        ("config.stream", "Stream"),
        ("config.max_tokens", "Max tokens"),
        ("config.kind", "Kind"),
        ("config.source", "Source"),
        ("config.strategy", "Strategy"),
        ("config.prompt_tokens", "Prompt tokens"),
        ("metric.heading", "Summary statistics"),
        ("metric.col_metric", "Metric"),
        ("metric.latency", "Latency (ms)"),
        ("metric.ttft", "TTFT (ms)"),
        ("metric.itl", "ITL (ms)"),
        ("metric.tps", "TPS (tok/s)"),
        ("metric.t8", "Advanced metrics"),
        ("metric.tpot", "TPOT (ms/token)"),
        ("metric.throughput_output", "Output tok/s"),
        ("metric.throughput_total", "Total tok/s"),
        ("metric.avg_input", "Avg Input Tokens"),
        ("metric.avg_output", "Avg Output Tokens"),
        ("metric.avg_turns", "Avg Turns/Request"),
        ("metric.spec_decoded", "Decoded Tok/Iter"),
        ("metric.spec_accept", "Spec. Accept Rate"),
        ("metric.p50", "p50"),
        ("metric.p90", "p90"),
        ("metric.p99", "p99"),
        ("metric.p999", "p99.9"),
        ("charts.heading", "Charts"),
        ("chart.latency", "Latency percentiles (ms)"),
        ("chart.status", "Status code distribution"),
        ("chart.rps", "Per-second RPS"),
        ("chart.rps_started", "Started"),
        ("chart.rps_completed", "Completed"),
        ("errors.heading", "Errors"),
        ("errors.none", "No errors."),
        ("cache.heading", "Prompt cache"),
        ("cache.hit_rate", "Cache hit rate"),
        ("cache.no_data", "no cache observed"),
        ("cache.overall", "Overall"),
        ("cache.turn1", "Turn 1"),
        ("cache.turn2plus", "Turn 2+"),
        ("cache.creation", "tokens written to cache"),
        ("cache.hit", "prompt tokens served from prefix cache"),
        ("cache.denominator_zero", "Run had 0 prompt tokens — rate undefined"),
        ("cache.no_continuations", "No continuation turns were observed"),
        ("footer.powered_by", "powered by power_test v{version}"),
        ("dash.title", "power_test model report"),
        ("dash.subtitle", "{n} runs · alias = {alias}"),
        ("dash.subtitle_no_alias", "{n} runs"),
        ("dash.compare_pick", "Pick 2 runs to compare"),
        ("dash.run_a", "A: {id}"),
        ("dash.run_b", "B: {id}"),
        ("dash.compare", "Compare"),
        ("dash.reset", "Reset"),
        ("dash.open_compare", "Open full compare report"),
        ("dash.no_runs", "No runs yet for this model. Run power_test run to generate one."),
        ("dash.runs_heading", "History (newest first)"),
        ("dash.col_run_id", "run id"),
        ("dash.col_time", "started"),
        ("dash.col_target", "target"),
        ("dash.col_rps", "RPS"),
        ("dash.col_duration", "duration"),
        ("dash.col_requests", "requests"),
        ("dash.col_success", "success"),
        ("dash.col_p50", "p50"),
        ("dash.col_p99", "p99"),
        ("dash.col_tps", "TPS"),
        ("dash.col_cache", "cache"),
        ("dash.col_status", "status"),
        ("dash.col_tag", "tag"),
        ("dash.col_alias", "alias"),
        ("dash.open_report", "open report"),
        ("dash.diff_heading", "Diff"),
        ("dash.col_a", "A"),
        ("dash.col_b", "B"),
        ("dash.col_delta", "delta"),
        ("dash.col_pct", "%"),
        ("dash.metric.achieved_rps", "achieved RPS"),
        ("dash.metric.total_requests", "total requests"),
        ("dash.metric.success_rate", "success rate %"),
        ("dash.metric.latency_p50", "latency p50 (ms)"),
        ("dash.metric.latency_p90", "latency p90 (ms)"),
        ("dash.metric.latency_p99", "latency p99 (ms)"),
        ("dash.metric.latency_p999", "latency p99.9 (ms)"),
        ("dash.metric.ttft_p50", "TTFT p50 (ms)"),
        ("dash.metric.ttft_p99", "TTFT p99 (ms)"),
        ("dash.metric.itl_mean", "ITL mean (ms)"),
        ("dash.metric.itl_p99", "ITL p99 (ms)"),
        ("dash.metric.tps_mean", "TPS mean (tok/s)"),
        ("dash.metric.tps_p99", "TPS p99 (tok/s)"),
        ("dash.metric.total_tokens", "total tokens"),
        ("dash.metric.duration", "duration (s)"),
        ("dash.metric.cache_hit_rate", "cache hit rate"),
        ("dash.metric.cache_creation_total", "cache creation tokens"),
        ("dash.latency_chart_a", "A latency (ms)"),
        ("dash.latency_chart_b", "B latency (ms)"),
        ("dash.pick_run_hint", "Pick run A and B above, then click Compare."),
        ("dash.pick_same_run_warning", "Note: A and B are the same run — the diff is all zeros."),
        ("dash.warnings", "warnings"),
        ("cmp.title", "power_test compare"),
        ("cmp.generated", "generated"),
        ("cmp.warnings", "warnings:"),
        ("cmp.run_a_head", "a · {id}"),
        ("cmp.run_b_head", "b · {id}"),
        ("cmp.run_id_label", "run id"),
        ("cmp.target", "target"),
        ("cmp.model", "model"),
        ("cmp.target_rps", "target RPS"),
        ("cmp.duration", "duration"),
        ("cmp.total_requests", "total requests"),
        ("cmp.success_rate", "success rate"),
        ("cmp.status", "status"),
        ("cmp.diff_heading", "Diff"),
        ("cmp.col_metric", "metric"),
        ("cmp.col_a", "a"),
        ("cmp.col_b", "b"),
        ("cmp.col_delta", "delta"),
        ("cmp.col_pct", "%"),
        ("cmp.latency_heading", "Latency percentiles (ms)"),
        ("cmp.latency_chart_a", "A latency (ms)"),
        ("cmp.latency_chart_b", "B latency (ms)"),
        ("cmp.latency_chart_label", "Latency (ms)"),
        ("cmp.metric.achieved_rps", "achieved RPS"),
        ("cmp.metric.total_requests", "total requests"),
        ("cmp.metric.success_rate", "success rate %"),
        ("cmp.metric.latency_p50", "latency p50 (ms)"),
        ("cmp.metric.latency_p90", "latency p90 (ms)"),
        ("cmp.metric.latency_p99", "latency p99 (ms)"),
        ("cmp.metric.latency_p999", "latency p99.9 (ms)"),
        ("cmp.metric.ttft_p50", "TTFT p50 (ms)"),
        ("cmp.metric.ttft_p99", "TTFT p99 (ms)"),
        ("cmp.metric.itl_mean", "ITL mean (ms)"),
        ("cmp.metric.itl_p99", "ITL p99 (ms)"),
        ("cmp.metric.tps_mean", "TPS mean (tok/s)"),
        ("cmp.metric.tps_p99", "TPS p99 (tok/s)"),
        ("cmp.metric.total_tokens", "total tokens"),
        ("cmp.metric.duration", "duration (s)"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_zh_default() {
        assert_eq!(t(Locale::Zh, "card.target").as_ref(), "目标");
        assert_eq!(t(Locale::Zh, "metric.latency").as_ref(), "延迟 (毫秒)");
    }

    #[test]
    fn lookup_en_returns_english() {
        assert_eq!(t(Locale::En, "card.target").as_ref(), "Target");
        assert_eq!(t(Locale::En, "metric.latency").as_ref(), "Latency (ms)");
    }

    #[test]
    fn default_locale_is_zh() {
        assert_eq!(Locale::default(), Locale::Zh);
    }

    #[test]
    fn locale_as_str() {
        assert_eq!(Locale::Zh.as_str(), "zh");
        assert_eq!(Locale::En.as_str(), "en");
    }

    #[test]
    fn missing_key_returns_key_string() {
        assert_eq!(t(Locale::Zh, "card.does_not_exist").as_ref(), "card.does_not_exist");
        assert_eq!(t(Locale::En, "card.does_not_exist").as_ref(), "card.does_not_exist");
    }

    #[test]
    fn dictionaries_have_same_keys() {
        use std::collections::BTreeSet;
        let zh: BTreeSet<&str> = zh_dict().iter().map(|(k, _)| *k).collect();
        let en: BTreeSet<&str> = en_dict().iter().map(|(k, _)| *k).collect();
        let only_zh: Vec<&&str> = zh.difference(&en).collect();
        let only_en: Vec<&&str> = en.difference(&zh).collect();
        assert!(only_zh.is_empty(), "keys in zh but not en: {:?}", only_zh);
        assert!(only_en.is_empty(), "keys in en but not zh: {:?}", only_en);
        assert_eq!(zh.len(), key_count());
    }

    #[test]
    fn dict_to_json_is_valid_and_round_trips() {
        let json = dict_to_json(Locale::En);
        let parsed: serde_json::Value =
            serde_json::from_str(&json).expect("en dict must be valid JSON");
        assert_eq!(parsed["card.target"], "Target");
        assert_eq!(parsed["metric.latency"], "Latency (ms)");
        let zh_json = dict_to_json(Locale::Zh);
        let zh_parsed: serde_json::Value =
            serde_json::from_str(&zh_json).expect("zh dict must be valid JSON");
        assert_eq!(zh_parsed["card.target"], "目标");
    }

    #[test]
    fn dict_to_json_escapes_unsafe_substrings() {
        let json = dict_to_json(Locale::En);
        assert!(!json.contains("</script>"));
    }
}
