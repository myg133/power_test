# Reading the report (single run)

`power_test` writes a `summary.txt` (plain text) and a
`report.html` (self-contained, with Chart.js) for every run.
The M7 per-model dashboard at
`~/.power_test/history/<model>/index.html` aggregates
every run for a model in one page.

This file is the **single-run** interpretation guide. For
the **two-run diff** (the compare page), see
`power-test-compare/references/compare-interpretation.md`.
For the TPM / RPM SLA workflow on a new upstream, see
`power-test-onboard/references/onboarding.md`.

## The headline numbers

`summary.txt` always has these four panels, in this order:

```
latency (ms)       — end-to-end request duration
ttft (ms)          — time-to-first-token (streaming only)
itl (ms)           — inter-token latency (streaming only)
tps (tokens/sec)   — tokens per second during streaming (streaming only)
```

For non-streaming runs (`--stream false`) only `latency` is
populated; the other three read "(no data)".

### latency p50 / p90 / p99 / p99.9

- **p50 (median)**: the typical user experience.
- **p90**: the unhappy-user threshold. Anything north of ~ 1 s
  here is usually a complaint.
- **p99**: the SLA number most teams quote. Compare this
  week-over-week.
- **p99.9**: the long tail. On chat workloads expect p99.9 to be
  3-5× p99.

What looks bad:
- p99 > 5 × p50: bimodal latency, often cache-cold vs cache-warm
  mixing.
- p99.9 >> p99 by an order of magnitude: GC pause, network blip,
  or a single broken shard.
- p50 creeping up over a soak run: queue saturation; raise
  concurrency or back off RPS.

### TTFT p50 / p99

Time from sending the request to receiving the first streamed
token. For chat this is the "feels responsive" number.

**TTFT includes the model's thinking/reasoning stream.** When a
model emits a `delta.reasoning_content` (OpenAI Chat-Completions
style) or `delta.type == "thinking_delta"` (Anthropic Messages
style) before the visible text, the first thinking token is
recorded as the TTFT. The summary prints
`ttft-includes-thinking: true` at the top so the number is
self-describing. Reasoning deltas also count toward ITL.

- < 200 ms p50: feels instant.
- 200-500 ms p50: feels normal.
- 500 ms-1 s p50: feels slow but acceptable for non-realtime.
- > 1 s p50: the model is doing prefill in the critical path.
  Either the prompt is too long, or the GPU is oversubscribed.

TTFT p99 should be 2-3× p50. If p99 / p50 > 10×, the endpoint
has a tail-latency problem that no amount of streaming will hide.

### ITL mean / p99

Gap between consecutive streamed tokens. This is the
"tokens-keep-flowing" number.

- 20-50 ms mean: smooth reading pace.
- 50-100 ms mean: noticeable stutter on long answers.
- > 100 ms mean: each token visibly lands; the model is
  throughput-bound, not latency-bound.

ITL p99 >> mean by > 10× means the run hit a rate limit or
queue stall — almost always a sign the server is over its
sustained throughput.

### TPS mean / p99

Tokens per second during the streaming portion of the request.
This is a **throughput** metric, not a latency one — higher is
better, and it usually trades against TTFT.

- "Good" depends entirely on the model: a 7B model on H100
  might do 200 TPS; a 70B might do 30. Don't compare TPS across
  model sizes.
- TPS p99 < mean: expected. The p99 sample is from a short
  answer that finished fast.
- TPS p99 ≈ mean: stable throughput, no outliers.
- TPS p99 < 50% of mean: severe tail-degradation. Often
  coincides with a spike in latency p99.9.

## M6e: prompt cache section (multi-turn / long-prefix runs)

When the run sees any prompt-cache data, `summary.txt` gains a
`prompt cache` block:

```
prompt cache
------------
  hit rate:        50.00%   (100 / 200 prompt tokens)
    turn 1:         0.00%   (0 / 100 prompt tokens)
    turn 2+:      100.00%   (100 / 100 prompt tokens)
  cache creation: 100 tokens (turn 1: 100 · turn 2+: 0)
```

- **`hit rate`**: `100 * cache_hit_tokens / total_prompt_tokens`.
  Across the whole run. The headline number.
- **`turn 1`**: hit rate on the first turn of a session (or
  any single-turn request). Almost always 0% on the first
  turn of a long-prefix session because the model has to
  read or build the cache.
- **`turn 2+`**: hit rate on continuation turns. For a
  well-tuned inference backend with prompt cache enabled,
  this should be **~ 100%**. A drop here is the earliest
  signal that KV cache is being evicted or the prefix is
  being rebuilt unnecessarily.
- **`cache creation`**: tokens the model wrote to cache.
  Anthropic reports this as `usage.cache_creation_input_tokens`;
  OpenAI does not. On the first turn of a long-prefix run
  this number can be large (it's the one-time cost of caching
  the prefix); on later turns it should be 0.

The HTML report renders the same numbers as a "Prompt cache"
card with the global rate as a large headline and a per-turn
bar pair (overall / turn 1, plus a `Turn 2+` row when
continuation turns were observed). M7 made the section
**always render** — single-turn or no-cache-data runs read
`0.0% (no cache observed)` instead of hiding the card. This
keeps the report layout stable when comparing single-turn vs
multi-turn runs of the same model.

## M6: session stats (multi-turn runs only)

Multi-turn runs (M6 dynamic_multi, with a TOML profile that
has `follow_ups`) also report session bookkeeping. The
`metrics.json` and `summary.txt` carry:

- **`session_count`**: number of distinct sessions that
  completed at least one turn.
- **`session_turn_total`**: total number of turn-completions
  across the run — every `record_completed` call counts as 1
  turn. For a single-turn run this equals `total_requests`; for
  multi-turn it grows with continuation turns.
- **`session_dropped`**: number of sessions that bailed out
  mid-conversation because a turn returned non-2xx. Should be
  0 for a healthy run; nonzero indicates an endpoint
  problem that's not catastrophic enough to fail the whole
  run.

## M6f / M6g: model grouping in the history directory

The history layout is now `<root>/<group_key>/<run_id>/` where
`group_key` is the alias (if `--model-alias` is set) or the
model name. The `config.json` inside the run directory carries
both the real `model` string and the `model_alias` (when set),
so reports always show the actual model name. The grouping key
is just for filesystem hygiene and the compare-with dropdown.

`compare` warns when the two runs have different group keys
("different model alias: 'A' vs 'B' (runs are in different
history subdirectories)"). That's the signal that the diff
is comparing across different logical models, even when the
literal model string happens to match.

## M7: advanced metrics card (TPOT / throughput / turns)

The HTML report carries a second table directly under the
Summary statistics table. Heading: `高级指标` (zh) /
`Advanced metrics` (en). Each row is a single-value metric (no
percentile distribution):

- **TPOT (毫秒/token)**: time per output token during the
  streaming portion. Complements ITL — ITL is the gap between
  consecutive tokens, TPOT is total streaming time divided by
  output tokens. Lower is better. On a 7B model on a single
  H100 expect 10-30 ms; on a 70B expect 30-100 ms.
- **输出 token/秒 (Output tok/s)**: sum of completion tokens
  divided by total streaming time. The aggregate throughput
  view; not the same as the per-request TPS in the Summary
  table (which is a per-request mean).
- **总 token/秒 (Total tok/s)**: same denominator but counts
  prompt + completion tokens. Useful when comparing against
  the server's rated context throughput.
- **平均输入 token / 平均输出 token**: per-request means.
- **平均轮次/请求**: `session_turn_total / total_requests`.
  Reads as 1.0 for a single-turn run; 2.0+ for multi-turn.
- **每轮解码 token / 投机接受率**: speculative-decoding
  fields. Only present when the run reported
  `usage.completion_tokens_details.accepted_prediction_tokens`
  (OpenAI) or an equivalent; the rows are hidden when no
  speculative data was observed.

## M7: language toggle

Every HTML report (`report.html` and the per-model
`index.html`) is rendered in Chinese by default. The
top-right corner has a `中文 / English` button; clicking flips
every `[data-i18n]` element to the English dictionary. The
choice is persisted in `localStorage` under `power_test.lang`.
The summary text and CLI output stay in English.

## M7: per-model dashboard

`power_test dashboard [<NAME>]` renders a per-model page that
lists every run for a model (newest first), with a side-by-side
compare picker at the bottom. The run_id cell is a real
`<a class="run-link">` pointing at the run's `report.html` —
middle-click / right-click open in a new tab natively, and
clicking anywhere else in the row also opens the report via a
JS row click handler. Diff math (delta / color_class) runs in
the browser; the page works offline because chart.js is
inlined.

The dashboard is auto-regenerated on every `run` save
(best-effort, failure logs a warning and the real save still
succeeds). The manual `power_test dashboard <NAME>` is mostly
for back-fills, CI, and explicit refreshes after re-rendering
a report.

## TPM / RPM (for SLA conversations)

`summary.txt` already prints the per-second throughput
numbers. For the SLA the operator usually wants the
per-minute rate. The conversion:

```
RPM                      = achieved_rps              × 60
TPM (output tokens / min) = output_throughput_tps     × 60
TPM (total tokens / min)  = total_throughput_tps      × 60
```

For a one-line SLA report (paste-into-ticket), pull the
numbers straight from `metrics.json`:

```powershell
$j = Get-Content "$env:USERPROFILE\.power_test\history\<model>\<run>\metrics.json" | ConvertFrom-Json
"  RPM:                $($j.achieved_rps * 60)"
"  TPM (output):       $($j.output_throughput_tps * 60)"
"  TPM (total):        $($j.total_throughput_tps * 60)"
"  p50 latency (ms):   $($j.avg_latency_ms)"
```

Vendor SLA tables usually quote **TPM (output)** for
generation cost, or **RPM** for concurrency, or both.
Match the formula to the contract: "RPM" is universal,
"TPM" depends on whether the vendor counts input +
output or just output. The summary's "output" vs "total"
distinction lets you quote either number without re-running
the test.

When you're quoting TPM / RPM numbers in a vendor ticket
or a customer SLA, also include **achieved RPS** as a
percentage of the target, and the run's `concurrency` —
those tell the other side how hard you pushed. The
"pass / fail criteria" table in
`power-test-onboard/references/onboarding.md` has the
thresholds we use.

For the per-request TPM check (the customer's
"am I getting the TPM per request I paid for?"):

```
TPM (output per request) = TPM (output) / RPM
                          = avg_output_tokens
```

i.e. the per-request TPM is just the average output-token
count. If the SLA is "10 000 TPM per request", that's
"the average response should have 10 000 output tokens"
in disguise — the contract is really an "average output
length" promise.

## Other sections of `summary.txt`

### rps: 4.93 achieved (5.00 target)

Achieved RPS is what the runner actually completed divided by
elapsed time. Target is what you asked for. If achieved < target
by > 5%, the runner is skipping ticks because of concurrency or
the endpoint is too slow.

A skipped-tick counter is in `metrics.json` under
`scheduled - completed - in_flight_at_end`. The e2e test
`e2e_skips_ticks_when_target_is_slow` covers the high-RPS /
low-concurrency case.

### requests: 148 (147 success, 1 error)

Success count from HTTP 2xx. Anything else counts as an error
and shows up in the next block.

### status codes

Histogram of HTTP status codes seen. `0: 12` means 12 requests
never produced a response (transport / timeout). `401: 1` means
one request was auth-rejected — usually a key issue, not a
performance issue.

### errors

First 5 unique error messages. If the run is healthy, this
section is empty or near-empty. A flood of `transport: error
sending request for url (...)` means the endpoint was refusing
connections, dropping them, or running out of file descriptors.

### tokens

- `prompt`: sum of `usage.prompt_tokens` reported by the server
  (when present). For `openai` with `stream_options.include_usage`,
  this is accurate. Otherwise it may be 0.
- `completion`: same, for `usage.completion_tokens`. The
  `estimated: true` flag (in `metrics.json` per-request) means
  the count is from chunk-counting rather than `usage`.

## HTML report (`report.html`)

The HTML is a single self-contained file. Open it in any
browser. It includes:

- The same numbers as `summary.txt`, with sparkline-style
  charts.
- A time-series of latency, TTFT, ITL, TPS at second-resolution
  (visible in the per-second charts).
- Pattern info: which load pattern was used, its peak / average
  RPS.
- Dataset info: prompt distribution (min / max / mean / count).
- A "compare with" dropdown of the last 10 runs on the same
  target. Picking one in a browser navigates to the pre-rendered
  `compare-*.html` for the pair.

## Cross-references

- `power-test-compare` — for two-run side-by-side diff
  (`references/compare-interpretation.md` is the diff guide).
- `power-test-onboard` — for the 3-report SLA workflow on a
  new upstream (`references/onboarding.md` has the hand-off
  checklist).
