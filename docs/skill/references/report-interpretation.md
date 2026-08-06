# Reading the report

`power_test` writes a `summary.txt` (plain text) and a
`report.html` (self-contained, with Chart.js) for every run, plus
`compare-<a>-vs-<b>-<ts>.html` when `compare --html` is invoked.

This file tells you what each metric means, what's a normal value,
and what shape of failure you should call out to the user.

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
- **p90**: the unhappy-user threshold. Anything north of ~1 s
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

## Compare HTML (`compare-<a>-vs-<b>-<ts>.html`)

Use this for regression reports. It prints every metric three
times: A's value, B's value, the delta. Deltas are color-coded:

- Green: improvement (lower latency, higher TPS, higher RPS,
  lower error rate).
- Red: regression.
- Grey: unchanged (within a small epsilon).

Watch for:

- **Latency p99 went up but p50 didn't**: tail regression. Often
  a single bad shard or a new slow code path.
- **Achieved RPS dropped but target was the same**: back-pressure
  kicked in. Either the new build is slower, or the server
  capacity changed.
- **TTFT unchanged but ITL regressed**: prefill is fine,
  decode got slower. Often a kernel change.
- **Success rate dropped**: real bug, not just a performance
  drift. Investigate before merging.
