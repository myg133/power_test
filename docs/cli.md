# CLI reference

`power_test` is split into four subcommands: `run`, `list`, `report`, and
`compare`. A handful of top-level options apply to every subcommand.

## Top-level options

| Flag | Description |
|---|---|
| `--config <PATH>` | Path to a TOML config file. Defaults to `./power_test.toml`, then `~/.power_test/config.toml`. CLI flags override TOML fields. |
| `--log-level <LEVEL>` | Tracing log level. `error` / `warn` / `info` (default) / `debug` / `trace`. Ignored when `--quiet` is also set. |
| `-h`, `--help` | Print help. |
| `-V`, `--version` | Print version. |

## `power_test run`

Execute one test run. The most common subcommand.

### Run basics

| Flag | Default | Description |
|---|---|---|
| `--target <URL>` | (required to start a run) | Target LLM HTTP endpoint. Optional only for `--print-config`. |
| `--rps <RPS>` | `10.0` | Target requests per second. Used by `constant` and `soak`; baseline for `spike`; ignored by `ramp`. |
| `--duration <SECS>` | `60` | How long to run, in seconds. |
| `--model <NAME>` | `gpt-3.5-turbo` | Model name sent in the request body. |
| `--max-tokens <N>` | `256` | Max tokens for the completion. |
| `--stream <BOOL>` | `true` | Enable streaming responses. `false` for non-streaming mode. |
| `--api <KIND>` | `openai` | API family. `openai` / `anthropic` / `raw`. |
| `--concurrency <N>` | auto | Concurrency cap. Defaults to `max(256, peak_rps*4)`, capped at 1024. |
| `--output-dir <DIR>` | `~/.power_test/history` | Where `<run-id>/` is created. |
| `--tag <TAG>` | — | Free-form tag for the run (logged in summary). |
| `--api-key <KEY>` | `OPENAI_API_KEY` | Bearer token. Falls back to env var. |

### Load pattern

| Flag | Default | Description |
|---|---|---|
| `--pattern <KIND>` | `constant` | `constant` / `ramp` / `spike` / `soak`. |
| `--rps-start <RPS>` | — | Required for `ramp`. Starting RPS. |
| `--rps-end <RPS>` | — | Required for `ramp`. Ending RPS. |
| `--spike-at <SECS>` | — | Required for `spike`. Repeatable. Time offset of each spike. |
| `--spike-rps <RPS>` | — | Required for `spike`. RPS during each spike. |
| `--spike-duration <SECS>` | — | Required for `spike`. Duration of each spike. |
| `--soak-checkpoint <SECS>` | `60` | `soak` only. Snapshot `metrics.json` every N seconds. `0` disables. |

### Dataset

| Flag | Default | Description |
|---|---|---|
| `--dataset <KIND>` | inferred | `literal` / `token-budget` / `built-in` / `sharegpt` / `custom`. |
| `--prompt <TEXT>` | built-in | Used when `dataset = literal`. |
| `--prompt-tokens <N>` | `200` | Used when `dataset = token-budget`. |
| `--sharegpt-path <FILE>` | — | Required for `sharegpt`. |
| `--custom-path <FILE>` | — | Required for `custom`. JSON or JSONL. |
| `--request-strategy <KIND>` | `random` | `random` (xorshift64) or `round-robin`. |

### Raw HTTP (M4)

| Flag | Description |
|---|---|
| `--raw-body-file <PATH>` | Body to POST, read once at construction. |
| `--raw-content-type <CT>` | Defaults to `application/json`. |

### TUI + polish (M4 + M5)

| Flag | Description |
|---|---|
| `--tui` | Show a live terminal UI. `q` to cancel, `p` to pause, `c` to cancel. |
| `--quiet` | Suppress info-level tracing. Errors and warnings still print. |
| `--print-config` | Dump the effective merged config as TOML and exit. Does not start a run. |

## `power_test list`

List past runs in the history directory. Prints one line per run with
`run-id`, timestamp, RPS, status, total request count, and the target
URL (truncated past 60 chars).

| Flag | Description |
|---|---|
| `--history-dir <DIR>` | History directory to scan. Defaults to the standard location. |

## `power_test report <RUN_ID>`

Re-render the HTML report and summary for a saved run. Useful when you
want to upgrade the report format without re-running the test.

| Flag | Description |
|---|---|
| `--history-dir <DIR>` | History directory to search. Defaults to the standard location. |

The re-rendered files land in the same `<run-id>/` directory as
`report.html` and `summary.txt`.

## `power_test compare <RUN_A> <RUN_B>`

Diff two historical runs side-by-side. The diff covers achieved RPS,
total requests, success rate, latency p50/p90/p99/p99.9, TTFT p50/p99,
ITL mean/p99, TPS mean/p99, total tokens, and wall-clock duration.

Improvements render in green, regressions in red, and unchanged values
in grey (when stdout is a TTY).

| Flag | Description |
|---|---|
| `--history-dir <DIR>` | History directory to search. Defaults to the standard location. |
| `--html` | Also write a self-contained HTML compare page. Filename: `compare-<a>-vs-<b>-<ts>.html` in the history dir. |

## Examples

```bash
# Constant 5 RPS for 60s, default OpenAI client.
power_test run --target https://api.openai.com/v1/chat/completions \
    --rps 5 --duration 60 --model gpt-4o-mini

# Ramp 2 → 20 RPS over 60s.
power_test run --target ... --pattern ramp --rps-start 2 --rps-end 20 \
    --duration 60

# Spike: 2 RPS baseline, two 5s spikes at 50 RPS at t=10 and t=30.
power_test run --target ... --pattern spike --rps 2 \
    --spike-at 10 --spike-at 30 --spike-rps 50 --spike-duration 5

# Soak for an hour with checkpoint every 60s.
power_test run --target ... --pattern soak --rps 5 --duration 3600 \
    --soak-checkpoint 60

# Built-in 12-prompt pool, random strategy.
power_test run --target ... --dataset built-in --request-strategy random

# Custom JSONL dataset.
power_test run --target ... --dataset custom --custom-path data/prompts.jsonl

# Anthropic API.
power_test run --api anthropic \
    --target https://api.anthropic.com/v1/messages \
    --model claude-3-5-sonnet-20240620 --max-tokens 1024

# Raw HTTP POST (no SSE).
power_test run --api raw \
    --target http://localhost:9999/echo \
    --raw-body-file body.json --raw-content-type application/json

# With a TOML config and a CLI override.
power_test --config ~/.power_test/config.toml run --rps 5

# Dump effective config to verify a merge.
power_test --config run.toml run --print-config

# Live TUI for a long run.
power_test run --target ... --rps 10 --duration 600 --tui

# Compare the two most recent runs.
power_test compare <run-a> <run-b> --html
```
