---
name: power-test-compare
description: |
  Compare two `power_test` runs side-by-side (HTML diff page
  or text report). Use when the user has two run_ids and wants
  a delta across latency / TTFT / ITL / TPS / cache /
  throughput, OR when they want a regression report after a
  code / config / endpoint change. Cross-target compare is
  supported (same model, different endpoint) and produces a
  "different target" warning by design — that is the
  upstream-vs-our pattern. For the 3-report SLA workflow
  (vendor + ours + diff) use `power-test-onboard` instead.
  For a single-endpoint stress test use `power-test-run`.
---

# power-test-compare (two-run side-by-side diff)

This is the diff skill. The inputs are two run_ids; the
output is `compare-<a>-vs-<b>.html` (with `--html`) or a
text delta. Cross-target compare is supported — the
compare header flags `different target` by design, which
is the whole point of the upstream-vs-our pattern in
`power-test-onboard`.

## Inputs

- **`<RUN_A>` / `<RUN_B>`**: the two `power_test run` ids.
  These are the timestamped strings printed in the run
  output, e.g. `20260808-11-28-15-1db22a`. Both runs must
  be in the history (`~/.power_test/history/<group>/<id>/`).
- **`--history-dir <path>`** (optional): override the
  default history root. Useful for CI or eval harnesses
  that pin a known location.
- **`--html`**: render the compare as a self-contained
  HTML page. Without it, the diff is plain text (ANSI-
  colorized when stdout is a TTY).
- **`--no-color`**: when running in a non-TTY capture (CI
  log, redirected to file) where ANSI codes would corrupt
  the output.

## Procedure

1. **Get the two run_ids.** The user usually has them in
   their recent terminal output. If not, list them:

   ```powershell
   & D:\MyCodes\Rust\power_test\target\debug\power_test.exe list
   ```

   Or open the per-model dashboard at
   `~/.power_test/history/<model>/index.html` and read the
   run_ids from the table (M7).

2. **Run compare.**

   ```powershell
   & D:\MyCodes\Rust\power_test\target\debug\power_test.exe compare `
       <RUN_A> <RUN_B> --html
   ```

   Output lands at
   `~/.power_test/history/compare-<RUN_A>-vs-<RUN_B>-<ts>.html`.

3. **Open the HTML.** The compare page is the
   authoritative view. It renders a side-by-side table
   with A's value, B's value, the absolute delta, and the
   percent delta, color-coded by direction:
   - **Green**: improvement. For latency / TTFT / ITL,
     lower is better, so B < A is green. For TPS / RPS /
     success rate, higher is better, so B > A is green.
   - **Red**: regression. The opposite.
   - **Grey**: unchanged. The 0.5% tolerance threshold
     means a < 0.5% delta is colored neutral.

4. **Or read the text output.** Without `--html`, the
   same delta is printed as a plain-text table — useful
   for piping into a log or PR description.

## What the diff contains

The compare page renders every metric three times: A's
value, B's value, the delta. The metric set:

- **Latency p50 / p90 / p99 / p99.9** — the headline
  numbers. p99 is the one to watch for regressions.
- **TTFT p50 / p99** — time-to-first-token. Drops here
  usually mean a faster prefill path; increases usually
  mean the prefill is now longer (e.g. a system prompt
  got bigger, or the new model has a heavier prefill).
- **ITL mean / p99** — inter-token latency. p99 spikes
  often correlate with a model that started a
  long-running speculative step.
- **TPS mean / p99** — generation throughput. Often
  trades against TTFT (faster prefill → more tokens, or
  vice versa).
- **Achieved RPS** — what the runner actually delivered.
  Drops with stable target RPS mean back-pressure; with
  `--pattern ramp` it can also mean the new build
  crashed mid-run.
- **Total / success counts**, **errors**, **prompt
  cache** hit rate.
- **Throughput** (output tok/s, total tok/s, TPOT, avg
  input / output tokens, avg turns/req).

For a token-reseller upstream-vs-our diff (the
`power-test-onboard` pattern), the metrics to focus on
are the latency overhead columns:
- **Latency p50 / p99**: our p50 should be ≤ 1.5×
  upstream's; our p99 should be ≤ 2.0× upstream's
  (single-hop, same-region). Multi-hop / cross-region
  legitimately costs more and that has to be on the SLA
  page.
- **TTFT p50 / p99**: our p50 should be ≤ 1.3×
  upstream's.
- **TPS mean**: within a few percent of upstream. If our
  is dramatically lower, our layer is serializing
  streaming.

## Failure handling

- **`run A not found` / `run B not found`**: one of the
  run_ids is wrong or the run was moved. `power_test
  list` shows what's actually in the history.
- **`index.json is corrupt` / falling back to scan**:
  the history index is broken. Compare still works
  (it scans the directory), but slower. Fix the index
  by re-saving any one of the listed runs (its
  `save_run` rebuilds the index).
- **Compare header says `different target` / `different
  model alias`**: by design. These are the upstream-vs-
  our pattern, or the dated-snapshot pattern (M6g).
  The diff is still computed; just note that the
  comparison is across different logical surfaces.

## Cross-target compare (the onboarding use case)

Same model, different target URL is the upstream-vs-our
pattern. The compare page renders the diff; the header
flags it as `different target`. To read the diff:

1. Open `compare-<upstream>-vs-<our>.html`.
2. Look at the latency overhead columns (see
   "What the diff contains" above).
3. Check the prompt-cache hit rate: our `turn 2+`
   should be close to upstream's if our stack is
   correctly forwarding the prefix.
4. The throughput delta is usually small (within a
   few percent of upstream). A big delta = our layer
   is bottlenecking.

The full 3-report SLA workflow — vendor side + our
side + latency-overhead diff with hand-off checklists
to vendor-management / customer-success / engineering
— lives in `power-test-onboard`.

## See also

- `references/compare-interpretation.md` — what each
  diff row means, the color-class rules, the upstream
  vs our latency-overhead budget, and the
  regression-shapes to call out.
- `power-test-run` — for a single-endpoint stress test
  (and for how to find the run_ids in the first place).
- `power-test-onboard` — the 3-report SLA workflow
  built on top of run + compare.
