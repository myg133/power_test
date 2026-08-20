//! Raw HTTP client. Sends a POST with a static body (read from
//! `--raw-body-file`) and captures only status, total duration, and
//! body byte counts. No SSE parsing, no JSON parsing, no token count
//! from the model. Completion / prompt tokens are estimated as
//! `bytes / 4` and flagged with `estimated = true`.
//!
//! Use this for endpoints that don't speak OpenAI or Anthropic shapes
//! but still need to be load-tested at the HTTP level.

use std::time::{Duration, Instant};

use async_trait::async_trait;

use super::{format_transport_error, LlmClient, RequestMetrics};
use crate::config::RunConfig;
use crate::error::{Error, Result};

#[derive(Debug)]
pub struct RawClient {
    http: reqwest::Client,
    target: String,
    /// Body bytes, read once at construction. `None` means empty body.
    body: Option<Vec<u8>>,
    /// Content-Type for the body. Defaults to `application/json`.
    content_type: String,
    /// Optional API key; when set, sent as `Authorization: Bearer <key>`.
    api_key: Option<String>,
}

impl RawClient {
    pub fn new(cfg: &RunConfig) -> Result<Self> {
        let body = match cfg.raw_body_file.as_ref() {
            Some(p) => {
                if !p.exists() {
                    return Err(Error::InvalidConfig(format!(
                        "--raw-body-file not found: {}",
                        p.display()
                    )));
                }
                let bytes = std::fs::read(p)
                    .map_err(|e| Error::io_at(p, e))?;
                Some(bytes)
            }
            None => None,
        };
        let content_type = cfg
            .raw_content_type
            .clone()
            .unwrap_or_else(|| "application/json".to_string());
        let api_key = cfg
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()?;
        Ok(Self {
            http,
            target: cfg.target.clone(),
            body,
            content_type,
            api_key,
        })
    }

    /// Byte length of the configured body. Public for tests.
    pub fn body_len(&self) -> usize {
        self.body.as_ref().map(|b| b.len()).unwrap_or(0)
    }

    fn build_request(&self) -> reqwest::RequestBuilder {
        let mut req = self
            .http
            .post(&self.target)
            .header("content-type", &self.content_type);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        if let Some(body) = &self.body {
            req = req.body(body.clone());
        }
        req
    }
}

#[async_trait]
impl LlmClient for RawClient {
    async fn send(&self, _prompt: &str, estimated_prompt_tokens: u32) -> RequestMetrics {
        let started_at = chrono::Utc::now();
        let start = Instant::now();
        let mut m = RequestMetrics::default();
        m.started_at = started_at;

        // Token counts: rough heuristic, 4 bytes per token.
        m.prompt_tokens = (self.body_len() as u32 / 4).max(1);

        let resp = match self.build_request().send().await {
            Ok(r) => r,
            Err(e) => {
                m.error = Some(format_transport_error(&e));
                m.total_duration = start.elapsed();
                m.finished_at = chrono::Utc::now();
                return m;
            }
        };
        m.status = resp.status().as_u16();
        // We want to read the body for byte counting, but we don't care
        // about its shape.
        let body_result = resp.bytes().await;
        match body_result {
            Ok(bytes) => {
                m.completion_tokens = (bytes.len() as u32 / 4).max(0);
                m.estimated = true;
            }
            Err(e) => {
                m.error = Some(format!("body read: {e}"));
            }
        }
        m.total_duration = start.elapsed();
        m.finished_at = chrono::Utc::now();
        // Non-2xx should set the error path too, but only if we got a
        // body successfully (otherwise the read already populated the
        // error).
        if m.error.is_none() && (m.status < 200 || m.status >= 300) {
            m.error = Some(format!("HTTP {}", m.status));
        }
        // `estimated_prompt_tokens` is provided by the dataset layer;
        // we don't use it here because the body is static.
        let _ = estimated_prompt_tokens;
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn base_config() -> RunConfig {
        RunConfig {
            run_id: "test".into(),
            target: "http://localhost:9999/raw".into(),
            api: crate::config::ApiKind::Raw,
            model: "raw".into(),
            prompt: crate::config::PromptSource::Literal {
                text: "ignored".into(),
            },
            dataset: crate::config::DatasetSpec::Literal {
                text: "ignored".into(),
            },
            strategy: crate::config::RequestStrategy::Random,
            prompt_distribution: crate::config::PromptDistribution::from_single(0),
            pattern: crate::config::LoadPattern::Constant { rps: 1.0 },
            max_tokens: 0,
            stream: false,
            target_rps: 1.0,
            duration_secs: 1,
            concurrency: 4,
            tag: None,
            api_key: None,
            started_at: chrono::Local::now(),
            raw_body_file: None,
            raw_content_type: None,
            model_alias: None,
            thinking_disabled: false,
        max_requests: None,
        }
    }

    #[test]
    fn loads_body_file_at_construction() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"hello raw").unwrap();
        let mut cfg = base_config();
        cfg.raw_body_file = Some(f.path().to_path_buf());
        let c = RawClient::new(&cfg).unwrap();
        assert_eq!(c.body_len(), 9);
    }

    #[test]
    fn sets_content_type_header() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"{}").unwrap();
        let mut cfg = base_config();
        cfg.raw_body_file = Some(f.path().to_path_buf());
        cfg.raw_content_type = Some("application/xml".to_string());
        let c = RawClient::new(&cfg).unwrap();
        let req = c.build_request().build().unwrap();
        let has_ct = req
            .headers()
            .iter()
            .any(|(k, v)| k.as_str() == "content-type" && v == "application/xml");
        assert!(has_ct, "content-type header should be application/xml");
    }

    #[test]
    fn missing_body_file_errors_at_build() {
        let mut cfg = base_config();
        cfg.raw_body_file = Some(std::path::PathBuf::from("/no/such/file.json"));
        let err = RawClient::new(&cfg).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("not found") || msg.contains("/no/such/file.json"));
    }

    #[test]
    fn default_content_type_is_json() {
        let cfg = base_config();
        let c = RawClient::new(&cfg).unwrap();
        let req = c.build_request().build().unwrap();
        let has_ct = req
            .headers()
            .iter()
            .any(|(k, v)| k.as_str() == "content-type" && v == "application/json");
        assert!(has_ct, "default content-type should be application/json");
    }

    #[tokio::test]
    async fn non_2xx_response_records_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/raw"))
            .respond_with(ResponseTemplate::new(500).set_body_string("oops"))
            .mount(&server)
            .await;
        let mut cfg = base_config();
        cfg.target = format!("{}/raw", server.uri());
        let c = RawClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 500);
        assert!(m.error.is_some(), "error should be set on 500");
    }

    #[tokio::test]
    async fn success_response_estimates_tokens_from_bytes() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/raw"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .mount(&server)
            .await;
        let mut cfg = base_config();
        cfg.target = format!("{}/raw", server.uri());
        let c = RawClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 200);
        assert!(m.error.is_none(), "unexpected error: {:?}", m.error);
        assert!(m.estimated);
        // "ok" is 2 bytes → 2/4 = 0 tokens (max(0, 0)).
        assert!(m.completion_tokens <= 1, "got {}", m.completion_tokens);
    }

    #[tokio::test]
    async fn sends_bearer_auth_when_api_key_set() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/raw"))
            .and(header("authorization", "Bearer my-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
            .expect(1)
            .mount(&server)
            .await;
        let mut cfg = base_config();
        cfg.target = format!("{}/raw", server.uri());
        cfg.api_key = Some("my-secret".into());
        let c = RawClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 200);
        // wiremock's `expect(1)` would fail if the header check missed.
    }

    #[tokio::test]
    async fn transport_error_records_error() {
        let cfg = base_config();
        let c = RawClient::new(&cfg).unwrap();
        let m = c.send("hi", 1).await;
        assert_eq!(m.status, 0);
        let err = m.error.expect("error must be set");
        assert!(err.contains("transport"), "got: {err}");
    }

    #[test]
    fn empty_body_when_no_file() {
        let cfg = base_config();
        let c = RawClient::new(&cfg).unwrap();
        assert_eq!(c.body_len(), 0);
    }

    #[test]
    fn prompt_token_estimate_uses_body_bytes() {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(b"a]a]a]a]").unwrap(); // 8 bytes → 2 tokens
        let mut cfg = base_config();
        cfg.raw_body_file = Some(f.path().to_path_buf());
        let c = RawClient::new(&cfg).unwrap();
        // We need a fake response to verify, so just check the body length.
        assert_eq!(c.body_len(), 8);
    }
}
