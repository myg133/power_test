//! Dataset abstraction. M1 only had [`simple::SimpleDataset`]; M2 adds a
//! built-in prompt pool, a ShareGPT loader, and a custom JSON/JSONL loader.
//! All share the same [`Dataset`] trait and are dispatched by [`build`].

pub mod builtin;
pub mod custom;
pub mod pool;
pub mod sharegpt;
pub mod simple;

use std::path::Path;

use async_trait::async_trait;

use crate::config::{DatasetSpec, PromptDistribution, RequestStrategy};
use crate::error::{Error, Result};

/// A single request ready to fire.
#[derive(Debug, Clone)]
pub struct DatasetItem {
    /// The prompt string to send.
    pub prompt: String,
    /// Rough pre-flight estimate of prompt token count.
    pub estimated_prompt_tokens: u32,
}

/// Anything that can produce a sequence of prompts for a load test.
#[async_trait]
pub trait Dataset: Send + Sync {
    /// Return the next item. For a constant-prompt dataset this is the same
    /// every time; for a streaming dataset this advances.
    async fn next(&self) -> DatasetItem;
}

/// Build a dataset from a [`DatasetSpec`] + [`RequestStrategy`], and
/// compute the resulting [`PromptDistribution`] in one pass.
pub fn build_with_distribution(
    spec: &DatasetSpec,
    strategy: RequestStrategy,
) -> Result<(Box<dyn Dataset>, PromptDistribution)> {
    match spec {
        DatasetSpec::Literal { text } => {
            let tokens = crate::config::estimate_tokens(text);
            let ds = simple::SimpleDataset::new_literal(text.clone());
            Ok((Box::new(ds), PromptDistribution::from_single(tokens)))
        }
        DatasetSpec::TokenBudget { target_tokens } => {
            // Resolve once to get the actual text + token estimate.
            let (text, tokens) = crate::config::PromptSource::TokenBudget {
                target_tokens: *target_tokens,
            }
            .resolve();
            let ds = simple::SimpleDataset::new_literal(text);
            Ok((Box::new(ds), PromptDistribution::from_single(tokens)))
        }
        DatasetSpec::Builtin => {
            let items = builtin::builtin_pool();
            let dist = PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            );
            let ds = pool::PoolDataset::new(items, strategy);
            Ok((Box::new(ds), dist))
        }
        DatasetSpec::ShareGpt { path } => {
            let items = sharegpt::load(path)?;
            let dist = PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            );
            let ds = pool::PoolDataset::new(items, strategy);
            Ok((Box::new(ds), dist))
        }
        DatasetSpec::Custom { path } => {
            let items = custom::load(path)?;
            let dist = PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            );
            let ds = pool::PoolDataset::new(items, strategy);
            Ok((Box::new(ds), dist))
        }
    }
}

/// Backward-compatible build: only returns the dataset, no distribution.
/// Used internally by the executor; the CLI also calls
/// [`build_with_distribution`] to populate `RunConfig::prompt_distribution`.
pub fn build(spec: &DatasetSpec, strategy: RequestStrategy) -> Result<Box<dyn Dataset>> {
    build_with_distribution(spec, strategy).map(|(d, _)| d)
}

/// Convenience for tests / callers that already have a path-shaped spec
/// and want to report a clean error if the file is missing.
pub fn ensure_exists(path: &Path) -> Result<()> {
    if !path.exists() {
        return Err(Error::InvalidConfig(format!(
            "dataset file not found: {}",
            path.display()
        )));
    }
    Ok(())
}
