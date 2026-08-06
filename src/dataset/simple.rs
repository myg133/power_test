//! Constant-prompt dataset: always returns the same resolved text. Used
//! for [`DatasetSpec::Literal`] and [`DatasetSpec::TokenBudget`].

use async_trait::async_trait;

use super::{Dataset, DatasetItem};

/// Always returns the same resolved prompt. The `estimated_prompt_tokens`
/// is computed once at construction.
pub struct SimpleDataset {
    item: DatasetItem,
}

impl SimpleDataset {
    pub fn new_literal(text: String) -> Self {
        let tokens = crate::config::estimate_tokens(&text);
        Self {
            item: DatasetItem {
                prompt: text,
                estimated_prompt_tokens: tokens,
                weight: None,
                tags: Vec::new(),
                name: None,
                messages: None,
                follow_ups: Vec::new(),
            },
        }
    }
}

#[async_trait]
impl Dataset for SimpleDataset {
    async fn next(&self) -> DatasetItem {
        self.item.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn literal_dataset_returns_same_prompt() {
        let d = SimpleDataset::new_literal("hello world".into());
        let a = d.next().await;
        let b = d.next().await;
        assert_eq!(a.prompt, "hello world");
        assert_eq!(b.prompt, "hello world");
        assert!(a.estimated_prompt_tokens >= 2);
    }
}
