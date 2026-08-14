# power_test

HTTP-level stress testing for LLM inference endpoints. Point it at any
OpenAI-compatible, Anthropic, or raw HTTP endpoint, choose a load pattern
(constant / ramp / spike / soak), pick a prompt dataset, and run. You get
TTFT, ITL, TPS, latency percentiles, success rates, and token throughput,
saved as a self-contained HTML report and a JSON record under
`~/.power_test/history/`.

```
$ power_test run --target https://open-gateway.anspire.ai/v6/chat/completions \
                 --rps 5 --duration 30 --model deepseek-v4-flash

starting run 7a1f... -> https://open-gateway.anspire.ai/v6/chat/completions (rps=5, duration=30s, concurrency=256)
  ... 30s later ...

power_test summary
=================
run id:    7a1f-...
target:    https://open-gateway.anspire.ai/v6/chat/completions
model:     deepseek-v4-flash
started:   2026-08-05T17:21:13Z
duration:  30.0s
requests:  148 (147 success, 1 error)
rps:       4.93 achieved (5.00 target)

latency (ms)
------------
  p50:    421.50
  p90:    612.80
  p99:    788.20
  p99.9:  991.00

ttft (ms)
---------
  p50:    184.20
  p99:    401.50

itl (ms)
--------
  mean:   36.40
  p99:    78.10

tps (tokens/sec)
----------------
  mean:   26.40
  p99:    18.20

tokens
------
  prompt:     2950
  completion: 4128

artifacts
---------
  report:      ~/.power_test/history/7a1f.../report.html
  summary:     ~/.power_test/history/7a1f.../summary.txt
```

## Quickstart

```bash
# 1. Build
cargo build --release

# 2. Run against a public OpenAI-compatible endpoint
export OPENAI_API_KEY=sk-...
./target/release/power_test run \
    --target https://open-gateway.anspire.ai/v6/chat/completions \
    --rps 5 --duration 60 --model gpt-4o-mini

# 3. Inspect results
./target/release/power_test list
./target/release/power_test report <run-id>
```

For a local endpoint that doesn't require auth:

```bash
./target/release/power_test run \
    --target http://localhost:11434/v1/chat/completions \
    --rps 10 --duration 30 --model llama3
```

## What it measures

| Metric | What it captures |
|---|---|
| **Latency** | End-to-end request duration, with p50/p90/p99/p99.9 percentiles. |
| **TTFT** | Time-to-first-token (streaming only). The `X-Chat-Trace` of an LLM call. |
| **ITL** | Inter-token latency — gap between consecutive streamed tokens. Mean and p99. |
| **TPS** | Tokens-per-second during streaming. Mean and p99. |
| **RPS** | Achieved vs. target requests per second, with skipped-tick count for back-pressure. |
| **Tokens** | Prompt + completion counts, taken from the server's `usage` field when present. |
| **Status** | Per-request HTTP status codes, with human-readable errors on transport / parse / non-2xx. |

Streaming responses use HdrHistogram for accurate percentile math; non-streaming
falls back to a single sample per request.

## Patterns

| Pattern | When to use it | Example |
|---|---|---|
| `constant` (default) | Sustained load at a fixed RPS. | `--pattern constant --rps 10 --duration 60` |
| `ramp` | Step load up or down. Reveals how latency degrades with pressure. | `--pattern ramp --rps-start 2 --rps-end 20 --duration 60` |
| `spike` | Steady baseline with periodic bursts. Stress recovery testing. | `--pattern spike --rps 2 --spike-at 10 --spike-at 30 --spike-rps 50 --spike-duration 5` |
| `soak` | Long-running constant load with periodic checkpointing. | `--pattern soak --rps 5 --duration 3600 --soak-checkpoint 60` |

For `ramp`, the RPS rises (or falls) linearly from `--rps-start` to
`--rps-end` over `--duration`. For `spike`, the baseline is `--rps` and
spikes fire at every `--spike-at` (repeatable), each lasting
`--spike-duration` at `--spike-rps`.

## Datasets

Each request needs a prompt. Pick one:

| Dataset | When to use it | Example |
|---|---|---|
| `literal` (default) | One prompt, repeated. Quick smoke tests. | `--prompt "Hello"` |
| `token-budget` | One prompt, scaled to ~N tokens. | `--prompt-tokens 800` |
| `built-in` | A 12-prompt pool of mixed-length English + Chinese. | `--dataset built-in` |
| `sharegpt` | Real conversation dataset (ShareGPT JSON). | `--dataset sharegpt --sharegpt-path data/sharegpt.json` |
| `custom` | Your own JSON or JSONL. | `--dataset custom --custom-path data/prompts.jsonl` |

Custom JSON format (`prompts.json`):
```json
[
  {"prompt": "Hello", "estimated_prompt_tokens": 1},
  {"prompt": "What is the capital of France?", "estimated_prompt_tokens": 7}
]
```

Custom JSONL format (one prompt per line, `#` comments OK, blank lines
skipped):
```jsonl
{"prompt": "Hello", "estimated_prompt_tokens": 1}
{"prompt": "What is the capital of France?", "estimated_prompt_tokens": 7}
# {"prompt": "ignored", "estimated_prompt_tokens": 9999}
```

For multi-prompt datasets, `--request-strategy random` (default) cycles
through them with an xorshift64 PRNG; `round-robin` walks the list in
order.

## Compare

Diff two historical runs side-by-side. The diff highlights
improvements (green) and regressions (red) for every metric — RPS,
latency percentiles, TTFT, ITL, TPS, success rate, token counts.

```bash
$ power_test compare <run-a> <run-b>
# (auto-detects TTY and emits ANSI color when stdout is a terminal)

# or write a self-contained HTML page
$ power_test compare <run-a> <run-b> --html
# → ~/.power_test/history/compare-<a>-vs-<b>-<ts>.html
```

The HTML page is suitable for sharing in PRs or commit messages.

## TUI

For long runs, attach a live terminal UI:

```bash
$ power_test run --target ... --rps 10 --duration 600 --tui
```

The TUI shows run id, target, model, target vs. current RPS, time
remaining, and the four metric panels (latency / TTFT / ITL / TPS).
Press `q` to cancel, `p` to pause, `c` to cancel. The summary is
printed on exit as usual.

## Config

Save your run setup in `power_test.toml` (the CLI looks in `./` first,
then `~/.power_test/`). **CLI flags always win over TOML fields.**

```toml
# ~/.power_test/config.toml

target = "https://open-gateway.anspire.ai/v6/chat/completions"
api = "openai"
model = "deepseek-v4-flash"
rps = 10
duration = 60
concurrency = 256
max_tokens = 256
stream = true
tag = "baseline"
api_key_env = "OPENAI_API_KEY"

[pattern]
kind = "constant"
# for ramp: start = 2.0; end = 8.0
# for spike: baseline = 1.0; spikes = [{ at_secs = 5.0, rps = 50.0, duration_secs = 2.0 }]

[dataset]
kind = "built-in"
# for literal: text = "Hello"
# for token-budget: target_tokens = 200
# for sharegpt / custom: path = "data/sharegpt.json"

strategy = "random"

# raw HTTP only:
# raw_body_file = "body.json"
# raw_content_type = "application/json"
```

```bash
# Use the default lookup (./power_test.toml or ~/.power_test/config.toml)
$ power_test run --rps 5

# Point at an explicit file
$ power_test --config /path/to/run.toml run --duration 30

# Dump the effective merged config (TOML + CLI overrides) and exit
$ power_test --config run.toml run --print-config
```

## API support

| `--api` | Endpoints | Streaming | Notes |
|---|---|---|---|
| `openai` (default) | OpenAI, OpenAI-compatible (vLLM, llama.cpp, ollama, etc.) | yes | `Authorization: Bearer`, `usage` in final chunk when `stream_options.include_usage` is set. |
| `anthropic` | Anthropic Messages API and proxies | yes | `x-api-key`, `anthropic-version: 2023-06-01`. |
| `raw` | Any HTTP endpoint | no | `--raw-body-file` and `--raw-content-type`; tokens are estimated at 4 bytes each. |

`--api-key <key>` (or the `OPENAI_API_KEY` env var) sends the bearer
token. Anthropic sends the same value as `x-api-key` and falls back to
`Authorization: Bearer` for OpenAI-compatible proxies. Raw HTTP sends
`Authorization: Bearer` when a key is set.

## Limitations

M5 deliberately omits:

- **Prometheus / OpenTelemetry export** — metrics are JSON + HTML only.
- **Bearer-only auth** — no AWS SigV4, no client certs.
- **Response caching** — every request hits the server.
- **Multi-host load balancing** — single endpoint per run.
- **Configurable percentiles** — the report uses the standard
  p50/p90/p99/p99.9 set.
- **Out-of-process cancellation API** — `q` in the TUI or `Ctrl-C` only.

## Building from source

```bash
git clone <repo>
cd power_test
cargo build              # debug
cargo build --release    # optimized
cargo test               # all tests (138 unit + 8 e2e)
```

Stable Rust toolchain. No new external dependencies beyond M5's
`toml = "0.8"` and M4's `ratatui = "0.26"` + `crossterm = "0.27"`.

## License

MIT — see [LICENSE](LICENSE) (or the `license = "MIT"` line in
`Cargo.toml`).
