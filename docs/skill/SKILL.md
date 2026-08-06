---
name: power-test
description: |
  Run HTTP-level stress tests against LLM inference endpoints with the
  `power_test` CLI (built at D:\MyCodes\Rust\power_test) and produce
  self-contained HTML reports. Use this skill when the user asks to
  "压测 LLM"、"压一下 QPS"、"stress test"、"benchmark inference"、"对比两
  个模型延迟"、"测一下 TTFT/ITL/TPS"、or mentions load testing a
  chat-completions / messages endpoint. Supports OpenAI, Anthropic, and
  raw HTTP, with patterns: constant / ramp / spike / soak. Do NOT use
  for: Locust/k6/wrk-style non-LLM load tests, GPU-side benchmarks
  (vLLM bench, sglang bench), unit/integration test authoring, or
  comparing framework features without sending real traffic.
---

# power-test (LLM HTTP stress testing)

## Inputs to collect

Before running, lock down these — they go on the command line or in
`~/.power_test/config.toml`. Ask the user only if the answer would
materially change the test (otherwise pick the obvious default).

- **`--target`**: full URL of the chat-completions / messages endpoint
  (e.g. `https://api.openai.com/v1/chat/completions`).
- **`--api`**: `openai` (default), `anthropic`, or `raw` (any HTTP).
- **`--model`**: e.g. `gpt-4o-mini`, `claude-3-5-sonnet-20240620`.
- **`--model-alias`** (M6g, optional): override the *history
  grouping key* for this run. Use when the real model name
  carries a date suffix (`DeepSeek-V4-Flash-20260115`,
  `claude-3-haiku-20240229`) but you want every snapshot of the
  same underlying model to land in the same
  `<history>/<alias>/` subdirectory and show up in each
  other's compare-with dropdown. When omitted, the model name
  is used as the group key.
- **`--api-key`** (or `OPENAI_API_KEY` env var): bearer / `x-api-key`.
  Local endpoints (ollama, vllm with no auth) may skip this.
- **`--rps`** + **`--duration`**: how hard and how long.
- **`--pattern`**: `constant` (default) / `ramp` / `spike` / `soak`.
- **`--dataset`**: `literal` / `token-budget` / `built-in` /
  `sharegpt` / `custom`. Default: `literal` with a built-in
  prompt. For M6 multi-turn conversations, use
  `--dataset custom --custom-path <file.toml>` with the TOML
  profile format (see `references/patterns-and-datasets.md`).

If the user already has a TOML config, prefer `--config <path>` over
repeating every flag (CLI flags still win over TOML).

## Templates

The repo ships copy-paste-ready templates in
`D:\MyCodes\Rust\power_test\docs\examples\`:

- `power_test.toml` — full config template (every field, every
  pattern kind, every dataset kind, commented).
- `datasets/multi-turn-conversation.toml` — M6 dynamic_multi
  (3 example conversations with follow_ups).
- `datasets/static-multi-conversation.toml` — M6 static_multi
  (3 example multi-message requests, no follow_ups).
- `datasets/single-turn-prompts.json` / `.jsonl` — custom
  dataset format.
- `datasets/sharegpt-mini.json` — ShareGPT format.

When the user wants to set up a recurring test (e.g. nightly
benchmark, regression suite), point them at the templates
first — copying `power_test.toml` to `./power_test.toml` and
editing is faster than `--help`.

## Procedure

1. **Verify the binary is built.** Run `cargo build --release` from
   `D:\MyCodes\Rust\power_test` (M4 spec: do not add deps beyond
   `toml = "0.8"`, `ratatui = "0.26"`, `crossterm = "0.27"`). If the
   build fails, surface the error verbatim — do not invent a
   workaround. Reason: a broken binary would silently produce
   garbage metrics.

2. **Pick `--api` to match the endpoint.** OpenAI-compatible servers
   (vLLM, llama.cpp, ollama, TGI, any `/v1/chat/completions`)
   → `--api openai`. Anthropic Messages API → `--api anthropic`.
   Anything else → `--api raw` with `--raw-body-file` and
   `--raw-content-type`. Wrong `--api` makes the client send the
   wrong headers and the server returns 4xx; you'll see it in
   `status codes` and `errors` in the summary.

3. **Pick `--pattern` to match the question.** Constant is the
   default — use it for "what's the steady-state RPS?". Ramp answers
   "how does latency degrade with pressure?". Spike answers
   "what happens when traffic bursts?". Soak answers "does the
   server fall over after an hour?" and writes `metrics.json`
   checkpoints so an interrupted run is partially analyzable.

4. **Pick `--dataset` to match the prompt shape.** Single prompt
   smoke test → `--prompt "..."` (literal). Variable-length
   realism → `--dataset built-in` (12 mixed English + Chinese
   prompts). Real conversation data → `--dataset sharegpt --sharegpt-path <file>`.
   Your own JSON / JSONL → `--dataset custom --custom-path <file>`.
   For multi-prompt datasets, `--request-strategy random` (default)
   or `round-robin` to control cycle order.

5. **Run the test.** Always use `--log-level info` (default) so the
   operator sees transport errors. Add `--tui` for long runs
   (≥ 60s) when a live progress display is wanted — press `q` to
   cancel cleanly. Always pin `--duration` explicitly; the 60s
   default is a trap. Three more knobs worth getting right:

   - **`--tag '<model>-<rps>rps-<duration>s'`** — always pass
     one. The tag is searchable via `power_test list` and is
     the easiest way to find the run later for `compare`.
   - **`--stream`**: keep the default (`true`) when TTFT/ITL/TPS
     are the point. Flip to `--stream false` when the run is
     short (≤ 30s) and only end-to-end latency matters — the
     non-streaming mode has a cleaner end-of-request timestamp
     and won't have ITL jitter from a small sample.
   - **`--output-dir`**: leave at the default
     (`~/.power_test/history/`) for interactive use. Pin it
     to a known path when running from a script, CI, or
     eval harness, so the `report.html` location is
     deterministic.
   - **Latency benchmark vs end-to-end prompt choice.** If the
     user is asking for a latency / p99 / TTFT number, pair
     the run with `--prompt "Reply with the single word: ok"
     --max-tokens 16`. That collapses TTFT ≈ total latency
     and keeps model-output variance out of the numbers. The
     built-in literal prompt ("Explain quantum entanglement…",
     256 max_tokens) is fine for a smoke test, but confounds
     the latency benchmark because a 200-token answer takes
     far longer than a 1-token one. When in doubt, ask the
     user — but if they say "just give me a number", pick the
     short-prompt path.

   ```bash
   & D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
       --target <URL> `
       --api <KIND> `
       --model <NAME> `
       --rps <N> --duration <S> `
       --dataset <KIND> `
       --tag '<model>-<N>rps-<S>s' `
       --log-level info
   ```

6. **Generate the report.** The run already wrote
   `~/.power_test/history/<run-id>/report.html` and `summary.txt`.
   Re-render (after editing the source) with:

   ```bash
   & D:\MyCodes\Rust\power_test\target\release\power_test.exe report <run-id>
   ```

   For a side-by-side diff between two runs:

   ```bash
   & D:\MyCodes\Rust\power_test\target\release\power_test.exe compare <run-a> <run-b> --html
   ```

   Output lands at
   `~/.power_test/history/compare-<a>-vs-<b>-<ts>.html`.

7. **Interpret the result.** Read `summary.txt` first — it is plain
   text. The four panels (latency / TTFT / ITL / TPS) are the
   headline numbers. For the chart-heavy view, open
   `report.html` in a browser. For a model-to-model diff, use
   `compare --html`. See `references/report-interpretation.md` for
   the "what each metric means and what's bad" cheat sheet.

8. **Hand back to the user.** Return exactly four things:

   1. The run-id's last 4 hex chars (UUIDs are too long to read).
   2. The absolute path to `report.html` (the user opens this).
   3. The `p50` and `p99` lines copied verbatim from
      `summary.txt` — do not paraphrase, do not "round to
      nearest 10 ms", do not pull a single number out of a chart.
   4. **What kind of test it was** — short-prompt (latency
      benchmark) or full-prompt (end-to-end throughput). One
      line, no interpretation. The numbers mean different things
      and the user should know which they got.

   If they asked for a diff, also send the `compare-*.html`
   path and the same four things for run B. Never try to
   summarize the numbers yourself if the user can read the
   report — they care about p99, not your opinion.

## Output contract

- Per run: `~/.power_test/history/<run-id>/{config.json,
  metrics.json, report.html, summary.txt}`.
- Per compare: `~/.power_test/history/compare-<a>-vs-<b>-<ts>.html`
  (only when `--html` is passed).
- A run-id is a UUIDv4. The last 4 hex chars are enough to refer
  to it in chat.
- The binary **does not** print a success/failure exit code based
  on per-request errors — failed requests are recorded into
  `metrics.json` and the run still completes. This is intentional:
  the operator wants to see how the endpoint degrades, not have
  the tool bail at the first error.

## Failure handling

- **Build fails with `Permission denied` on a `.exe`**: another
  `power_test.exe` is still running. Find and kill it:
  `Get-Process -Name power_test | Stop-Process -Force`. Do not
  retry the build blindly.
- **Run hangs for > 5s after `--duration` should have elapsed**:
  the `Notify` cancel had a known race in M1–M2 builds; the
  M5 build has a `start.elapsed() >= duration` guard. If you
  inherit an older build, kill the process and rebuild.
- **All requests return `0: N` in `status codes` and `[N] transport: …` in `errors`**: the target refused or
  the request body is malformed for that `--api`. Verify
  `--target` is reachable (`curl -I <url>`) and that
  `--api` matches the endpoint family. Use
  `--log-level debug` to see the wire-level request.
- **`401 Unauthorized`**: missing or wrong `--api-key` /
  `OPENAI_API_KEY`. For OpenAI-compatible local servers you can
  drop the env var; for hosted ones you need a real key.
- **`TOML parse error` from `--config`**: run
  `cargo test --test e2e e2e_print_config_emits_valid_toml` to
  see the canonical schema. Or run `power_test run
  --print-config --log-level error` against a clean TOML and
  re-format.
- **`unknown --api 'X'` / `unknown --pattern 'X'`**: the
  validator only accepts `openai|anthropic|raw` and
  `constant|ramp|spike|soak`. Suggest the closest match to
  the user.

## Windows (win32) platform notes

The binary path on this host is `D:\MyCodes\Rust\power_test`. Use
PowerShell syntax. `cargo` and the binary are both first-class
Windows processes; no WSL / Git-Bash layer is needed.

- Always quote paths that contain spaces. `--config
  "C:\Users\myg13\My Config\power_test.toml"` works.
- Forward slashes in TOML paths also work on Windows, but
  backslashes inside TOML strings need to be escaped (`\\`).
- If you need to feed stdin / pipe a JSON file, use
  `Get-Content -Raw -Encoding UTF8 <file>` — never
  `cat`, which PowerShell 5.1 silently ANSI-decodes.
- For background runs that should survive a model turn, use the
  harness's `run_in_background: true` rather than
  `Start-Process -NoNewWindow` — the harness can then resume on
  completion.

## Examples

### 1. Smoke test a local ollama at 10 RPS for 30 s

```powershell
& D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --target http://localhost:11434/v1/chat/completions `
    --rps 10 --duration 30 --model llama3 `
    --prompt 'Reply with the single word: ok' --max-tokens 16
```

(Pin a short prompt + low `--max-tokens` so the numbers are a
**latency benchmark**, not "how fast does the model write a
200-word essay".)

### 2. Compare gpt-4o-mini vs gpt-4o at 5 RPS, 60 s

```powershell
$runA = & D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --target https://api.openai.com/v1/chat/completions `
    --rps 5 --duration 60 --model gpt-4o-mini `
    --tag "gpt-4o-mini-baseline"
$runB = & D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --target https://api.openai.com/v1/chat/completions `
    --rps 5 --duration 60 --model gpt-4o `
    --tag "gpt-4o-baseline"
# capture the two run ids from the printed "run id: <uuid>" lines
& D:\MyCodes\Rust\power_test\target\release\power_test.exe compare <runA> <runB> --html
```

### 3. Find the latency cliff with ramp 1 → 50 RPS over 5 minutes

```powershell
& D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --target https://api.openai.com/v1/chat/completions `
    --pattern ramp --rps-start 1 --rps-end 50 --duration 300 `
    --model gpt-4o-mini --tag "ramp-1-50"
```

### 4. Soak test for an hour with checkpoints every 60 s

```powershell
& D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --target https://api.openai.com/v1/chat/completions `
    --pattern soak --rps 5 --duration 3600 --soak-checkpoint 60 `
    --model gpt-4o-mini --tag "soak-1h"
```

### 5. Anthropic Messages API

```powershell
& D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --api anthropic `
    --target https://api.anthropic.com/v1/messages `
    --model claude-3-5-sonnet-20240620 `
    --rps 5 --duration 60
```

### 6. Stress an arbitrary HTTP endpoint with a static body

```powershell
# body.json is your static request body
& D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --api raw `
    --target http://localhost:9999/echo `
    --raw-body-file D:\tmp\body.json `
    --raw-content-type application/json `
    --rps 10 --duration 30
```

### 7. Multi-turn conversation (M6 dynamic_multi)

`docs/examples/datasets/multi-turn-conversation.toml` defines
3 example conversations with follow-ups. Each session runs
its own chain of serial turns; K parallel sessions equal
`--concurrency`.

```powershell
& D:\MyCodes\Rust\power_test\target\release\power_test.exe run `
    --target http://192.168.31.101:8317/v1/chat/completions `
    --api-key sk-myg133-1 `
    --model MiniMax-M3 `
    --dataset custom --custom-path D:\MyCodes\Rust\power_test\docs\examples\datasets\multi-turn-conversation.toml `
    --rps 5 --duration 60 --concurrency 4 `
    --tag "multiturn-5rps-60s"
```

The summary's `prompt cache` section is the headline metric
here: turn 1 should be a miss, turn 2+ should be ~100% hit if
the endpoint's KV cache is wired up correctly.

### 8. Group dated snapshots under one alias (M6g)

`--model-alias` lets dated model snapshots share a history
subdirectory and a compare-with group. Without it,
`DeepSeek-V4-Flash-20260115` and `DeepSeek-V4-Flash-20260201`
end up in separate folders and never see each other in
`compare`.

```powershell
# Both runs land in <history>/DeepSeek-V4-Flash/ and
# show up in each other's compare-with dropdown.
& D:\...\power_test.exe run --model DeepSeek-V4-Flash-20260115 --model-alias DeepSeek-V4-Flash ...
& D:\...\power_test.exe run --model DeepSeek-V4-Flash-20260201 --model-alias DeepSeek-V4-Flash ...
```

## See also

- `references/patterns-and-datasets.md` — every pattern × every
  dataset combo with the exact flag set and a one-line
  rationale.
- `references/report-interpretation.md` — what TTFT / ITL / TPS
  / latency percentiles mean, what's "good", and the common
  failure shapes you should report up.
