//! HTTP clients for LLM endpoints.

pub mod anthropic;
pub mod openai;
pub mod raw;
pub mod responses;

use std::time::Duration;

use async_trait::async_trait;
use serde::Serialize;

use crate::config::ApiKind;
use crate::dataset::OwnedChatMessage;
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
    /// M6d: the assistant's response text, joined across all chunks
    /// (streaming) or copied from the body (non-streaming). Empty
    /// when the request errored before any text was produced, or
    /// when the client doesn't surface text (e.g. `RawClient`,
    /// which has no JSON to read). The session pool feeds this
    /// back into the next turn's `messages` so the model sees
    /// the prior assistant response.
    pub response_text: String,
    /// M6e: prompt tokens the model wrote to its prefix KV cache
    /// on this request. Anthropic emits this as
    /// `usage.cache_creation_input_tokens`; OpenAI does not surface
    /// an equivalent field, so this stays 0 on the OpenAI path.
    /// A high number here means the model is paying a one-time cost
    /// to cache a long prefix; subsequent requests to the same
    /// prefix will start hitting `cache_hit_input_tokens` instead.
    pub cache_creation_input_tokens: u32,
    /// M6e: prompt tokens served from the model's prefix cache.
    /// On Anthropic, this is `usage.cache_read_input_tokens` (a
    /// hit, not a re-compute). On OpenAI, this is
    /// `usage.prompt_tokens_details.cached_tokens` (same concept,
    /// different field name). `RawClient` leaves this 0 because
    /// raw HTTP has no JSON to read.
    pub cache_hit_input_tokens: u32,
    /// M8: speculative decoding stats. Standard OpenAI usage
    /// does not expose these, so on OpenAI this stays 0.
    /// vLLM and similar local servers sometimes add
    /// accepted_prediction_tokens to completion_tokens_details.
    pub spec_decoded_tok: u32,
    pub spec_accepted_tok: u32,
    pub spec_iterations: u32,
    /// Wall-clock time the request was issued.
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// Wall-clock time the request finished.
    pub finished_at: chrono::DateTime<chrono::Utc>,
    /// M9: server-side response id from the `/v1/responses` endpoint.
    /// Populated by [`responses::ResponsesClient`] after a successful
    /// turn; empty for the other clients. The session pool reads this
    /// and feeds it back as `previous_response_id` on the next turn
    /// of the same session, enabling stateful multi-turn conversation
    /// without re-sending the prior `input` items.
    pub response_id: Option<String>,
}

impl RequestMetrics {
    /// True when the request produced a usable response (2xx).
    pub fn is_ok(&self) -> bool {
        self.status >= 200 && self.status < 300 && self.error.is_none()
    }
}

/// Send one chat completion. Implementations must never panic on a bad
/// response — they should return a [`RequestMetrics`] with `error` set.
#[async_trait]
pub trait LlmClient: Send + Sync {
    /// Single-turn send. The `prompt` is the user-role text. The
    /// default `send_messages` impl below collapses a multi-turn
    /// `messages` array into one user prompt for clients that don't
    /// override it.
    async fn send(&self, prompt: &str, estimated_prompt_tokens: u32) -> RequestMetrics;

    /// Multi-turn send. Default impl joins all messages into a single
    /// user-role prompt and calls `send`. Clients with first-class
    /// multi-turn support (OpenAI, Anthropic) override this to send
    /// the real `messages` array.
    ///
    /// `previous_response_id` is an M9 hook for stateful APIs that
    /// can resume a prior server-side conversation by id (the
    /// OpenAI `/v1/responses` endpoint via `previous_response_id`).
    /// Default impl ignores it; clients that support the hook
    /// (currently only `ResponsesClient`) read it and act on it.
    async fn send_messages(
        &self,
        messages: &[OwnedChatMessage],
        previous_response_id: Option<&str>,
        estimated_prompt_tokens: u32,
    ) -> RequestMetrics {
        let _ = previous_response_id;
        let joined = messages
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        self.send(&joined, estimated_prompt_tokens).await
    }
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
        ApiKind::Responses => {
            let c = responses::ResponsesClient::new(cfg)?;
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
