//! HTTP clients for LLM endpoints.

pub mod anthropic;
pub mod openai;
pub mod raw;

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use crate::config::ApiKind;
use crate::error::Result;

/// Per-request measurement, filled in by the client and consumed by the
/// metrics aggregator.
#[derive(Debug, Clone, Default)]
pub struct RequestMetrics {
    /// HTTP status code. `0` if the request never produced a response.
    pub status: u16,
    /// Human-readable error, populated on transport / parse / non-2xx failures.
    pub error: Option<String>,
    /// Time from request start to first content token arriving. `None` for
    /// non-streaming or errored requests.
    pub ttft: Option<Duration>,
    /// Per-token inter-arrival times after the first token. Empty for
    /// non-streaming requests.
    pub itl_samples: Vec<Duration>,
    /// Completion tokens reported by the model (or estimated from chunks).
    pub completion_tokens: u32,
    /// Prompt tokens reported by the model (or 0 if unknown).
    pub prompt_tokens: u32,
    /// End-to-end duration of the request.
    pub total_duration: Duration,
    /// `true` when `completion_tokens` is a chunk-count estimate, not from
    /// the model's `usage` field.
    pub estimated: bool,
    /// Wall-clock time the request was issued.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Wall-clock time the request finished.
    pub finished_at: chrono::DateTime<chrono::Utc>,
}

/// Send one chat completion. Implementations must never panic on a bad
/// response — they should return a [`RequestMetrics`] with `error` set.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn send(&self, prompt: &str, estimated_prompt_tokens: u32) -> RequestMetrics;
}

/// Build the right client for the chosen [`ApiKind`].
pub fn build(cfg: &crate::config::RunConfig) -> Result<Box<dyn LlmClient>> {
    match cfg.api {
        ApiKind::Openai => {
            let c = openai::OpenaiClient::new(cfg)?;
            Ok(Box::new(c))
        }
        ApiKind::Anthropic => {
            let c = anthropic::AnthropicClient::new(cfg)?;
            Ok(Box::new(c))
        }
        ApiKind::Raw => {
            let c = raw::RawClient::new(cfg)?;
            Ok(Box::new(c))
        }
    }
}

/// JSON request body shared across API styles. OpenAI for now.
#[derive(Debug, Serialize)]
pub struct ChatRequest<'a> {
    pub model: &'a str,
    pub messages: Vec<ChatMessage<'a>>,
    pub max_tokens: u32,
    pub stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_options: Option<StreamOptions>,
}

#[derive(Debug, Serialize)]
pub struct StreamOptions {
    /// Ask OpenAI to include a final chunk with `usage`. Not all
    /// OpenAI-compatible servers honor this.
    pub include_usage: bool,
}

#[derive(Debug, Serialize)]
pub struct ChatMessage<'a> {
    pub role: &'a str,
    pub content: &'a str,
}
