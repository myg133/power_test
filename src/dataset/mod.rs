//! Dataset abstraction. M1 only had [`simple::SimpleDataset`]; M2 adds a
//! built-in prompt pool, a ShareGPT loader, and a custom JSON/JSONL loader.
//! M6 extends the custom loader with a TOML profile format and adds
//! multi-turn execution modes (single / static_multi / dynamic_multi) that
//! are selected automatically by the loader — never by a CLI flag.

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
///
/// In single-turn mode, `prompt` carries the request text and `messages`
/// is `None`. In static-multi mode, `messages` carries the full body and
/// `prompt` is the joined preview for distribution / report purposes.
/// In dynamic-multi mode, `messages` is the seed and `follow_ups` lists
/// the user-side messages for subsequent turns.
#[derive(Debug, Clone)]
pub struct DatasetItem {
    /// Single-turn convenience prompt text. Used when `messages` is `None`.
    /// For multi-turn items this is the join of all message contents,
    /// kept around for the prompt-distribution report and tag summaries.
    pub prompt: String,
    /// Rough pre-flight estimate of prompt token count.
    pub estimated_prompt_tokens: u32,
    /// Optional sampling weight. `None` or `0` items are skipped. `1`
    /// is the default when not set.
    pub weight: Option<u32>,
    /// Optional tags. Carried through for report grouping.
    pub tags: Vec<String>,
    /// Optional stable name. Defaults to a synthetic id.
    pub name: Option<String>,
    /// Optional full `messages` array. `Some(_)`: static or dynamic
    /// multi-turn. `None`: single-turn.
    pub messages: Option<Vec<OwnedChatMessage>>,
    /// Per-turn follow-up user messages. Non-empty on a multi-turn
    /// item ⇒ dynamic multi-turn (session pool). Empty on a multi-turn
    /// item ⇒ static multi-turn (single request with the full
    /// `messages`).
    pub follow_ups: Vec<String>,
}

/// Owned counterpart of [`crate::client::ChatMessage`]. The client trait
/// borrows strings; the dataset owns them so items can be cloned freely
/// across the executor and session pool.
#[derive(Debug, Clone)]
pub struct OwnedChatMessage {
    pub role: String,
    pub content: String,
}

impl OwnedChatMessage {
    pub fn new(role: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// Execution mode for the dataset. Determined by the loader at load
/// time; one dataset file = one mode (no mixing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatasetMode {
    /// Every item is a single text prompt. One HTTP request per item,
    /// one assistant response, no session.
    Single,
    /// Every item has a `messages` array but no `follow_ups`. One
    /// HTTP request per item with the full `messages` body, one
    /// assistant response, no session.
    StaticMulti,
    /// Every item has a `messages` seed plus a non-empty `follow_ups`
    /// list. Each item is run as a chain of serial turns inside its
    /// own session. K = `--concurrency` parallel sessions.
    DynamicMulti,
}

/// Anything that can produce a sequence of prompts for a load test.
#[async_trait]
pub trait Dataset: Send + Sync {
    /// Return the next item. For a constant-prompt dataset this is the same
    /// every time; for a streaming dataset this advances.
    async fn next(&self) -> DatasetItem;

    /// The execution mode of this dataset. Defaults to `Single` for
    /// the simple / builtin / sharegpt / custom-JSON paths; the
    /// custom-TOML profile path overrides it.
    fn mode(&self) -> DatasetMode {
        DatasetMode::Single
    }
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
            let ds = pool::PoolDataset::new(items, strategy, DatasetMode::Single);
            Ok((Box::new(ds), dist))
        }
        DatasetSpec::ShareGpt { path } => {
            let items = sharegpt::load(path)?;
            let dist = PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            );
            let ds = pool::PoolDataset::new(items, strategy, DatasetMode::Single);
            Ok((Box::new(ds), dist))
        }
        DatasetSpec::Custom { path } => {
            // M6: custom loader now returns (items, mode). JSON paths
            // are always `Single`; TOML profile can be any of the
            // three. The loader does the mode-detection / fail-fast
            // for mixed files.
            let (items, mode) = custom::load_with_mode(path)?;
            let dist = PromptDistribution::from_slice(
                &items.iter().map(|i| i.estimated_prompt_tokens).collect::<Vec<_>>(),
            );
            let ds = pool::PoolDataset::new(items, strategy, mode);
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
