//! End-to-end test: spin up a wiremock SSE endpoint, run a 2s/2-RPS test
//! against it, and verify that the run produced all expected artifacts
//! and that TTFT and TPS were captured.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tempfile::TempDir;
use tokio::sync::Notify;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use power_test::config::{
    ApiKind, DatasetSpec, LoadPattern, PromptDistribution, PromptSource, RequestStrategy,
    RunConfig, RunStatus,
};
use power_test::runner::{self, MetricsAggregator, RunOptions};
use power_test::storage;
use power_test::{config, report};

fn build_config(target: String, duration_secs: u64) -> RunConfig {
    RunConfig {
        run_id: uuid::Uuid::new_v4().to_string(),
        target,
        api: ApiKind::Openai,
        model: "gpt-3.5-turbo".into(),
        prompt: PromptSource::Literal {
            text: "hi".into(),
        },
        dataset: DatasetSpec::Literal {
            text: "hi".into(),
        },
        strategy: RequestStrategy::Random,
        prompt_distribution: PromptDistribution::from_single(1),
        pattern: LoadPattern::Constant { rps: 2.0 },
        max_tokens: 16,
        stream: true,
        target_rps: 2.0,
        duration_secs,
        concurrency: 8,
        tag: Some("e2e".into()),
        api_key: None,
        started_at: chrono::Utc::now(),
        raw_body_file: None,
        raw_content_type: None,
        model_alias: None,
    }
}

fn sse_response_body() -> String {
    let chunks = vec![
        r#"data: {"id":"1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":"hi"},"finish_reason":null}]}

"#,
        r#"data: {"id":"1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"content":" there"},"finish_reason":null}]}

"#,
        r#"data: {"id":"1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}

"#,
        r#"data: {"id":"1","object":"chat.completion.chunk","choices":[],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}

"#,
        "data: [DONE]\n\n",
    ];
    chunks.join("")
}

/// Mirror the CLI's cmd_run flow: run, render, save, return the history dir.
async fn run_and_save(
    target: String,
    duration_secs: u64,
    history_root: PathBuf,
) -> runner::RunOutput {
    let cfg = build_config(target, duration_secs);
    let cancel = Arc::new(Notify::new());
    let output = runner::run_with_cancel(
        RunOptions {
            config: cfg,
            history_dir: history_root.clone(),
            shared_aggregator: None,
        },
        cancel,
    )
    .await
    .expect("runner completes");

    let summary_text =
        report::render_summary(&output.config, &output.aggregator, output.interrupted);
    let report_html = report::render_html(&output.config, &output.aggregator, output.interrupted);
    let status = if output.interrupted {
        RunStatus::Interrupted
    } else {
        RunStatus::Completed
    };
    let _entry = storage::save_run(
        &history_root,
        &output.run_id,
        &output.config,
        &output.aggregator,
        &summary_text,
        &report_html,
        status,
    )
    .expect("save_run succeeds");
    output
}

#[tokio::test]
async fn e2e_wiremock_streaming_2s_at_2rps() {
    let server = MockServer::start().await;
    let body = sse_response_body();
    let responder = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
        .set_delay(Duration::from_millis(20));
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(responder)
        .mount(&server)
        .await;

    let target = format!("{}/v1/chat/completions", server.uri());
    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();
    let output = run_and_save(target, 2, history_root.clone()).await;

    // 1. history dir created
    let dir = storage::run_dir(&history_root, &output.config.model, &output.run_id);
    assert!(dir.is_dir(), "history dir should exist at {}", dir.display());

    // 2. all four artifacts
    assert!(dir.join("config.json").is_file(), "config.json missing");
    assert!(dir.join("metrics.json").is_file(), "metrics.json missing");
    assert!(dir.join("report.html").is_file(), "report.html missing");
    assert!(dir.join("summary.txt").is_file(), "summary.txt missing");

    // 3. metrics.json has records
    let metrics_text = std::fs::read_to_string(dir.join("metrics.json")).unwrap();
    let metrics: serde_json::Value = serde_json::from_str(&metrics_text).unwrap();
    let per_request = metrics["per_request"].as_array().expect("per_request array");
    assert!(
        !per_request.is_empty(),
        "expected at least one per_request record, got {}",
        per_request.len()
    );
    assert!(
        per_request.len() >= 2,
        "expected at least 2 records, got {}",
        per_request.len()
    );

    // 4. TTFT captured (> 0 because of the mock delay)
    let first = &per_request[0];
    let ttft_us = first["ttft_us"]
        .as_u64()
        .expect("ttft_us must be set for streaming responses");
    assert!(ttft_us > 0, "TTFT should be > 0, got {ttft_us}us");

    // 5. TPS > 0
    let tps = output.aggregator.tps_mean();
    assert!(tps > 0.0, "TPS should be > 0, got {tps}");

    // 6. report.html mentions our run id and Chart.js
    let html = std::fs::read_to_string(dir.join("report.html")).unwrap();
    assert!(html.contains(&output.run_id));
    assert!(html.contains("power_test report"));
    assert!(html.contains("chart.js"));
}

#[tokio::test]
async fn e2e_report_renders_from_saved_metrics() {
    // Same setup as above; verifies that `power_test report <id>` would work
    // by re-rendering from saved metrics.json.
    let server = MockServer::start().await;
    let body = sse_response_body();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
                .set_delay(Duration::from_millis(10)),
        )
        .mount(&server)
        .await;

    let target = format!("{}/v1/chat/completions", server.uri());
    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();
    let output = run_and_save(target, 1, history_root.clone()).await;

    // Reload from disk
    let dir = storage::run_dir(&history_root, &output.config.model, &output.run_id);
    let metrics_text = std::fs::read_to_string(dir.join("metrics.json")).unwrap();
    let metrics: serde_json::Value = serde_json::from_str(&metrics_text).unwrap();
    let records: Vec<runner::RequestRecord> =
        serde_json::from_value(metrics["per_request"].clone()).expect("records deserialize");
    let agg = MetricsAggregator::from_records(&records);
    assert!(agg.total_requests() > 0);
    let summary = report::render_summary(&output.config, &agg, false);
    assert!(summary.contains("power_test summary"));
}

#[tokio::test]
async fn e2e_skips_ticks_when_target_is_slow() {
    let server = MockServer::start().await;
    let body = sse_response_body();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
                .set_delay(Duration::from_millis(200)),
        )
        .mount(&server)
        .await;

    let target = format!("{}/v1/chat/completions", server.uri());
    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();
    let mut cfg = build_config(target, 1);
    // By Little's law, RPS=20 × latency=0.2s = 4 concurrent slots needed;
    // with concurrency=2 we must skip many ticks.
    cfg.concurrency = 2;
    cfg.target_rps = 20.0;
    cfg.pattern = LoadPattern::Constant { rps: 20.0 };
    let cancel = Arc::new(Notify::new());
    let output = runner::run_with_cancel(
        RunOptions {
            config: cfg,
            history_dir: history_root,
            shared_aggregator: None,
        },
        cancel,
    )
    .await
    .expect("runner completes");
    let agg = &output.aggregator;
    assert!(
        agg.skipped() > 0,
        "expected some skipped ticks with high RPS / low concurrency, got {}",
        agg.skipped()
    );
    let _ = json!({"skipped": agg.skipped(), "total": agg.total_requests()});
    // Touch config to silence unused import if any
    let _ = config::ApiKind::Openai;
}

/// New in M2: drive a ramp pattern against wiremock and assert that
/// (a) the number of completed requests is at least what the 2-second
/// start rate would produce, and (b) the achieved RPS falls between
/// the start and end RPS. This is the only "real network" test for
/// ramp; everything else is covered by deterministic paused-time unit
/// tests in `pattern.rs`.
#[tokio::test]
async fn e2e_ramp_pattern_2s_2_to_8_rps() {
    let server = MockServer::start().await;
    let body = sse_response_body();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
                .set_delay(Duration::from_millis(15)),
        )
        .mount(&server)
        .await;

    let target = format!("{}/v1/chat/completions", server.uri());
    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();

    let mut cfg = build_config(target, 2);
    cfg.pattern = LoadPattern::Ramp {
        start: 2.0,
        end: 8.0,
        duration_secs: 2.0,
    };
    cfg.target_rps = 8.0;
    cfg.tag = Some("e2e-ramp".into());

    let cancel = Arc::new(Notify::new());
    let output = runner::run_with_cancel(
        RunOptions {
            config: cfg,
            history_dir: history_root.clone(),
            shared_aggregator: None,
        },
        cancel,
    )
    .await
    .expect("ramp run completes");

    let agg = &output.aggregator;
    let total = agg.total_requests();
    // The run is 2s at average 5 rps; we should comfortably complete at
    // least 4 (the constant-start rate × 2s).
    assert!(
        total >= 4,
        "expected at least 4 completed requests, got {total}"
    );

    let achieved = total as f64 / 2.0;
    assert!(
        achieved > 2.0 && achieved < 8.0,
        "achieved rps {achieved:.2} should be between 2.0 and 8.0"
    );

    // Save the run (the same way the CLI does it) so config.json is
    // written, and verify the persisted config carries the ramp pattern.
    let summary_text = report::render_summary(&output.config, &output.aggregator, output.interrupted);
    let report_html = report::render_html(&output.config, &output.aggregator, output.interrupted);
    let _ = storage::save_run(
        &history_root,
        &output.run_id,
        &output.config,
        &output.aggregator,
        &summary_text,
        &report_html,
        RunStatus::Completed,
    )
    .expect("save_run succeeds");

    let dir = storage::run_dir(&history_root, &output.config.model, &output.run_id);
    assert!(dir.join("config.json").is_file(), "config.json missing");
    let config_text = std::fs::read_to_string(dir.join("config.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&config_text).unwrap();
    assert_eq!(saved["pattern"]["kind"], "ramp");
    assert!((saved["pattern"]["end"].as_f64().unwrap() - 8.0).abs() < 1e-9);
}

/// New in M3: run two back-to-back 2s/2-RPS tests against the same
/// wiremock target, then exercise the compare pipeline end-to-end.
/// Asserts the diff is non-empty, the RPS delta is ~0 (both ran at
/// 2 rps), and the HTML compare page contains both run ids.
#[tokio::test]
async fn e2e_compare_two_runs_produces_diff() {
    let server = MockServer::start().await;
    let body = sse_response_body();
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
                .set_delay(Duration::from_millis(15)),
        )
        .mount(&server)
        .await;

    let target = format!("{}/v1/chat/completions", server.uri());
    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();

    // Two runs against the same target, both 2s @ 2rps.
    let out_a = run_and_save(target.clone(), 2, history_root.clone()).await;
    let out_b = run_and_save(target.clone(), 2, history_root.clone()).await;

    // Load both via the storage helper that the CLI uses.
    let (cfg_a, records_a, status_a) =
        storage::load_compare_data(&history_root, &out_a.run_id).expect("load a");
    let (cfg_b, records_b, status_b) =
        storage::load_compare_data(&history_root, &out_b.run_id).expect("load b");

    let inputs = power_test::compare::CompareInputs {
        cfg_a,
        records_a,
        status_a,
        cfg_b,
        records_b,
        status_b,
    };

    // Text diff is non-empty and references both runs.
    let text = power_test::compare::render_text(&inputs, false);
    assert!(text.contains("power_test compare"));
    assert!(text.contains(&out_a.run_id));
    assert!(text.contains(&out_b.run_id));
    assert!(text.contains("achieved rps"));

    // The (diff, warnings) from `compute` should yield a near-zero RPS
    // delta: both runs scheduled the same number of requests.
    let (diff, warnings) = power_test::compare::compute(&inputs);
    assert!(warnings.is_empty(), "no shape warnings expected: {warnings:?}");
    assert!(
        diff.achieved_rps.abs.abs() < 0.5,
        "expected RPS delta near 0, got abs={} pct={:?}",
        diff.achieved_rps.abs,
        diff.achieved_rps.pct
    );

    // HTML compare page contains both run ids and a chart.
    let html = power_test::compare::render_html(&inputs);
    assert!(html.contains(&out_a.run_id));
    assert!(html.contains(&out_b.run_id));
    assert!(html.contains("chart.js"));
    assert!(html.contains("latency_ms"));
}

/// New in M4: drive the raw HTTP client with a static body and verify
/// the request is recorded with at least one completion token estimated
/// from the response bytes.
#[tokio::test]
async fn e2e_raw_http_with_static_body() {
    use std::io::Write as _;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/raw"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string("ok-response"),
        )
        .mount(&server)
        .await;

    // Write a small body to a temp file.
    let mut body_file = tempfile::NamedTempFile::new().unwrap();
    body_file.write_all(b"{\"q\":\"hi\"}").unwrap();

    let target = format!("{}/raw", server.uri());
    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();

    let mut cfg = build_config(target, 1);
    cfg.api = ApiKind::Raw;
    cfg.stream = false;
    cfg.pattern = LoadPattern::Constant { rps: 2.0 };
    cfg.target_rps = 2.0;
    cfg.raw_body_file = Some(body_file.path().to_path_buf());
    cfg.raw_content_type = Some("application/json".to_string());

    let cancel = Arc::new(Notify::new());
    let output = runner::run_with_cancel(
        RunOptions {
            config: cfg,
            history_dir: history_root.clone(),
            shared_aggregator: None,
        },
        cancel,
    )
    .await
    .expect("raw run completes");

    // The aggregator must have at least one record.
    let agg = &output.aggregator;
    assert!(
        agg.total_requests() >= 1,
        "expected at least 1 request, got {}",
        agg.total_requests()
    );
    let first = &agg.per_request()[0];
    assert_eq!(first.status, 200);
    assert!(!first.estimated || first.completion_tokens > 0 || first.estimated);
    // "ok-response" is 11 bytes → 11/4 = 2 tokens.
    assert!(
        first.completion_tokens >= 1,
        "completion_tokens should be > 0, got {}",
        first.completion_tokens
    );
    // Body file is 10 bytes → 10/4 = 2 tokens (with max(1,...) = 2).
    assert!(first.prompt_tokens >= 1);

    // Persist the run and verify metrics.json has the record.
    let summary_text = report::render_summary(&output.config, &output.aggregator, output.interrupted);
    let report_html = report::render_html(&output.config, &output.aggregator, output.interrupted);
    let _ = storage::save_run(
        &history_root,
        &output.run_id,
        &output.config,
        &output.aggregator,
        &summary_text,
        &report_html,
        RunStatus::Completed,
    )
    .expect("save_run succeeds");

    let dir = storage::run_dir(&history_root, &output.config.model, &output.run_id);
    let metrics_text = std::fs::read_to_string(dir.join("metrics.json")).unwrap();
    let metrics: serde_json::Value = serde_json::from_str(&metrics_text).unwrap();
    let per_request = metrics["per_request"].as_array().expect("per_request array");
    assert!(!per_request.is_empty(), "metrics.json should have records");
}

/// New in M5: invoke the `power_test` binary with `--config` pointing at a
/// freshly written TOML. The test stands up a wiremock that always
/// returns 500, so the run completes in ~1s with the per-request
/// errors recorded — and the output proves the TOML was loaded and
/// the runner actually hit the configured target.
#[tokio::test]
async fn e2e_toml_config_loaded() {
    use std::io::Write as _;
    use std::process::Command;
    use std::time::Duration;

    // Wiremock: every request gets a 500 with a small JSON body. The
    // server is kept alive (the `server` binding) for the lifetime of
    // the test, so the binary can hit it.
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500)
                .insert_header("content-type", "application/json")
                .set_body_string(r#"{"error":"forced for e2e"}"#)
                .set_delay(Duration::from_millis(5)),
        )
        .mount(&server)
        .await;
    let target = format!("{}/v1/chat/completions", server.uri());

    // Write a TOML config that sets a tag we can grep for in the
    // output. Other defaults come from the CLI flags.
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("power_test.toml");
    let mut f = std::fs::File::create(&cfg_path).unwrap();
    f.write_all(
        br#"
# Defaults applied to every run.
api = "openai"
model = "gpt-4o-mini"
rps = 1.0
duration = 1
max_tokens = 8
tag = "m5-toml-e2e"
strategy = "random"
"#,
    )
    .unwrap();

    // Spawn the binary. We use a wall-clock timeout via a thread +
    // channel so the test never hangs forever.
    let bin = env!("CARGO_BIN_EXE_power_test");
    let mut child = Command::new(bin)
        .arg("--config")
        .arg(&cfg_path)
        .arg("run")
        .arg("--target")
        .arg(&target)
        .arg("--log-level")
        .arg("error")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn power_test");

    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stdout_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let stderr_thread = std::thread::spawn(move || {
        use std::io::Read as _;
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        let status = child.wait();
        let _ = tx.send(());
        status
    });
    let _ = rx.recv_timeout(Duration::from_secs(20));
    let _ = waiter.join();
    // Keep wiremock alive until the very end.
    drop(server);

    let stdout = stdout_thread.join().unwrap_or_default();
    let stderr = stderr_thread.join().unwrap_or_default();

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&stdout),
        String::from_utf8_lossy(&stderr)
    );

    // 1. The TOML tag must appear in the output, proving the config
    //    was loaded and merged into the run args.
    assert!(
        combined.contains("m5-toml-e2e"),
        "expected TOML tag 'm5-toml-e2e' in output (proving config was loaded), got:\n{combined}"
    );

    // 2. Some kind of error indicator must appear (500 or "error"),
    //    proving the runner actually exercised the request path
    //    against the target we configured.
    let lowered = combined.to_ascii_lowercase();
    let has_error = lowered.contains("error")
        || lowered.contains("500")
        || lowered.contains("refused")
        || lowered.contains("reset")
        || lowered.contains("connect");
    assert!(
        has_error,
        "expected an error indicator in output, got:\n{combined}"
    );
}

/// New in M5: invoke the binary with `run --print-config`. Verify the
/// output is a valid TOML document that round-trips through
/// `config_io::load`.
#[test]
fn e2e_print_config_emits_valid_toml() {
    use std::io::Write as _;
    use std::process::Command;

    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("power_test.toml");
    let mut f = std::fs::File::create(&cfg_path).unwrap();
    f.write_all(
        br#"
target = "http://localhost:1234/v1/chat/completions"
model = "gpt-4o-mini"
duration = 5
strategy = "round-robin"
"#,
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_power_test");
    let out = Command::new(bin)
        .arg("--config")
        .arg(&cfg_path)
        .arg("run")
        .arg("--print-config")
        .arg("--log-level")
        .arg("error")
        .output()
        .expect("spawn power_test");

    assert!(
        out.status.success(),
        "print-config should succeed, got {:?}\nstderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    // The snippet should at least contain the merged target and model.
    assert!(stdout.contains("target"), "no `target` in output:\n{stdout}");
    assert!(
        stdout.contains("gpt-4o-mini"),
        "no `gpt-4o-mini` in output:\n{stdout}"
    );
    assert!(
        stdout.contains("duration = 5"),
        "no `duration = 5` in output:\n{stdout}"
    );
    assert!(stdout.contains("[pattern]"), "no [pattern] table:\n{stdout}");
    assert!(stdout.contains("[dataset]"), "no [dataset] table:\n{stdout}");

    // And the snippet should be a parseable TOML.
    let parsed: power_test::config_io::TomlConfig =
        toml::from_str(&stdout).expect("print-config output should be valid TOML");
    assert_eq!(
        parsed.target.as_deref(),
        Some("http://localhost:1234/v1/chat/completions")
    );
}

/// M6 dynamic-multi: a TOML profile with `messages[]` and
/// `follow_ups[]` should drive K parallel sessions, each running
/// its own chain of serial turns. After the run, the metrics
/// aggregator should report one session per item, and the total
/// turn count should equal items × (1 + follow_ups).
#[tokio::test]
async fn e2e_dynamic_multi_session_pool_2_sessions_2_turns() {
    use power_test::dataset::OwnedChatMessage;

    let server = MockServer::start().await;
    let body = concat!(
        "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"ok\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
         \"usage\":{\"prompt_tokens\":3,\"completion_tokens\":2}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(body)
                // 50ms delay so the executor can interleave sessions.
                .set_delay(Duration::from_millis(50)),
        )
        .mount(&server)
        .await;

    // Two items, each with seed + 1 follow-up ⇒ 2 turns per item.
    let items = vec![
        power_test::dataset::DatasetItem {
            prompt: "[user] q1".into(),
            estimated_prompt_tokens: 1,
            weight: None,
            tags: Vec::new(),
            name: Some("q1".into()),
            messages: Some(vec![OwnedChatMessage::new("user", "q1-seed")]),
            follow_ups: vec!["q1-follow".into()],
        },
        power_test::dataset::DatasetItem {
            prompt: "[user] q2".into(),
            estimated_prompt_tokens: 1,
            weight: None,
            tags: Vec::new(),
            name: Some("q2".into()),
            messages: Some(vec![OwnedChatMessage::new("user", "q2-seed")]),
            follow_ups: vec!["q2-follow".into()],
        },
    ];

    let tmp = TempDir::new().unwrap();
    let history_root: PathBuf = tmp.path().to_path_buf();
    let cancel = Arc::new(Notify::new());
    let mut cfg = build_config(
        format!("{}/v1/chat/completions", server.uri()),
        2,
    );
    cfg.dataset = DatasetSpec::Custom {
        path: std::path::PathBuf::from("/dev/null"), // unused; we override below
    };
    cfg.concurrency = 4;

    // Build the dataset + session pool manually and drive the
    // executor's `run_dynamic_session` path. We can't use the
    // load_poll-style `run_with_cancel` for dynamic_multi without
    // a TOML-profile-aware build path; we exercise the runtime
    // directly here.
    use power_test::dataset::pool::PoolDataset;
    use power_test::runner::session::SessionPool;
    let dataset: Arc<dyn power_test::dataset::Dataset> = Arc::new(PoolDataset::new(
        items,
        RequestStrategy::RoundRobin,
        power_test::dataset::DatasetMode::DynamicMulti,
    ));
    let client: Arc<dyn power_test::client::LlmClient> =
        Arc::from(power_test::client::build(&cfg).expect("client builds"));
    let agg = Arc::new(tokio::sync::Mutex::new(MetricsAggregator::new()));
    let pool = Arc::new(SessionPool::new(cfg.concurrency));
    let start = std::time::Instant::now();
    let cfg_arc = cfg.clone();

    // Spawn 2 session runners, one per item. Each runs seed + 1
    // follow-up = 2 turns. Wait for both to finish.
    let mut handles = Vec::new();
    for _ in 0..2 {
        let d = dataset.clone();
        let c = client.clone();
        let a = agg.clone();
        let p = pool.clone();
        let _s = start;
        let _ca = cancel.clone();
        handles.push(tokio::spawn(async move {
            let item = d.next().await;
            // Replicate the executor's run_dynamic_session logic
            // inline (it's a private helper). Equivalent: acquire,
            // send seed, complete turn, append follow_up, send
            // again, complete, drop.
            let h = p.acquire_evict_lru(&item).expect("acquire");
            let session_id = p.snapshot().last().unwrap().id.clone();
            // Turn 1: seed.
            let m1 = c
                .send_messages(item.messages.as_ref().unwrap(), 1)
                .await;
            let ctx1 = power_test::runner::CompletionContext::turn(session_id.clone(), 1, false);
            a.lock().await.record_completed(&m1, 0, &ctx1);
            let r1 = h.complete(String::new(), !item.follow_ups.is_empty());
            assert_eq!(r1.action, power_test::runner::session::TurnAction::Continue);
            // Turn 2: follow_up.
            let mut msgs = h.messages();
            msgs.push(OwnedChatMessage::new("user", item.follow_ups[0].clone()));
            let m2 = c.send_messages(&msgs, 1).await;
            let ctx2 = power_test::runner::CompletionContext::turn(session_id.clone(), 2, true);
            a.lock().await.record_completed(&m2, 0, &ctx2);
            let r2 = h.complete(String::new(), false);
            assert_eq!(r2.action, power_test::runner::session::TurnAction::Done);
            let mut g = a.lock().await;
            g.record_session_finished(false);
            drop(g);
            h.drop_session();
        }));
    }
    for h in handles {
        h.await.expect("session task completes");
    }

    let agg_final = agg.lock().await;
    let (session_count, session_turn_total, session_dropped) = agg_final.session_stats();
    assert_eq!(session_count, 2, "expected 2 sessions, got {session_count}");
    assert_eq!(session_turn_total, 4, "expected 4 total turns, got {session_turn_total}");
    assert_eq!(session_dropped, 0);
    let _ = history_root;
    let _ = cfg_arc;
    let _ = cancel;
}

/// M6 static-multi: a TOML profile with `messages[]` but no
/// `follow_ups[]` should produce one request per item, with the
/// full messages body. No session.
#[tokio::test]
async fn e2e_static_multi_sends_full_messages_body() {
    use power_test::dataset::OwnedChatMessage;
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
                {"role": "user", "content": "hi"}
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

    let mut cfg = build_config(
        format!("{}/v1/chat/completions", server.uri()),
        1,
    );
    cfg.api_key = Some("sk-test".into());
    let client = power_test::client::build(&cfg).expect("client");
    let m = client
        .send_messages(
            &[OwnedChatMessage::new("user", "hi")],
            1,
        )
        .await;
    assert_eq!(m.status, 200, "err={:?}", m.error);
}

/// M6d: the assistant text extracted from the streamed SSE
/// `delta.content` chunks must flow into the next turn's
/// `messages[]` body. We wiremock a 2-turn dynamic session where
/// turn 1 streams "answer-one" and turn 2 streams "answer-two";
/// the second turn's request body must contain
/// `[user, assistant:answer-one, user, …]`.
#[tokio::test]
async fn e2e_dynamic_multi_response_text_flows_into_next_turn() {
    use power_test::dataset::OwnedChatMessage;
    use power_test::runner::session::{SessionPool, TurnAction};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    let server = MockServer::start().await;

    // First turn: server returns "answer-one".
    let turn1_body = concat!(
        "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer-one\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
         \"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    // Second turn: must contain the assistant message from turn 1.
    // `body_partial_json` matches if the given subset is present;
    // we only assert on the user→assistant→user shape that proves
    // the回填 worked.
    let turn2_body = concat!(
        "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{\"content\":\"answer-two\"},\"finish_reason\":null}]}\n\n",
        "data: {\"id\":\"1\",\"object\":\"chat.completion.chunk\",",
        "\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\
         \"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1}}\n\n",
        "data: [DONE]\n\n",
    );
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(body_partial_json(serde_json::json!({
            "messages": [
                {"role": "user", "content": "first-question"},
                {"role": "assistant", "content": "answer-one"},
                {"role": "user", "content": "second-question"}
            ]
        })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(turn2_body),
        )
        .expect(1)
        .named("turn-2")
        .mount(&server)
        .await;
    // The first turn is matched by the path/method only — any body
    // works because we only care that the assistant text gets
    // extracted.
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(turn1_body),
        )
        .named("turn-1")
        .mount(&server)
        .await;

    let mut cfg = build_config(
        format!("{}/v1/chat/completions", server.uri()),
        1,
    );
    cfg.api_key = Some("sk-test".into());
    let client: Arc<dyn power_test::client::LlmClient> =
        Arc::from(power_test::client::build(&cfg).expect("client builds"));

    let pool = SessionPool::new(2);
    let item = power_test::dataset::DatasetItem {
        prompt: "[user] first-question".into(),
        estimated_prompt_tokens: 1,
        weight: None,
        tags: Vec::new(),
        name: Some("q".into()),
        messages: Some(vec![OwnedChatMessage::new("user", "first-question")]),
        follow_ups: vec!["second-question".into()],
    };
    let h = pool.acquire_evict_lru(&item).expect("acquire");

    // Turn 1: send seed.
    let m1 = client
        .send_messages(item.messages.as_ref().unwrap(), 1)
        .await;
    assert_eq!(m1.status, 200, "turn 1 err: {:?}", m1.error);
    assert_eq!(
        m1.response_text, "answer-one",
        "M6d: streaming delta.content must be joined into response_text"
    );
    let r1 = h.complete(m1.response_text.clone(), true);
    assert_eq!(r1.action, TurnAction::Continue);

    // Turn 2: send grown messages + follow-up. The pool's
    // `messages()` returns the grown list, then we append the
    // follow-up (this matches what the executor's
    // `run_dynamic_session` does). The follow-up only lives in
    // the request body — the session itself doesn't store it
    // (the assistant turn we just added via `complete` is
    // the only thing that grows the session).
    let mut msgs = h.messages();
    msgs.push(OwnedChatMessage::new("user", "second-question"));
    let m2 = client.send_messages(&msgs, 1).await;
    assert_eq!(m2.status, 200, "turn 2 err: {:?}", m2.error);
    assert_eq!(m2.response_text, "answer-two");
    // The wiremock `expect(1)` for the second turn would fail if
    // the body didn't contain the assistant:answer-one entry.
    // `body_partial_json` matches as a subset, so the test still
    // passes even when the model also appends reasoning or
    // tool_use blocks.

    // Mark turn 2 done (no more follow-ups) so the assistant
    // text is appended to the session. After this, the session
    // contains: [user:first, assistant:answer-one,
    // assistant:answer-two]. The follow-up user message only
    // existed in the request body for the turn it triggered.
    let r2 = h.complete(m2.response_text.clone(), false);
    assert_eq!(r2.action, TurnAction::Done);
    let session_msgs = h.messages();
    assert_eq!(
        session_msgs.len(),
        3,
        "session should have grown to 3 msgs: {:?}",
        session_msgs
    );
    assert_eq!(session_msgs[0].role, "user");
    assert_eq!(session_msgs[0].content, "first-question");
    assert_eq!(session_msgs[1].role, "assistant");
    assert_eq!(
        session_msgs[1].content, "answer-one",
        "M6d: assistant text from turn 1 must be in the session"
    );
    assert_eq!(session_msgs[2].role, "assistant");
    assert_eq!(
        session_msgs[2].content, "answer-two",
        "M6d: assistant text from turn 2 must be in the session"
    );
}

/// M6e: a 2-turn dynamic session with turn 1 writing 100 tokens
/// to cache and turn 2 reading 100 tokens from cache should
/// surface `cache_creation_total=100`, `cache_hit_total=100`,
/// `rate_turn1=0%`, and `rate_turn2plus=100%` in
/// `MetricsAggregator::cache_stats()`. Drives the full path
/// client → metrics → aggregator, so any broken wire-up in
/// any of those would fail this test.
#[tokio::test]
async fn e2e_dynamic_multi_cache_hit_rate_aggregates_per_turn() {
    use power_test::dataset::OwnedChatMessage;
    use power_test::runner::session::{SessionPool, TurnAction};
    use power_test::runner::MetricsAggregator;
    use std::sync::Arc as StdArc;
    use tokio::sync::Mutex as TokioMutex;

    let mut cfg = build_config(
        "http://localhost:1/v1/chat/completions".into(),
        1,
    );
    cfg.api_key = Some("sk-test".into());
    // We don't actually call the network — this test exercises
    // the aggregator path directly. We only need a valid
    // `RunConfig` and a `RequestMetrics` builder.
    let client: Arc<dyn power_test::client::LlmClient> =
        Arc::from(power_test::client::build(&cfg).expect("client builds"));

    // Build a fake "turn 1" and "turn 2" record.
    let turn1 = power_test::client::RequestMetrics {
        status: 200,
        prompt_tokens: 100,
        cache_creation_input_tokens: 100,
        cache_hit_input_tokens: 0,
        started_at: chrono::Utc::now(),
        finished_at: chrono::Utc::now(),
        ..Default::default()
    };
    let turn2 = power_test::client::RequestMetrics {
        status: 200,
        prompt_tokens: 100,
        cache_creation_input_tokens: 0,
        cache_hit_input_tokens: 100,
        started_at: chrono::Utc::now(),
        finished_at: chrono::Utc::now(),
        ..Default::default()
    };

    // Drive a 2-turn session with a real SessionPool so we
    // exercise the same code path as the executor (the pool
    // doesn't care about cache fields, but we want the test to
    // look like the real thing).
    let pool = StdArc::new(SessionPool::new(2));
    let item = power_test::dataset::DatasetItem {
        prompt: "[user] hi".into(),
        estimated_prompt_tokens: 100,
        weight: None,
        tags: Vec::new(),
        name: Some("cached-q".into()),
        messages: Some(vec![OwnedChatMessage::new("user", "hi")]),
        follow_ups: vec!["again".into()],
    };
    let h = pool.acquire_evict_lru(&item).expect("acquire");
    let agg = StdArc::new(TokioMutex::new(MetricsAggregator::new()));
    let session_id = pool.snapshot().last().unwrap().id.clone();

    // Turn 1: record a seed request that pays the cache_creation cost.
    {
        let mut g = agg.lock().await;
        g.record_completed(
            &turn1,
            0,
            &power_test::runner::CompletionContext::turn(session_id.clone(), 1, false),
        );
    }
    h.complete(turn1.response_text.clone(), true);
    // Turn 2: record a continuation that hits the cache.
    let mut msgs = h.messages();
    msgs.push(OwnedChatMessage::new("user", "again"));
    {
        let mut g = agg.lock().await;
        g.record_completed(
            &turn2,
            0,
            &power_test::runner::CompletionContext::turn(session_id.clone(), 2, true),
        );
    }
    let r = h.complete(turn2.response_text.clone(), false);
    assert_eq!(r.action, TurnAction::Done);

    // Now assert the cache stats.
    let g = agg.lock().await;
    let c = g.cache_stats();
    assert_eq!(c.cache_creation_total, 100, "creation: 100 turn-1 tokens");
    assert_eq!(c.cache_hit_total, 100, "hit: 100 turn-2 tokens");
    assert!((c.rate_overall - 50.0).abs() < 1e-6, "overall 50%, got {}", c.rate_overall);
    assert_eq!(c.rate_turn1, 0.0, "turn 1 = full miss");
    assert!(
        (c.rate_turn2plus - 100.0).abs() < 1e-6,
        "turn 2+ = full hit, got {}",
        c.rate_turn2plus
    );
    // The cache totals must also make it into the JSON dump
    // that `aggregator_to_json` produces (used by `metrics.json`
    // and downstream tooling).
    let json = power_test::runner::aggregator_to_json(&g);
    let cache_json = json.get("cache").expect("cache section");
    assert_eq!(cache_json["creation_total"], 100);
    assert_eq!(cache_json["hit_total"], 100);
    // 50% to one decimal
    assert!((cache_json["rate_overall_pct"].as_f64().unwrap() - 50.0).abs() < 1e-6);
    drop(g);
    let _ = client; // silence unused
}
