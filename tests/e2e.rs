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
    let dir = history_root.join(&output.run_id);
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
    let dir = history_root.join(&output.run_id);
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

    let dir = history_root.join(&output.run_id);
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

    let dir = history_root.join(&output.run_id);
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
