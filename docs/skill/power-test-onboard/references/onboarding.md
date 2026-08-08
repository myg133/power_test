# Onboarding a new upstream model · 双端压测 walkthrough

This is the long-form companion to the "Onboarding a new
upstream · 双端压测" section in `SKILL.md`. Read that first
for the inputs and the procedure; this file is the
**checklist + worked example + TPM / RPM math** that
operator / vendor-management / customer-success teams need
when handing the three reports up the chain.

## When to run this

Run this every time you bring a new upstream model online
in a token-reseller setup:

1. The team sits between the upstream model vendor and your
   downstream customers. Both sides pay for tokens, so
   TPM / RPM matter in both directions.
2. The vendor publishes a TPM / RPM. You want to know what
   you actually get before signing the contract.
3. You commit a TPM / RPM SLA to your customers. You want
   to know what your stack actually delivers before
   quoting the number.
4. The model is the same on both sides (you resell tokens
   for the same model the vendor sells). The point of the
   compare is your overhead, not a model-quality diff.

Skip this if the two sides serve different model families
— that's an M-series compare, not this workflow.

## The three reports

| # | Side | File | What it answers | Hand to |
|---|---|---|---|---|
| 1 | Upstream | `<root>/<model>/<run>/report.html` (+ `summary.txt`) | What does the vendor actually deliver? TPM / RPM real, p99 real. | Vendor management / procurement |
| 2 | Our | `<root>/<model>/<run>/report.html` (+ `summary.txt`) | What do we deliver to our customers? TPM / RPM real, p99 real. | Customer success / sales |
| 3 | Compare | `<root>/compare-<a>-vs-<b>-<ts>.html` | Our overhead vs upstream. Latency delta, throughput delta. | Engineering / product |

The compare-*.html file is the only one that spans the two
runs. It carries a `different target` warning by design —
that is the whole point of this workflow.

The per-model dashboard at `<root>/<model>/index.html`
(M7) lists both runs side by side with their tags, so the
three reports are also reachable in two clicks without
typing the run id.

## Tag suffix convention

```
<model>-rps<N>-dur<S>-upstream     # vendor-side run
<model>-rps<N>-dur<S>-our         # our-side run
```

The same `<model>` on both runs is non-negotiable — the
history is grouped by model name, and breaking that
breaks the dashboard and the compare-with dropdown. The
`-upstream` / `-our` suffix is what makes the role visible.

`-rps<N>` and `-dur<S>` are a small convention that makes
sibling runs easy to find. With it, the operator can
`power_test list` and immediately spot the pair.

Do not use `--model-alias` for the upstream/our split —
alias is for **dated snapshots of the same logical model**
(M6g), e.g. `DeepSeek-V4-Flash-20260115` vs
`DeepSeek-V4-Flash-20260201`; upstream/our is a different
axis. They can coexist (alias for snapshot, tag suffix
for role) but the role always lives in the tag, not the
alias.

## Worked example

Target: bring `qwen36-27B` online as a new upstream.
Shape: 1 RPS, 60 s, concurrency 2, multi-turn (3 turns × 3
conversations), stream, max-tokens 32, short prompt.

### 1. Upstream run

```powershell
$env:OPENAI_API_KEY = '<vendor-key>'
& D:\MyCodes\Rust\power_test\target\debug\power_test.exe run `
    --target 'https://vendor.api/v1/chat/completions' `
    --api openai `
    --model qwen36-27B `
    --rps 1 --duration 60 --concurrency 2 `
    --dataset custom `
    --custom-path D:\MyCodes\Rust\power_test\docs\examples\datasets\multi-turn-conversation.toml `
    --request-strategy round-robin `
    --max-tokens 32 --stream true `
    --tag 'qwen36-27b-rps1-dur60-upstream'
```

The terminal prints:

```
run id:      20260808-11-28-15-1db22a
history dir: C:\Users\myg13\.power_test\history\qwen36-27B\20260808-11-28-15-1db22a
report:      C:\Users\myg13\.power_test\history\qwen36-27B\20260808-11-28-15-1db22a/report.html
```

Capture `20260808-11-28-15-1db22a` — that's `RUN_UPSTREAM`.

### 2. Our run

Override the env var so we don't accidentally hit the
vendor with our key:

```powershell
$env:OPENAI_API_KEY = '<our-key>'
& D:\MyCodes\Rust\power_test\target\debug\power_test.exe run `
    --target 'https://our.api/v1/chat/completions' `
    --api openai `
    --model qwen36-27B `
    --rps 1 --duration 60 --concurrency 2 `
    --dataset custom `
    --custom-path D:\MyCodes\Rust\power_test\docs/examples/datasets/multi-turn-conversation.toml `
    --request-strategy round-robin `
    --max-tokens 32 --stream true `
    --tag 'qwen36-27b-rps1-dur60-our'
```

Capture `RUN_OUR`.

### 3. Compare

```powershell
& D:\MyCodes\Rust\power_test\target\debug\power_test.exe compare `
    $RUN_UPSTREAM $RUN_OUR --html
```

The compare-*.html lands at
`~/.power_test/history/compare-<RUN_UPSTREAM>-vs-<RUN_OUR>-<ts>.html`.

### 4. Open the dashboard

The dashboard at
`C:\Users\myg13\.power_test\history\qwen36-27B\index.html`
now lists both runs (M7). The run_id cell is clickable and
opens the run's `report.html`; the row is also clickable
for the same effect. The diff view at the bottom of the
dashboard is a JS-side compare with the same diff math
as the compare-*.html file — pick run A and B and click
"对比".

## TPM / RPM math

The summary.txt already prints the per-second throughput
numbers. The minute-scale TPM / RPM are simple multiplies:

| Metric | Formula | Source field |
|---|---|---|
| RPM (requests per minute) | `achieved_rps × 60` | `summary.txt` "results" block |
| TPM (output tokens / minute) | `output_throughput_tps × 60` | "throughput (system-wide)" block |
| TPM (total tokens / minute) | `total_throughput_tps × 60` | same |
| TPM (output per request) | `TPM (output) / RPM` ≈ `avg_output_tokens × RPS × 60 / RPS / 60` = `avg_output_tokens` | derived |

For the worked example above (qwen36-27B, multi-turn):

```
achieved rps:        1.02    ->  RPM  61.2
output tok/s:       14.65   ->  TPM (output)   879
total  tok/s:       69.88   ->  TPM (total)  4193
avg output tok:     26      ->  TPM (output) per request  ≈ 26
```

Vendor SLA tables usually quote **TPM (output)** or
**RPM**, sometimes both. The output-tokens one is the
honest number for generation cost; the RPM one is the
honest number for concurrency. When the contract only
quotes one of them, compare the right one and note the
other for the operator.

### Pulling the numbers from `summary.txt` via `jq`

For a one-line SLA report (e.g. for pasting into a vendor
ticket), parse the relevant numbers straight from
`metrics.json`:

```bash
# from WSL / git-bash, or replace with python -c on PowerShell
jq -r '
  "RPM:                 " + (.achieved_rps * 60 | tostring | .[0:6]),
  "TPM (output tok/s):  " + (.output_throughput_tps * 60 | tostring | .[0:6]),
  "TPM (total tok/s):   " + (.total_throughput_tps * 60 | tostring | .[0:6]),
  "p50 latency (ms):    " + (.avg_latency_ms | tostring | .[0:6]),
  "TTFT p50 (ms):       " + (.avg_ttft_ms     | tostring | .[0:6])
' ~/.power_test/history/<model>/<run>/metrics.json
```

PowerShell equivalent (no jq available):

```powershell
$j = Get-Content "$env:USERPROFILE\.power_test\history\<model>\<run>\metrics.json" | ConvertFrom-Json
"  RPM:                $($j.achieved_rps * 60)"
"  TPM (output):       $($j.output_throughput_tps * 60)"
"  TPM (total):        $($j.total_throughput_tps * 60)"
"  p50 latency (ms):   $($j.avg_latency_ms)"
```

## Pass / fail criteria

These are starting points, not contracts. Tune them per
model and per workload.

| Check | Pass | Investigate | Fail |
|---|---|---|---|
| **Achieved RPS** | ≥ 0.98 × target | 0.95-0.98 × target | < 0.95 × target |
| **Latency p50 (ours)** | ≤ 1.5 × upstream p50 | 1.5-2.0 × upstream | > 2.0 × upstream |
| **Latency p99 (ours)** | ≤ 2.0 × upstream p99 | 2.0-3.0 × upstream | > 3.0 × upstream |
| **TTFT p50 (ours)** | ≤ 1.3 × upstream p50 | 1.3-1.8 × upstream | > 1.8 × upstream |
| **TPM (output) SLA** | achieved ≥ committed | achieved 90-100% of committed | achieved < 90% of committed |
| **Errors** | 0 | < 2% of total | ≥ 2% of total |
| **Skipped ticks** | < 5% of scheduled | 5-15% of scheduled | > 15% of scheduled (back-pressure) |

The "investigate" zone is where you write a note in the
report ("p99 is 2.3× upstream's because of cross-region
NAT — accepted by vendor on ticket #1234") and move on.
The "fail" zone is where you don't sign the SLA / don't
commit to the customer.

## Hand-off checklist

For **vendor management** (upstream report):

- [ ] Achieved RPM / TPM vs vendor-published limit
- [ ] Latency p50 / p99 envelope (is the vendor's number
      honest?)
- [ ] Skipped ticks (was the vendor throttling under
      sustained load?)
- [ ] 5xx / transport errors in the run
- [ ] If any of the above is bad: open a vendor ticket
      with the `summary.txt` and `report.html` attached;
      the `compare-<a>-vs-<b>-<ts>.html` is the
      "we can do better" part of the negotiation.

For **customer success** (our report):

- [ ] Achieved RPM / TPM vs the SLA we are quoting
- [ ] Latency p50 / p99 vs our SLA
- [ ] Cache hit rate (turn 2+ should be high if our
      stack uses the upstream's prefix cache)
- [ ] If we don't meet the SLA: don't quote it. Either
      raise the SLA on the contract, or constrain the
      customer's usage.

For **engineering** (compare):

- [ ] Our latency overhead vs upstream at every percentile
- [ ] Our throughput delta (output tok/s) — should be
      within a few percent of upstream
- [ ] Where the gap is largest (TTFT? ITL p99? overall
      latency?) — points to where to invest

## What this workflow does NOT cover

- Different model families on the two sides — that's an
  M-series compare, not this one. Use `power_test compare
  --html` for a model-quality diff and ignore the
  upstream/our framing.
- Streaming vs non-streaming mismatches between the two
  sides — both runs must use the same `--stream` setting,
  otherwise ITL / TPS are not comparable.
- Different `--max-tokens` between the two sides — that
  changes TPS and the cache shape; not comparable.
- Long-soak stability (> 1 h) — both sides need a soak
  run with `--pattern soak`, and the TPM / RPM numbers
  should be re-evaluated on the latest snapshot, not the
  initial 60 s average.
- Cross-region latency — the compare shows it; the
  question is whether the customer's SLA accepts it.
