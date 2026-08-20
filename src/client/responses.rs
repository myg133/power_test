//! OpenAI `/v1/responses` client (M9).
//!
//! This is the agent-era endpoint. It differs from chat-completions in:
//!
//! - **Body shape**: `input` instead of `messages`. `input` is either a
//!   string (single text) or a list of typed items
//!   (`{"type":"message","role":"user|assistant|system|developer",
//!     "content":[{"type":"input_text"|"output_text","text":"..."}]}`).
//! - **Streaming events**: typed events on `event:` lines, not just
//!   `data:`. We care about:
//!     - `response.created`           — initial `response.id`
//!     - `response.output_text.delta` — visible text delta
//!     - `response.reasoning_summary_text.delta` — reasoning delta
//!       (o1/o3-style models)
//!     - `response.completed`         — final usage + `response.id`
//! - **Stateful multi-turn**: send `previous_response_id` to continue
//!   an existing server-side conversation without re-sending prior
//!   `input` items. The session pool stores the response id from one
//!   turn and feeds it back as `previous_response_id` on the next.
//!
//! TTFT and ITL treat both `output_text.delta` and
//! `reasoning_summary_text.delta` as token arrivals (a reasoning
//! delta landing before any text delta is still the model's first
//! emitted event). This mirrors the openai.rs treatment of
//! `delta.reasoning_content` for chat-completions.

use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::StreamExt;
use serde::Serialize;

use super::{format_stream_error, format_transport_error, LlmClient, RequestMetrics};
use crate::config::RunConfig;
use crate::dataset::OwnedChatMessage;
use crate::error::Result;

/// Hard cap on the SSE parser's `data:` payload. The Responses API
/// streams one event per `event:`/`data:` pair; the data payload
/// stays small in practice (just a delta string for text deltas),
/// but the `response.completed` event can carry the full response
/// object. 1 MiB is comfortable and well below reqwest's default
/// chunk ceiling. We bail out of further parsing if a single
/// event exceeds this; that would be a server-side bug, not a
/// normal failure mode.
const MAX_EVENT_BYTES: usize = 1024 * 1024;

pub struct ResponsesClient {
    http: reqwest::Client,
    target: String,
    model: String,
    max_tokens: u32,
    stream: bool,
    api_key: Option<String>,
}

impl ResponsesClient {
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

    /// Build a body for a single text prompt. The Responses API
    /// accepts `input` as a bare string for the single-user-text
    /// case, which is the simplest and least error-prone form.
    fn build_body<'a>(&'a self, prompt: &'a str) -> ResponsesRequest<'a> {
        ResponsesRequest {
            model: &self.model,
            input: ResponsesInput::Text(prompt),
            max_output_tokens: self.max_tokens,
            stream: self.stream,
            previous_response_id: None,
        }
    }

    /// Build a body for a multi-turn conversation. Two modes:
    ///
    /// - `previous_response_id` is `Some` → **stateful** path.
    ///   `input` is the latest user message (the new turn's input)
    ///   and the server resumes the prior conversation by id. Prior
    ///   `messages` (assistant turns, prior user turns) are NOT
    ///   re-sent; the server already has them. This is the
    ///   cheap-and-correct way to do multi-turn on the Responses
    ///   API.
    /// - `previous_response_id` is `None` → **stateless** path.
    ///   `input` is the full `messages` array rebuilt as a list of
    ///   typed items. Used on the first turn of a session, or any
    ///   time the caller doesn't have a server-side response id
    ///   (e.g. after a server reset).
    fn build_body_messages<'a>(
        &'a self,
        messages: &'a [OwnedChatMessage],
        previous_response_id: Option<&'a str>,
    ) -> ResponsesRequest<'a> {
        let input = match previous_response_id {
            Some(_) => {
                // Stateful: send only the newest user message. The
                // session always has the just-appended follow-up as
                // its last entry on turns 2+; on turn 1 there's no
                // previous_response_id and we hit the stateless
                // branch below.
                let last = messages
                    .last()
                    .map(|m| InputItem::Message {
                        role: m.role.as_str(),
                        content: vec![InputText::InputText {
                            text: m.content.as_str(),
                        }],
                    })
                    .unwrap_or(InputItem::Message {
                        role: "user",
                        content: vec![InputText::InputText { text: "" }],
                    });
                ResponsesInput::Items(vec![last])
            }
            None => {
                // Stateless: rebuild the full conversation as a
                // list of typed items. Filter out empty content
                // (defensive — the dataset loader shouldn't emit
                // empties, but RustDome's `Vec` constructor doesn't
                // catch it either).
                let items: Vec<InputItem<'_>> = messages
                    .iter()
                    .filter(|m| !m.content.is_empty())
                    .map(|m| {
                        let role = m.role.as_str();
                        // The Responses API distinguishes text the
                        // model received (`input_text`) from text the
                        // model produced (`output_text`). For the
                        // re-built conversation we have to pick the
                        // right one based on the message's role.
                        let content = match role {
                            "assistant" => vec![InputText::OutputText {
                                text: m.content.as_str(),
                            }],
                            _ => vec![InputText::InputText {
                                text: m.content.as_str(),
                            }],
                        };
                        InputItem::Message { role, content }
                    })
                    .collect();
                ResponsesInput::Items(items)
            }
        };
        ResponsesRequest {
            model: &self.model,
            input,
            max_output_tokens: self.max_tokens,
            stream: self.stream,
            previous_response_id,
        }
    }
}

// ---------------------------------------------------------------------------
// Request body serialization
// ---------------------------------------------------------------------------

/// The Responses API request body. `input` is rendered as either a
/// bare string (single text) or a list of typed items (multi-turn
/// or structured input). `previous_response_id` is `None` on
/// first-touch and `Some(_)` on stateful continuations.
#[derive(Debug, Serialize)]
struct ResponsesRequest<'a> {
    model: &'a str,
    input: ResponsesInput<'a>,
    /// The Responses API field is `max_output_tokens`, not
    /// `max_tokens`. We always send it; a 0 value means "no cap"
    /// per the spec.
    max_output_tokens: u32,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<&'a str>,
}

#[derive(Debug, Serialize)]
#[serde(untagged)]
enum ResponsesInput<'a> {
    /// Single bare string. Cheapest form, used for single-turn.
    Text(&'a str),
    /// List of typed input items. Used for multi-turn stateless
    /// (full history) and stateful (latest message only).
    Items(Vec<InputItem<'a>>),
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputItem<'a> {
    /// A role-tagged message in the conversation. `content` is
    /// always a list of typed parts (the API spec requires an
    /// array even for single-text messages). power_test's dataset
    /// format is single-text-per-message, so the list always has
    /// exactly one element; M9 doesn't carry images / files.
    Message {
        role: &'a str,
        content: Vec<InputText<'a>>,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum InputText<'a> {
    /// Text the model received (user/developer/system role).
    /// Serializes as `{"type":"input_text","text":"..."}`.
    InputText { text: &'a str },
    /// Text the model produced (assistant role). The Responses API
    /// requires this distinction when re-sending prior turns.
    /// Serializes as `{"type":"output_text","text":"..."}`.
    OutputText { text: &'a str },
}

// ---------------------------------------------------------------------------
// Streaming parser
// ---------------------------------------------------------------------------

impl ResponsesClient {
    async fn parse_stream(&self, resp: reqwest::Response, start: Instant, m: &mut RequestMetrics) {
        // M9.x: capture status + headers BEFORE moving the response
        // into `bytes_stream()`. Used by `format_stream_error` to
        // attach Content-Encoding / Transfer-Encoding / source chain
        // to the error string.
        let status = resp.status();
        let headers = resp.headers().clone();
        let mut stream = resp.bytes_stream();
        let mut parser = ResponsesSseParser::new();
        let mut first_token_at: Option<Duration> = None;
        let mut last_token_at: Option<Duration> = None;
        let mut delta_count: u32 = 0;
        let mut usage_completion: Option<u32> = None;
        let mut usage_prompt: Option<u32> = None;
        // M9.x: track total bytes received for the error report.
        let mut bytes_received: usize = 0;
        let mut response_text = String::new();

        while let Some(chunk_result) = stream.next().await {
            let chunk = match chunk_result {
                Ok(c) => c,
                Err(e) => {
                    m.error = Some(format_stream_error(
                        &e,
                        status.as_u16(),
                        &headers,
                        bytes_received,
                    ));
                    break;
                }
            };
            bytes_received += chunk.len();
            for ev in parser.feed(&chunk) {
                if ev.event_type.is_empty() && ev.data.trim().is_empty() {
                    continue;
                }
                // The Responses API uses typed events: the event
                // type on the `event:` line is the routing key,
                // the JSON is on the `data:` line. We only react
                // to a small set of types.
                let parsed: serde_json::Value = match serde_json::from_str(&ev.data) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let now = start.elapsed();

                match ev.event_type.as_str() {
                    "response.created" => {
                        // The first event. We extract `response.id`
                        // so the session pool can reuse it as
                        // `previous_response_id` on the next turn.
                        // Stash it on `m.response_id` immediately so
                        // it's available even if the stream is
                        // truncated before `response.completed`.
                        if m.response_id.is_none() {
                            if let Some(id) = parsed
                                .get("response")
                                .and_then(|r| r.get("id"))
                                .and_then(|v| v.as_str())
                            {
                                m.response_id = Some(id.to_string());
                            }
                        }
                    }
                    "response.completed" => {
                        // Final event. Carries the canonical
                        // response object (with `id` and `usage`).
                        // We prefer this id over the one from
                        // `response.created` because some
                        // implementations only fill it here.
                        if let Some(resp_obj) = parsed.get("response") {
                            if m.response_id.is_none() {
                                if let Some(id) =
                                    resp_obj.get("id").and_then(|v| v.as_str())
                                {
                                    m.response_id = Some(id.to_string());
                                }
                            }
                            if let Some(u) = resp_obj.get("usage") {
                                if let Some(ct) = u
                                    .get("output_tokens")
                                    .and_then(|v| v.as_u64())
                                {
                                    usage_completion = Some(ct as u32);
                                }
                                if let Some(pt) =
                                    u.get("input_tokens").and_then(|v| v.as_u64())
                                {
                                    usage_prompt = Some(pt as u32);
                                }
                                // The Responses API puts cached
                                // tokens at `input_tokens_details.cached_tokens`
                                // (same shape as chat-completions,
                                // different parent).
                                if let Some(cached) = u
                                    .get("input_tokens_details")
                                    .and_then(|d| d.get("cached_tokens"))
                                    .and_then(|v| v.as_u64())
                                {
                                    m.cache_hit_input_tokens = cached as u32;
                                }
                            }
                        }
                    }
                    // Visible text delta. Counts as a "token" for
                    // ITL and as the source of `response_text` for
                    // session back-fill.
                    "response.output_text.delta" => {
                        if let Some(d) =
                            parsed.get("delta").and_then(|v| v.as_str())
                        {
                            if !d.is_empty() {
                                response_text.push_str(d);
                                bump_token_timing(
                                    &mut first_token_at,
                                    &mut last_token_at,
                                    &mut delta_count,
                                    &mut m.itl_samples,
                                    now,
                                );
                            }
                        }
                    }
                    // Reasoning delta. Same TTFT/ITL rules as
                    // `delta.reasoning_content` on chat-completions:
                    // a reasoning delta landing first is still the
                    // model's first emitted event, so it sets TTFT.
                    "response.reasoning_summary_text.delta" => {
                        if let Some(d) =
                            parsed.get("delta").and_then(|v| v.as_str())
                        {
                            if !d.is_empty() {
                                bump_token_timing(
                                    &mut first_token_at,
                                    &mut last_token_at,
                                    &mut delta_count,
                                    &mut m.itl_samples,
                                    now,
                                );
                            }
                        }
                    }
                    _ => {
                        // Ignore other event types
                        // (`response.in_progress`, `response.output_item.added`,
                        // `response.content_part.added`,
                        // `response.output_text.done`, etc.). They
                        // don't carry per-token timing data.
                    }
                }
            }
        }

        m.ttft = first_token_at;
        m.completion_tokens = usage_completion.unwrap_or(delta_count);
        m.estimated = usage_completion.is_none();
        m.prompt_tokens = usage_prompt.unwrap_or(0);
        m.response_text = response_text;
        m.total_duration = start.elapsed();
    }

    async fn parse_single(
        &self,
        resp: reqwest::Response,
        start: Instant,
        m: &mut RequestMetrics,
    ) {
        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                m.error = Some(format!("body parse: {e}"));
                m.total_duration = start.elapsed();
                return;
            }
        };
        // The non-streaming Responses API returns a single
        // response object with `id` and `output` (array of items).
        // Each output item with `type: "message"` has a `content`
        // array; each content part with `type: "output_text"` has
        // a `text` field.
        if let Some(id) = body.get("id").and_then(|v| v.as_str()) {
            m.response_id = Some(id.to_string());
        }
        let mut joined = String::new();
        if let Some(output) = body.get("output").and_then(|v| v.as_array()) {
            for item in output {
                if item.get("type").and_then(|v| v.as_str()) != Some("message") {
                    continue;
                }
                if let Some(content) =
                    item.get("content").and_then(|v| v.as_array())
                {
                    for part in content {
                        if part.get("type").and_then(|v| v.as_str())
                            != Some("output_text")
                        {
                            continue;
                        }
                        if let Some(text) =
                            part.get("text").and_then(|v| v.as_str())
                        {
                            if !joined.is_empty() {
                                joined.push('\n');
                            }
                            joined.push_str(text);
                            m.completion_tokens +=
                                crate::config::estimate_tokens(text);
                        }
                    }
                }
            }
        }
        m.response_text = joined;
        m.estimated = true;
        if let Some(u) = body.get("usage") {
            if let Some(ct) = u.get("output_tokens").and_then(|v| v.as_u64()) {
                m.completion_tokens = ct as u32;
                m.estimated = false;
            }
            if let Some(pt) = u.get("input_tokens").and_then(|v| v.as_u64()) {
                m.prompt_tokens = pt as u32;
            }
            if let Some(cached) = u
                .get("input_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(|v| v.as_u64())
            {
                m.cache_hit_input_tokens = cached as u32;
            }
        }
        let total = start.elapsed();
        m.ttft = Some(total); // non-streaming: TTFT == total
        m.total_duration = total;
    }
}

/// Update the running TTFT / ITL state on a token-arrival event.
/// Extracted so both `output_text.delta` and
/// `reasoning_summary_text.delta` paths share the same timing
/// arithmetic.
fn bump_token_timing(
    first_token_at: &mut Option<Duration>,
    last_token_at: &mut Option<Duration>,
    delta_count: &mut u32,
    itl_samples: &mut Vec<Duration>,
    now: Duration,
) {
    *delta_count += 1;
    if first_token_at.is_none() {
        *first_token_at = Some(now);
    } else if let Some(last) = *last_token_at {
        let gap = now.checked_sub(last).unwrap_or(Duration::ZERO);
        itl_samples.push(gap);
    }
    *last_token_at = Some(now);
}

// ---------------------------------------------------------------------------
// LlmClient trait
// ---------------------------------------------------------------------------

#[async_trait]
impl LlmClient for ResponsesClient {
    async fn send(&self, prompt: &str, _estimated_prompt_tokens: u32) -> RequestMetrics {
        let body = self.build_body(prompt);
        self.dispatch(body).await
    }

    async fn send_messages(
        &self,
        messages: &[OwnedChatMessage],
        previous_response_id: Option<&str>,
        _estimated_prompt_tokens: u32,
    ) -> RequestMetrics {
        let body = self.build_body_messages(messages, previous_response_id);
        self.dispatch(body).await
    }
}

impl ResponsesClient {
    /// Shared send path. Identical shape to openai.rs: build body,
    /// send, dispatch on stream vs non-stream, fill in
    /// `m.finished_at`.
    async fn dispatch(&self, body: ResponsesRequest<'_>) -> RequestMetrics {
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
                m.error = Some(format_transport_error(&e));
                m.total_duration = start.elapsed();
                m.finished_at = chrono::Utc::now();
                return m;
            }
        };

        m.status = resp.status().as_u16();
        if !resp.status().is_success() {
            let snippet = resp.text().await.unwrap_or_default();
            m.error = Some(format!(
                "HTTP {}: {}",
                m.status,
                truncate(&snippet, 200)
            ));
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

// ---------------------------------------------------------------------------
// SSE parser (typed events: `event:` + `data:`)
// ---------------------------------------------------------------------------

/// One SSE event with both the `event:` type and the `data:`
/// payload. We need both because the Responses API uses the
/// `event:` line for routing.
struct ResponsesSseEvent {
    event_type: String,
    data: String,
}

/// Streaming SSE parser tailored for the Responses API. The
/// Responses API emits lines like:
///
/// ```text
/// event: response.output_text.delta
/// data: {"type":"response.output_text.delta","delta":"hi"}
///
/// ```
///
/// i.e. with both `event:` and `data:` lines per event. Chat-
/// completions only uses `data:`. We parse both for completeness,
/// falling back to a `data:`-only form for compatibility.
struct ResponsesSseParser {
    buffer: Vec<u8>,
    current_event: Option<String>,
    current_data: String,
}

impl ResponsesSseParser {
    fn new() -> Self {
        Self {
            buffer: Vec::new(),
            current_event: None,
            current_data: String::new(),
        }
    }

    fn feed(&mut self, chunk: &[u8]) -> Vec<ResponsesSseEvent> {
        self.buffer.extend_from_slice(chunk);
        let mut events = Vec::new();
        loop {
            match find_line(&self.buffer) {
                Some((line_end, sep_len)) => {
                    let bytes: Vec<u8> = self.buffer.drain(..line_end).collect();
                    self.buffer.drain(..sep_len);
                    let line = String::from_utf8_lossy(&bytes);
                    self.process_line(&line, &mut events);
                }
                None => break,
            }
        }
        events
    }

    fn process_line(&mut self, line: &str, out: &mut Vec<ResponsesSseEvent>) {
        if line.is_empty() {
            // Empty line = event boundary. Flush if we have data.
            if !self.current_data.is_empty() || self.current_event.is_some() {
                if self.current_data.len() > MAX_EVENT_BYTES {
                    // Server bug: a single event's data grew past
                    // 1 MiB. Stop processing; the next event
                    // boundary would have flushed anyway.
                    self.current_data.clear();
                    self.current_event = None;
                    return;
                }
                out.push(ResponsesSseEvent {
                    event_type: self.current_event.take().unwrap_or_default(),
                    data: std::mem::take(&mut self.current_data),
                });
            }
            return;
        }
        if let Some(rest) = line.strip_prefix("event: ") {
            self.current_event = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("event:") {
            self.current_event = Some(rest.to_string());
        } else if let Some(rest) = line.strip_prefix("data: ") {
            if !self.current_data.is_empty() {
                self.current_data.push('\n');
            }
            self.current_data.push_str(rest);
        } else if let Some(rest) = line.strip_prefix("data:") {
            if !self.current_data.is_empty() {
                self.current_data.push('\n');
            }
            self.current_data.push_str(rest);
        } else if line.starts_with(':') {
            // SSE comment line — ignore.
        }
        // Other lines (`id:`, `retry:`) are not used by the
        // Responses API; ignore them.
    }
}

/// Find the next `\n` (or `\r\n`) in the buffer. Returns
/// `(line_end, sep_len)` where `line_end` is the index of `\n`
/// and `sep_len` is 1 for `\n` or 2 for `\r\n`.
fn find_line(buf: &[u8]) -> Option<(usize, usize)> {
    for (i, b) in buf.iter().enumerate() {
        if *b == b'\n' {
            return Some((i, 1));
        }
        if *b == b'\r' && buf.get(i + 1) == Some(&b'\n') {
            return Some((i, 2));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_parser_typed_event() {
        let mut p = ResponsesSseParser::new();
        let evs = p.feed(
            b"event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\n",
        );
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, "response.output_text.delta");
        assert_eq!(evs[0].data, "{\"delta\":\"hi\"}");
    }

    #[test]
    fn sse_parser_data_only_event() {
        // Tolerate data-only events (chat-completions style).
        let mut p = ResponsesSseParser::new();
        let evs = p.feed(b"data: {\"x\":1}\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, "");
        assert_eq!(evs[0].data, "{\"x\":1}");
    }

    #[test]
    fn sse_parser_split_across_chunks() {
        let mut p = ResponsesSseParser::new();
        let evs1 = p.feed(b"event: response.create");
        assert!(evs1.is_empty());
        let evs2 = p.feed(b"d\ndata: {\"a\":1}\n\n");
        assert_eq!(evs2.len(), 1);
        assert_eq!(evs2[0].event_type, "response.created");
        assert_eq!(evs2[0].data, "{\"a\":1}");
    }

    #[test]
    fn sse_parser_multiple_events_in_one_chunk() {
        let mut p = ResponsesSseParser::new();
        let evs = p.feed(
            b"event: a\ndata: 1\n\nevent: b\ndata: 2\n\n",
        );
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event_type, "a");
        assert_eq!(evs[0].data, "1");
        assert_eq!(evs[1].event_type, "b");
        assert_eq!(evs[1].data, "2");
    }

    #[test]
    fn sse_parser_ignores_comment_lines() {
        let mut p = ResponsesSseParser::new();
        let evs = p.feed(b": keepalive\ndata: 1\n\n");
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].data, "1");
    }

    #[test]
    fn sse_parser_crlf_endings() {
        let mut p = ResponsesSseParser::new();
        let evs = p.feed(
            b"event: a\r\ndata: 1\r\n\r\n",
        );
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].event_type, "a");
        assert_eq!(evs[0].data, "1");
    }

    // -----------------------------------------------------------------
    // Wiremock integration tests
    // -----------------------------------------------------------------

    use crate::config::{
        ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
        RunConfig,
    };
    use crate::dataset::OwnedChatMessage;

    fn base_config(target: String) -> RunConfig {
        RunConfig {
            run_id: "test".into(),
            target,
            api: ApiKind::Responses,
            model: "responses-test-model".into(),
            prompt: PromptSource::Literal { text: "hi".into() },
            dataset: DatasetSpec::Literal { text: "hi".into() },
            strategy: RequestStrategy::Random,
            prompt_distribution: PromptDistribution::from_single(1),
            pattern: LoadPattern::Constant { rps: 1.0 },
            max_tokens: 64,
            stream: true,
            target_rps: 1.0,
            duration_secs: 1,
            concurrency: 4,
            tag: None,
            api_key: Some("sk-responses-test".into()),
            started_at: chrono::Local::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
            thinking_disabled: false,
        max_requests: None,
        }
    }

    /// M9: a single-turn streaming request with one
    /// `output_text.delta` event must record TTFT, ITL, completion
    /// tokens (from the final usage), and the streamed `response.id`
    /// on `m.response_id`.
    #[tokio::test]
    async fn streaming_response_with_text_delta_records_ttft_and_response_id() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = concat!(
            "event: response.created\n",
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"resp_abc123\"}}\n",
            "\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n",
            "\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_abc123\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n",
            "\n",
        );
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(header("authorization", "Bearer sk-responses-test"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let cfg = base_config(format!("{}/v1/responses", server.uri()));
        let c = ResponsesClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
        assert!(m.error.is_none());
        assert!(m.ttft.is_some(), "TTFT must be set after a delta");
        assert_eq!(m.response_text, "ok");
        assert_eq!(m.completion_tokens, 1, "usage wins over delta count");
        assert!(!m.estimated);
        assert_eq!(m.prompt_tokens, 3);
        assert_eq!(
            m.response_id.as_deref(),
            Some("resp_abc123"),
            "response_id from response.created must be captured"
        );
    }

    /// M9: a reasoning delta landing before any text delta must
    /// still set TTFT. Same rule as the chat-completions
    /// `delta.reasoning_content` path. ITL spans the
    /// reasoning→content boundary.
    #[tokio::test]
    async fn streaming_with_reasoning_then_text_counts_reasoning_as_ttft() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = concat!(
            "event: response.reasoning_summary_text.delta\n",
            "data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"thinking\"}\n",
            "\n",
            "event: response.output_text.delta\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n",
            "\n",
            "event: response.completed\n",
            "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\",\"usage\":{\"input_tokens\":2,\"output_tokens\":1}}}\n",
            "\n",
        );
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let cfg = base_config(format!("{}/v1/responses", server.uri()));
        let c = ResponsesClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
        assert!(m.ttft.is_some());
        // Both deltas land — response_text is the visible text only.
        assert_eq!(m.response_text, "ok");
        // ITL should have 1 sample (the reasoning→content gap).
        assert_eq!(m.itl_samples.len(), 1);
    }

    /// M9: the multi-turn `send_messages` body must carry
    /// `previous_response_id` when supplied, and only the latest
    /// user message as `input` (not the full conversation). The
    /// server-side conversation is implied by the id.
    #[tokio::test]
    async fn stateful_multi_turn_body_uses_previous_response_id() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_partial_json(
                serde_json::json!({
                    "previous_response_id": "resp_turn1",
                    "input": [{
                        "type": "message",
                        "role": "user",
                        "content": [{
                            "type": "input_text",
                            "text": "second-question",
                        }],
                    }],
                }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: response.completed\n\
                         data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_turn2\",\"usage\":{\"input_tokens\":5,\"output_tokens\":2}}}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let cfg = base_config(format!("{}/v1/responses", server.uri()));
        let c = ResponsesClient::new(&cfg).unwrap();
        let messages = vec![
            OwnedChatMessage::new("user", "first-question"),
            OwnedChatMessage::new("assistant", "first-answer"),
            OwnedChatMessage::new("user", "second-question"),
        ];
        let m = c.send_messages(&messages, Some("resp_turn1"), 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
        assert_eq!(m.response_id.as_deref(), Some("resp_turn2"));
    }

    /// M9: without `previous_response_id`, multi-turn must rebuild
    /// the full conversation as `input` items. Assistant messages
    /// use `output_text` (the model-produced kind), user messages
    /// use `input_text`.
    #[tokio::test]
    async fn stateless_multi_turn_body_rebuilds_full_input_items() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_partial_json(
                serde_json::json!({
                    "input": [
                        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "u1"}]},
                        {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "a1"}]},
                        {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "u2"}]},
                    ],
                }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: response.completed\n\
                         data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\",\"usage\":{\"input_tokens\":5,\"output_tokens\":1}}}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let cfg = base_config(format!("{}/v1/responses", server.uri()));
        let c = ResponsesClient::new(&cfg).unwrap();
        let messages = vec![
            OwnedChatMessage::new("user", "u1"),
            OwnedChatMessage::new("assistant", "a1"),
            OwnedChatMessage::new("user", "u2"),
        ];
        let m = c.send_messages(&messages, None, 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
    }

    /// M9: single-turn `send` must emit a bare-string `input` (the
    /// simplest form), not a list of items.
    #[tokio::test]
    async fn single_turn_input_is_bare_string() {
        use wiremock::matchers::{body_partial_json, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .and(body_partial_json(
                serde_json::json!({
                    "input": "what is 2+2?",
                }),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(
                        "event: response.completed\n\
                         data: {\"type\":\"response.completed\",\"response\":{\"id\":\"resp_x\",\"usage\":{\"input_tokens\":3,\"output_tokens\":1}}}\n\n",
                    ),
            )
            .mount(&server)
            .await;

        let cfg = base_config(format!("{}/v1/responses", server.uri()));
        let c = ResponsesClient::new(&cfg).unwrap();
        let m = c.send("what is 2+2?", 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
    }

    /// M9: non-streaming response with `output` array must
    /// extract text from `output[].content[].text` and read
    /// `output_tokens` from the response-level `usage`.
    #[tokio::test]
    async fn non_streaming_response_extracts_text_and_usage() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        let body = serde_json::json!({
            "id": "resp_single",
            "output": [{
                "type": "message",
                "content": [{
                    "type": "output_text",
                    "text": "single answer",
                }],
            }],
            "usage": {
                "input_tokens": 5,
                "output_tokens": 2,
            },
        })
        .to_string();
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "application/json")
                    .set_body_string(body),
            )
            .mount(&server)
            .await;

        let mut cfg = base_config(format!("{}/v1/responses", server.uri()));
        cfg.stream = false;
        let c = ResponsesClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 200, "err={:?}", m.error);
        assert_eq!(m.response_text, "single answer");
        assert_eq!(m.completion_tokens, 2);
        assert_eq!(m.prompt_tokens, 5);
        assert!(!m.estimated);
        assert_eq!(m.response_id.as_deref(), Some("resp_single"));
    }
}
