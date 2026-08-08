---
name: power-test-onboard
description: |
  Onboard a new upstream model in a token-reseller setup:
  stress-test the vendor endpoint AND our own fronting
  endpoint with the same shape, then produce a 3-report SLA
  bundle (upstream TPM/RPM, our TPM/RPM, latency-overhead
  diff). Use when the team is its own token vendor and needs
  to validate TPM/RPM against the upstream contract AND
  the downstream SLA. Hands the three reports to
  vendor-management, customer-success, and engineering
  respectively. For a single-endpoint stress test use
  `power-test-run`. For a two-run diff without the SLA
  scaffolding use `power-test-compare`.
---

# power-test-onboard (3-report SLA workflow for new upstreams)

When the team is its own model-token vendor (your service
sits between the upstream model vendor and your downstream
customers), every new upstream model needs **two** stress
tests plus one diff:

1. **Upstream** — exercise the vendor's API directly. The
   numbers go on the contract / SLA negotiation table; if
   achieved TPM / RPM are below the vendor's published
   limit, open a ticket asking them to lift TPM / RPM.
2. **Our** — exercise your own endpoint that fronts the
   vendor. The numbers are the SLA you commit to your
   downstream customers. Verify our TPM / RPM actually
   meet what you promised.
3. **Compare** — diff the two runs. The interesting number
   is **our latency overhead vs upstream's** at the same
   model. Big overhead = our stack adds cost; the
   customer pays for that, so it has to be visible.

All three run with the **same model name and the same
shape** (RPS, duration, concurrency, prompt, max-tokens)
so the diff isolates the upstream↔our layer. History is
grouped by model (M6f), so both runs land in the same
`<root>/<model>/` folder and show up in each other's
compare-with dropdown. The per-model dashboard (M7)
lists both rows with their `-upstream` / `-our` tags.

## When to run

- A new upstream model is being brought online in a
  token-reseller setup.
- The vendor publishes a TPM / RPM; you want to know what
  you actually get before signing the contract.
- You commit a TPM / RPM SLA to your customers; you want
  to know what your stack actually delivers before
  quoting the number.
- The model is the same on both sides (you resell tokens
  for the same model the vendor sells). The point of the
  compare is your overhead, not a model-quality diff.

Skip this if the two sides serve different model families
— that's an M-series compare via `power-test-compare`,
not this workflow.

## Inputs to collect

- **`<upstream-url>`** and **`<upstream-key>`**: the
  vendor's chat-completions / messages endpoint and
  bearer / `x-api-key` token. For OpenAI-compatible
  vendors, the URL ends in `/v1/chat/completions`; for
  Anthropic, `/v1/messages`.
- **`<our-url>`** and **`<our-key>`**: your own endpoint
  that fronts the vendor. Same `--api` shape.
- **`<model>`**: the model name as the vendor names it
  (and as your endpoint exposes it). Use the **same**
  string for both runs — the model name is the
  group key in the history, and breaking it would put
  the two runs in different folders.
- **`<rps>` / `<duration>` / `<concurrency>`**: the
  shape. For a 27B-class model on a remote vendor, a
  sensible starting point is **1 RPS × 60s ×
  concurrency 2** — bump after the first run if
  achieved RPS is comfortably on target.
- **`<max-tokens>`**: keep it low (32) so the per-turn
  response is short and the cache pressure is visible.
  Both runs must use the same `--max-tokens`, otherwise
  TPS and the cache shape are not comparable.
- **`--stream`**: same on both sides (almost always
  `true`; you need streaming to get TTFT / ITL / TPS).
- **`<prompt>` / `<dataset>`**: same on both sides.
  For latency benchmarks, `--prompt "Reply with the
  single word: ok" --max-tokens 16`. For multi-turn
  cache tests, use the M6 multi-turn TOML dataset.
- **SLA TPM / RPM (optional)**: if you have a contracted
  number, pass it to the SLA check in step 4. Without
  it, the pass/fail thresholds default to the
  "Investigate" zone in `references/onboarding.md`.

## Tag convention (suffix form)

Append a role suffix to the tag so `power_test list` and
the per-model dashboard make the role obvious:

```
<model>-rps<N>-dur<S>-upstream     # vendor-side run
<model>-rps<N>-dur<S>-our         # our-side run
```

Example for `qwen36-27B` at 1 RPS × 60 s:

```
--tag 'qwen36-27b-rps1-dur60-upstream'
--tag 'qwen36-27b-rps1-dur60-our'
```

Both runs keep the **same `--model`** (the model name is
the group key, not the tag). Don't use `--model-alias`
for this — alias is for grouping **dated snapshots of the
same logical model** (M6g); upstream/our is a different
axis.

## Procedure

1. **Upstream run** — the vendor side.

   ```powershell
   $env:OPENAI_API_KEY = '<upstream-key>'
   & D:\MyCodes\Rust\power_test\target\debug\power_test.exe run `
       --target '<upstream-url>' `
       --api openai --model <model> `
       --rps <N> --duration <S> --concurrency <K> `
       --dataset custom `
       --custom-path D:\MyCodes\Rust\power_test\docs\examples\datasets/multi-turn-conversation.toml `
       --request-strategy round-robin `
       --max-tokens <T> --stream true `
       --tag '<model>-rps<N>-dur<S>-upstream'
   ```

   The terminal prints the run id (e.g.
   `20260808-11-28-15-1db22a`). Capture it as `RUN_UPSTREAM`.

2. **Our run** — your side. Override the env var so we
   don't accidentally hit the vendor with our key:

   ```powershell
   $env:OPENAI_API_KEY = '<our-key>'
   & D:\MyCodes\Rust\power_test\target\debug\power_test.exe run `
       --target '<our-url>' `
       --api openai --model <model> `
       --rps <N> --duration <S> --concurrency <K> `
       --dataset custom `
       --custom-path D:\MyCodes\Rust\power_test\docs/examples/datasets/multi-turn-conversation.toml `
       --request-strategy round-robin `
       --max-tokens <T> --stream true `
       --tag '<model>-rps<N>-dur<S>-our'
   ```

   Capture as `RUN_OUR`.

3. **Compare** — the diff.

   ```powershell
   & D:\MyCodes\Rust\power_test\target\debug\power_test.exe compare `
       $RUN_UPSTREAM $RUN_OUR --html
   ```

   The compare-*.html lands at
   `~/.power_test/history/compare-<RUN_UPSTREAM>-vs-<RUN_OUR>-<ts>.html`.

4. **Extract TPM / RPM** — the SLA numbers. Both
   `summary.txt` and the dashboard carry them, but
   `metrics.json` is the source of truth for piping
   into a ticket or a sheet:

   ```powershell
   function Get-SlaNumbers {
       param([string]$RunId, [string]$Model)
       $j = Get-Content "$env:USERPROFILE\.power_test\history\$Model\$RunId\metrics.json" | ConvertFrom-Json
       [PSCustomObject]@{
           run_id          = $RunId
           rpm             = [math]::Round($j.achieved_rps * 60, 1)
           tpm_output      = [math]::Round($j.output_throughput_tps * 60, 0)
           tpm_total       = [math]::Round($j.total_throughput_tps * 60, 0)
           p50_latency_ms  = [math]::Round($j.avg_latency_ms, 1)
           ttft_p50_ms     = [math]::Round($j.avg_ttft_ms, 1)
       }
   }
   $up = Get-SlaNumbers -RunId $RUN_UPSTREAM -Model '<model>'
   $us = Get-SlaNumbers -RunId $RUN_OUR     -Model '<model>'
   $up; $us
   ```

5. **Hand off the three reports:**

   - **Vendor management** gets the upstream `summary.txt`
     + `report.html`. If achieved TPM / RPM are below
     the vendor's published limit, attach the report
     to a vendor ticket asking them to lift TPM / RPM.
   - **Customer success** gets the our-side `summary.txt`
     + `report.html`. If our TPM / RPM are below the
     SLA you quoted, don't quote that SLA; either
     raise it on the contract, or constrain the
     customer's usage.
   - **Engineering** gets the compare-*.html. Look at
     the latency-overhead columns (our p50 / p99 vs
     upstream's). That is the cost your layer adds
     per request.

## Pass / fail criteria

These are starting points, not contracts. Tune them per
model and per workload. Full table in
`references/onboarding.md`.

| Check | Pass | Investigate | Fail |
|---|---|---|---|
| **Achieved RPS** | ≥ 0.98 × target | 0.95-0.98 × target | < 0.95 × target |
| **Latency p50 (ours)** | ≤ 1.5 × upstream p50 | 1.5-2.0 × upstream | > 2.0 × upstream |
| **Latency p99 (ours)** | ≤ 2.0 × upstream p99 | 2.0-3.0 × upstream | > 3.0 × upstream |
| **TTFT p50 (ours)** | ≤ 1.3 × upstream p50 | 1.3-1.8 × upstream | > 1.8 × upstream |
| **TPM (output) SLA** | achieved ≥ committed | achieved 90-100% of committed | achieved < 90% of committed |
| **Errors** | 0 | < 2% of total | ≥ 2% of total |
| **Skipped ticks** | < 5% of scheduled | 5-15% of scheduled | > 15% of scheduled (back-pressure) |

## Output contract

- `~/.power_test/history/<model>/<RUN_UPSTREAM>/report.html`
  and `summary.txt` — vendor side.
- `~/.power_test/history/<model>/<RUN_OUR>/report.html`
  and `summary.txt` — our side.
- `~/.power_test/history/compare-<RUN_UPSTREAM>-vs-<RUN_OUR>-<ts>.html`
  — diff.
- `~/.power_test/history/<model>/index.html` — per-model
  dashboard (M7) listing both runs side-by-side.

All three reports go to different teams. See
`references/onboarding.md` for the hand-off checklist
per recipient.

## What this workflow does NOT cover

- **Different model families on the two sides** — that's
  an M-series compare, not this one. Use
  `power-test-compare` for a model-quality diff and
  ignore the upstream/our framing.
- **Streaming vs non-streaming mismatches** — both runs
  must use the same `--stream` setting, otherwise ITL
  / TPS are not comparable.
- **Different `--max-tokens` between the two sides** —
  that changes TPS and the cache shape; not comparable.
- **Long-soak stability (> 1 h)** — both sides need a
  soak run with `--pattern soak`, and the TPM / RPM
  numbers should be re-evaluated on the latest
  snapshot, not the initial 60 s average.
- **Cross-region latency** — the compare shows it; the
  question is whether the customer's SLA accepts it.

## See also

- `references/onboarding.md` — full walkthrough, TPM /
  RPM math, `jq` one-liner for the SLA report,
  per-recipient hand-off checklist.
- `power-test-run` — the single-endpoint stress test
  (one of the two building blocks).
- `power-test-compare` — the two-run diff (the other
  building block).
