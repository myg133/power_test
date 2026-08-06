//! OpenAI-compatible `/v1/chat/completions` client with SSE streaming support.
//!
//! The streaming path buffers partial events across chunks (TCP chunks do
//! not align with SSE event boundaries), records TTFT as the elapsed time
//! from request start to the first delta (content or reasoning), and ITL
//! as the gap between consecutive deltas of either kind.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;

use super::{ChatMessage, ChatRequest, LlmClient, RequestMetrics, StreamOptions};
use crate::config::RunConfig;
use crate::dataset::OwnedChatMessage;
use crate::error::Result;

pub struct OpenaiClient {
    http: reqwest::Client,
    target: String,
    model: String,
    max_tokens: u32,
    stream: bool,
    /// Optional API key. When set, sent as `Authorization: Bearer <key>`.
    api_key: Option<String>,
}

impl OpenaiClient {
    pub fn new(cfg: &RunConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            target: cfg.target.clone(),
            model: cfg.model.clone(),
            max_tokens: cfg.max_tokens,
            stream: cfg.stream,
            api_key: cfg.api_key.clone(),
        })
    }

    fn build_body<'a>(&'a self, prompt: &'a str) -> ChatRequest<'a> {
        ChatRequest {
            model: &self.model,
            messages: vec![ChatMessage {
                role: "user",
                content: prompt,
            }],
            max_tokens: self.max_tokens,
            stream: self.stream,
            stream_options: if self.stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
        }
    }

    /// Build a body with a full `messages` array. Used by the
    /// multi-turn `send_messages` override below.
    fn build_body_messages<'a>(
        &'a self,
        messages: &'a [OwnedChatMessage],
    ) -> ChatRequest<'a> {
        let borrowed: Vec<ChatMessage<'a>> = messages
            .iter()
            .map(|m| ChatMessage {
                role: m.role.as_str(),
                content: m.content.as_str(),
            })
            .collect();
        ChatRequest {
            model: &self.model,
            messages: borrowed,
            max_tokens: self.max_tokens,
            stream: self.stream,
            stream_options: if self.stream {
                Some(StreamOptions {
                    include_usage: true,
                })
            } else {
                None
            },
        }
    }

    async fn parse_stream(&self, resp: reqwest::Response, start: Instant, m: &mut RequestMetrics) {
        let mut stream = resp.bytes_stream();
        let mut parser = SseParser::new();
        let mut first_token_at: Option<Duration> = None;
        let mut last_token_at: Option<Duration> = None;
        let mut delta_count: u32 = 0;
        let mut usage_completion: Option<u32> = None;
        let mut usage_prompt: Option<u32> = None;
        // M6d: collect the visible assistant text (delta.content
        // only — reasoning_content is NOT echoed back to the model
        // because most providers treat reasoning as ephemeral
        // state, not conversation history).
        let mut response_text = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    m.error = Some(format!("stream read: {e}"));
                    break;
                }
            };
            let now = start.elapsed();
            for event in parser.feed(&chunk) {
                if event.data == "[DONE]" {
                    continue;
                }
                let parsed: serde_json::Value = match serde_json::from_str(&event.data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if let Some(u) = parsed.get("usage") {
                    if let Some(ct) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
                        usage_completion = Some(ct as u32);
                    }
                    if let Some(pt) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
                        usage_prompt = Some(pt as u32);
                    }
                }
                if let Some(choices) = parsed.get("choices").and_then(|v| v.as_array()) {
                    for choice in choices {
                        // TTFT and ITL treat both content deltas (the visible
                        // response) and reasoning deltas (the model's thinking
                        // stream) as "token arrivals". A reasoning token that
                        // lands before the first content token is still the
                        // first token the model emitted.
                        let delta = choice.get("delta");
                        let content = delta
                            .and_then(|d| d.get("content"))
                            .and_then(|c| c.as_str());
                        let reasoning = delta
                            .and_then(|d| d.get("reasoning_content"))
                            .and_then(|c| c.as_str());
                        // M6d: only the visible content is echoed back into
                        // the session. Reasoning is not conversation
                        // history under most providers' semantics.
                        if let Some(c) = content {
                            if !c.is_empty() {
                                response_text.push_str(c);
                            }
                        }
                        let combined: String = match (content, reasoning) {
                            (Some(c), Some(r)) if !c.is_empty() && !r.is_empty() => {
                                format!("{c}{r}")
                            }
                            (Some(c), _) if !c.is_empty() => c.to_string(),
                            (_, Some(r)) if !r.is_empty() => r.to_string(),
                            _ => String::new(),
                        };
                        if !combined.is_empty() {
                            delta_count += 1;
                            if first_token_at.is_none() {
                                first_token_at = Some(now);
                            } else if let Some(last) = last_token_at {
                                let gap = now.checked_sub(last).unwrap_or(Duration::ZERO);
                                m.itl_samples.push(gap);
                            }
                            last_token_at = Some(now);
                        }
                    }
                }
            }
        }

        m.ttft = first_token_at;
        m.completion_tokens = usage_completion.unwrap_or(delta_count);
        m.estimated = usage_completion.is_none();
        m.prompt_tokens = usage_prompt.unwrap_or(0);
        // M6d: hand the joined assistant text to the session pool.
        m.response_text = response_text;
        m.total_duration = start.elapsed();
    }

    async fn parse_single(&self, resp: reqwest::Response, start: Instant, m: &mut RequestMetrics) {
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                m.error = Some(format!("body parse: {e}"));
                m.total_duration = start.elapsed();
                return;
            }
        };
        // M6d: collect the assistant text from the first choice's
        // message.content. We join across choices in case the model
        // returns multiple (n>1), but for the common n=1 case this
        // is a single assignment.
        let mut joined = String::new();
        if let Some(choices) = body.get("choices").and_then(|v| v.as_array()) {
            for choice in choices {
                if let Some(content) = choice
                    .get("message")
                    .and_then(|m| m.get("content"))
                    .and_then(|c| c.as_str())
                {
                    if !joined.is_empty() {
                        joined.push('\n');
                    }
                    joined.push_str(content);
                    m.completion_tokens += crate::config::estimate_tokens(content);
                }
            }
        }
        m.response_text = joined;
        m.estimated = true;
        if let Some(u) = body.get("usage") {
            if let Some(ct) = u.get("completion_tokens").and_then(|v| v.as_u64()) {
                m.completion_tokens = ct as u32;
                m.estimated = false;
            }
            if let Some(pt) = u.get("prompt_tokens").and_then(|v| v.as_u64()) {
                m.prompt_tokens = pt as u32;
            }
        }
        let total = start.elapsed();
        m.ttft = Some(total); // non-streaming: TTFT == total
        m.total_duration = total;
    }
}

#[async_trait]
impl LlmClient for OpenaiClient {
    async fn send(&self, prompt: &str, _estimated_prompt_tokens: u32) -> RequestMetrics {
        let body = self.build_body(prompt);
        self.dispatch(body).await
    }

    async fn send_messages(
        &self,
        messages: &[OwnedChatMessage],
        _estimated_prompt_tokens: u32,
    ) -> RequestMetrics {
        let body = self.build_body_messages(messages);
        self.dispatch(body).await
    }
}

impl OpenaiClient {
    /// Shared send path: build a body, send the request, parse the
    /// response. Used by both `send` (single-turn) and `send_messages`
    /// (multi-turn).
    async fn dispatch(&self, body: ChatRequest<'_>) -> RequestMetrics {
        let started_at = chrono::Utc::now();
        let start = Instant::now();
        let mut m = RequestMetrics::default();
        m.started_at = started_at;

        let mut req = self.http.post(&self.target).json(&body);
        if let Some(key) = &self.api_key {
            if !key.is_empty() {
                req = req.bearer_auth(key);
            }
        }
        let resp = match req.send().await {
            Ok(r) => r,
            Err(e) => {
                m.error = Some(format!("transport: {e}"));
                m.total_duration = start.elapsed();
                m.finished_at = chrono::Utc::now();
                return m;
            }
        };

        m.status = resp.status().as_u16();
        if !resp.status().is_success() {
            let snippet = resp.text().await.unwrap_or_default();
            m.error = Some(format!("HTTP {}: {}", m.status, truncate(&snippet, 200)));
            m.total_duration = start.elapsed();
            m.finished_at = chrono::Utc::now();
            return m;
        }

        if self.stream {
            self.parse_stream(resp, start, &mut m).await;
        } else {
            self.parse_single(resp, start, &mut m).await;
        }
        m.finished_at = chrono::Utc::now();
        m
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        format!("{}…", &s[..end])
    }
}

/// Tiny streaming SSE parser. Buffers bytes, emits one event per `\n\n`
/// boundary. Tolerates both `\n` and `\r\n` line endings.
struct SseParser {
    buffer: Vec<u8>,
}

struct SseEvent {
    data: String,
}

impl SseParser {
    fn new() -> Self {
        Self { buffer: Vec::new() }
    }

    fn feed(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            match find_event_boundary(&self.buffer) {
                Some((event_end, sep_len)) => {
                    let bytes: Vec<u8> = self.buffer.drain(..event_end).collect();
                    self.buffer.drain(..sep_len);
                    if let Some(ev) = Self::parse_event_bytes(&bytes) {
                        events.push(ev);
                    }
                }
                None => break,
            }
        }
        events
    }

    fn parse_event_bytes(bytes: &[u8]) -> Option<SseEvent> {
        let text = String::from_utf8_lossy(bytes);
        let mut data_lines: Vec<String> = Vec::new();
        for line in text.lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                data_lines.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.to_string());
            }
        }
        if data_lines.is_empty() {
            return None;
        }
        Some(SseEvent {
            data: data_lines.join("\n"),
        })
    }
}

fn find_event_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    // Returns (event_content_end, separator_length).
    if buf.len() < 2 {
        return None;
    }
    for i in 0..buf.len() - 1 {
        if buf[i] == b'\n' && buf[i + 1] == b'\n' {
            return Some((i, 2));
        }
        if i + 3 < buf.len() && buf[i] == b'\r' && buf[i + 1] == b'\n' && buf[i + 2] == b'\r' && buf[i + 3] == b'\n' {
            return Some((i, 4));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
        RunConfig,
    };

    fn base_config() -> RunConfig {
        RunConfig {
            run_id: "test".into(),
            target: "http://localhost:9999/v1/chat/completions".into(),
            api: ApiKind::Openai,
            model: "gpt-test".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 1.0 },
            max_tokens: 16,
            stream: true,
            target_rps: 1.0,
            duration_secs: 1,
            concurrency: 4,
            tag: None,
            api_key: Some("sk-openai-test".into()),
            started_at: chrono::Utc::now(),
            raw_body_file: None,
            raw_content_type: None,
        }
    }

    #[test]
    fn sse_parser_single_event() {
        let mut p = SseParser::new();
        let evs = p.feed(b"data: {\"a\":1}\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "{\"a\":1}");
    }

    #[test]
    fn sse_parser_multi_event_in_one_chunk() {
        let mut p = SseParser::new();
        let evs = p.feed(b"data: 1\n\ndata: 2\n\n");
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].data, "1");
        assert_eq!(evs[1].data, "2");
    }

    #[test]
    fn sse_parser_split_across_chunks() {
        let mut p = SseParser::new();
        let evs1 = p.feed(b"data: {\"a\"");
        assert!(evs1.is_empty());
        let evs2 = p.feed(b":1}\n\n");
        assert_eq!(evs2.len(), 1);
        assert_eq!(evs2[0].data, "{\"a\":1}");
    }

    #[test]
    fn sse_parser_done_marker() {
        let mut p = SseParser::new();
        let evs = p.feed(b"data: [DONE]\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "[DONE]");
    }

    #[test]
    fn sse_parser_crlf_endings() {
        let mut p = SseParser::new();
        let evs = p.feed(b"data: hi\r\n\r\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "hi");
    }

    #[test]
    fn sse_parser_ignores_non_data_lines() {
        let mut p = SseParser::new();
        let evs = p.feed(b"event: message\ndata: x\nid: 1\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "x");
    }

    #[test]
    fn truncate_respects_char_boundaries() {
        let s = "héllo";
        let t = truncate(s, 2);
        assert!(t.ends_with('…') || t == "héllo");
    }

    /// A model that emits `delta.reasoning_content` before the visible
    /// `delta.content` must record TTFT against the first reasoning
    /// delta, and ITL across the reasoning→content boundary.
    #[tokio::test]
    async fn full_streaming_with_reasoning_content_counts_as_ttft() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
            "\"choices\":[{\"index\":0,\"delta\":{\"reasoning_content\":\"Let me think\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
            "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
            "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":3,\"completion_tokens\":5}}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(header("authorization", "Bearer sk-openai-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let mut cfg = base_config();
        cfg.target = format!("{}/v1/chat/completions", server.uri());
        cfg.stream = true;
        cfg.api_key = Some("sk-openai-test".into());
        let c = OpenaiClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
        assert!(m.error.is_none());
        assert!(m.ttft.is_some(), "TTFT must be set even when first delta is a reasoning_content");
        // Two deltas (reasoning + content) → one ITL sample.
        assert_eq!(m.itl_samples.len(), 1, "reasoning→content gap must contribute one ITL sample");
        // usage.completion_tokens (5) wins over the delta count (2).
        assert_eq!(m.completion_tokens, 5);
        assert!(!m.estimated);
    }

    /// M6 multi-turn: `send_messages` must put the full `messages`
    /// array into the request body, not collapse to a single user
    /// prompt.
    #[tokio::test]
    async fn send_messages_emits_full_messages_array_in_body() {
        use crate::dataset::OwnedChatMessage;
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = concat!(
            "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
            "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
            "data: [DONE]\n\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_partial_json(serde_json::json!({
                "messages": [
                    {"role": "system", "content": "be terse"},
                    {"role": "user",   "content": "what is 2+2?"},
                    {"role": "assistant", "content": "4."},
                    {"role": "user",   "content": "and 3+3?"}
                ]
            })))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .expect(1)
            .mount(&server)
            .await;

        let mut cfg = base_config();
        cfg.target = format!("{}/v1/chat/completions", server.uri());
        cfg.stream = true;
        cfg.api_key = Some("sk-openai-test".into());
        let c = OpenaiClient::new(&cfg).unwrap();
        let messages = vec![
            OwnedChatMessage::new("system", "be terse"),
            OwnedChatMessage::new("user", "what is 2+2?"),
            OwnedChatMessage::new("assistant", "4."),
            OwnedChatMessage::new("user", "and 3+3?"),
        ];
        let m = c.send_messages(&messages, 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
        assert!(m.error.is_none());
    }

    /// M6 multi-turn: the unit-level body shape should be a real
    /// multi-message `ChatRequest`, not the single-message form.
    #[test]
    fn build_body_messages_includes_all_roles() {
        use crate::dataset::OwnedChatMessage;
        let cfg = base_config();
        let c = OpenaiClient::new(&cfg).unwrap();
        let messages = vec![
            OwnedChatMessage::new("system", "you are terse"),
            OwnedChatMessage::new("user", "hi"),
            OwnedChatMessage::new("assistant", "hello"),
        ];
        let body = c.build_body_messages(&messages);
        assert_eq!(body.messages.len(), 3);
        assert_eq!(body.messages[0].role, "system");
        assert_eq!(body.messages[1].role, "user");
        assert_eq!(body.messages[2].role, "assistant");
        assert_eq!(body.messages[0].content, "you are terse");
    }
}
