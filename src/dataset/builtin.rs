//! Hardcoded prompt pool of varying length. Mix of English and Chinese,
//! ranging from a few words to a couple hundred, so a soak/spike run sees
//! realistic variance in prompt token counts.
//!
//! The pool is intentionally small and fixed: this lets CI and humans
//! reason about the expected distribution without referring to a file.
//! For larger / more varied prompt sets, use `--dataset sharegpt` or
//! `--dataset custom`.

use super::DatasetItem;

/// The hardcoded pool. 12 prompts spanning short / medium / long / very-long
/// and English / Chinese. Ordered roughly by token count so the test report
/// is easy to read.
pub fn builtin_pool() -> Vec<DatasetItem> {
    let raw: &[&str] = &[
        // --- short (≤ 8 tokens) ---
        "Hi.",
        "Translate 'hello' to French.",
        "What is 2+2?",
        "你好。",
        // --- medium (≈ 20-50 tokens) ---
        "Explain the concept of quantum entanglement in simple terms, suitable for a high-school student.",
        "Write a haiku about the changing seasons, focusing on the transition from summer to autumn.",
        "List five healthy breakfast options that can be prepared in under ten minutes.",
        "用一段话解释一下区块链的工作原理,要求通俗易懂,适合完全没有技术背景的读者。",
        // --- long (≈ 100-200 tokens) ---
        "Compare and contrast the political systems of the United States and Sweden. Discuss their historical origins, electoral processes, role of political parties, approaches to social welfare, and the relationship between central and local government. Conclude with an assessment of which system you believe better serves its citizens and why.",
        "You are a senior backend engineer reviewing a pull request that introduces a new caching layer in front of PostgreSQL. The author has chosen a write-through strategy with a 60-second TTL. Identify the main risks of this design (cache stampede, stale data, memory pressure), and propose concrete mitigations. Provide code-level examples where helpful.",
        // --- very long (≈ 300-500 tokens) ---
        "Write a detailed first-person narrative from the perspective of a lighthouse keeper living on a remote island in the early 20th century. The story should span a full year, with the keeper describing the harsh winter storms, the loneliness, the arrival of supply ships in spring, the nesting of seabirds in summer, and the first autumn gales. Include sensory details (the smell of salt, the sound of the foghorn, the feel of wet rope), specific incidents (a ship in distress, a visitor, a personal loss), and reflections on what it means to maintain a steady light through all of it. Aim for approximately 500 words.",
        "请以一位资深前端工程师的身份,撰写一份关于现代 React 性能优化的技术指南。内容应涵盖:1) 组件渲染优化(memo、useMemo、useCallback 的正确使用场景与陷阱);2) 状态管理(Redux、Zustand、Context 的对比);3) 网络层优化(请求合并、缓存策略、SWR / React Query 的取舍);4) 打包优化(code splitting、tree shaking、动态 import);5) 运行时性能(virtualization、web workers、service worker)。每节都应包含真实项目中的反例与正确做法,并简要说明在何种规模下应该引入该优化。",
    ];

    raw.iter()
        .map(|s| DatasetItem {
            prompt: (*s).to_string(),
            estimated_prompt_tokens: crate::config::estimate_tokens(s),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pool_has_mixed_sizes() {
        let pool = builtin_pool();
        assert!(pool.len() >= 10, "expected at least 10 prompts, got {}", pool.len());
        // Verify we have a mix of short and long prompts.
        let tokens: Vec<u32> = pool.iter().map(|i| i.estimated_prompt_tokens).collect();
        let min = *tokens.iter().min().unwrap();
        let max = *tokens.iter().max().unwrap();
        assert!(min <= 5, "min tokens should be tiny, got {min}");
        assert!(max >= 100, "max tokens should be large, got {max}");
    }

    #[test]
    fn pool_contains_chinese() {
        let pool = builtin_pool();
        let has_cjk = pool.iter().any(|i| i.prompt.chars().any(|c| c as u32 > 0x4E00));
        assert!(has_cjk, "expected at least one Chinese prompt");
    }
}
