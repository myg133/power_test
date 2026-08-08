# Reading the compare page (two-run diff)

The compare skill (`power_test compare <A> <B> [--html]`)
produces a side-by-side diff between two runs. Without
`--html` the diff is plain text (ANSI-colorized when stdout
is a TTY); with `--html` it is a self-contained
`compare-<a>-vs-<b>-<ts>.html` page.

This file is the **two-run diff** guide. For single-run
metrics (latency / TTFT / ITL / TPS / cache / M7 stuff /
TPM-RPM math), see
`power-test-run/references/report-interpretation.md`. For
the upstream-vs-our 3-report SLA workflow, see
`power-test-onboard/references/onboarding.md`.

## Color rules

The diff page colors each cell by the direction the
metric is moving:

- **Green**: improvement. For latency / TTFT / ITL, lower
  is better, so B < A is green. For TPS / RPS / success
  rate / cache hit rate, higher is better, so B > A is
  green.
- **Red**: regression. The opposite of green.
- **Grey**: unchanged. The 0.5% tolerance threshold means
  a < 0.5% delta is colored neutral (the math is computed
  but the color stays muted).

The color is per-direction, not per-metric: latency
up = good (got faster), latency down = bad (got
slower). The page header also calls out the direction
of each metric so the reader doesn't have to memorize
"which way is up for this one".

## What the diff page contains

The page renders every metric three times: A's value,
B's value, the absolute delta. The metric set is the
union of "single-run interpretation" (latency p50/90/99/99.9,
TTFT p50/99, ITL mean/99, TPS mean/99) plus
comparator-only rows (achieved RPS, total/success/error
counts, cache hit rate, throughput, prompt-cache
breakdown).

For the upstream-vs-our pattern (see
`power-test-onboard`), the most important rows to read:

- **Latency p50 / p99 (ours)**: how much latency our
  layer adds on top of upstream's. **Our p50 should
  be ≤ 1.5× upstream's** for a same-region, single-hop
  setup; **our p99 should be ≤ 2.0× upstream's**.
  Multi-hop / cross-region legitimately costs more and
  that has to be on the SLA page.
- **TTFT p50 / p99 (ours)**: prefill overhead. **Our
  p50 should be ≤ 1.3× upstream's.** Bigger gaps
  usually mean our stack is serializing streaming
  or padding the system prompt.
- **TPS mean (ours)**: generation throughput. Should
  be within a few percent of upstream's. Big drops
  = our layer is bottlenecking the model.
- **Cache hit rate, turn 2+ (ours)**: prefix-cache
  forwarding. If we don't pass through the upstream's
  cache, this drops to ~0 and the customer pays for
  full prefill every turn.
- **Achieved RPS**: both sides should hit the same
  target. If ours is lower, our layer is dropping
  requests under load.

## What to look for in a regression report

When the diff is a code / config / endpoint change (not
the upstream-vs-our pattern), the diagnostic priorities
change:

- **Latency p99 went up but p50 didn't**: tail
  regression. Often a single bad shard or a new slow
  code path. Look at the per-status-code histogram in
  the underlying single-run reports for hints.
- **Achieved RPS dropped but target was the same**:
  back-pressure kicked in. Either the new build is
  slower, or the server capacity changed. Compare the
  skipped-tick counters in `metrics.json`.
- **TTFT unchanged but ITL regressed**: prefill is
  fine, decode got slower. Often a kernel change.
- **TPS mean up but latency p50 also up**: probably
  the model is now writing longer answers (check
  `avg_output_tokens`). Not a regression of the
  endpoint, a regression of the prompt that was
  measured.
- **Success rate dropped**: real bug, not just
  performance drift. Investigate before merging.

## Compare header warnings

The compare page prints a small banner at the top of
the diff table when the two runs aren't a "natural"
pair:

- **`different target`**: the `--target` URLs differ
  between A and B. **This is the upstream-vs-our
  pattern, by design** — the diff is the cost your
  layer adds. See
  `power-test-onboard/references/onboarding.md`.
- **`different model alias`**: the `--model-alias`
  differs. This is the dated-snapshot pattern (M6g);
  the diff is the cost of the model change. Note the
  diff because alias is for grouping, not for hiding
  model switches.
- **`different stream` / `different max-tokens` /
  `different pattern`**: the shapes differ. The diff
  is still computed but the comparison is not
  apples-to-apples. Re-run with matched shape before
  drawing conclusions.

## How the math works

`Delta::new(a, b)` computes:

```
abs = b - a
pct = 100 * (b - a) / a   when a != 0
       null               when a == 0
```

`color_class(delta, direction)` looks at `pct`:

- If `direction == "neutral"` (duration, total tokens) or
  `pct == null` → grey
- If `|pct| < 0.5` → grey (the tolerance threshold)
- For `direction == "up"` (higher-is-better metrics):
  positive `pct` → green, negative → red
- For `direction == "down"` (lower-is-better metrics):
  positive `pct` → red, negative → green

The same math runs in the per-model dashboard's
"对比" (compare) view, so the diff you see in the
browser matches the diff in the standalone compare-*.html
file.

## Cross-references

- `power-test-run` — for a single-endpoint stress test
  (and for the metrics the diff is built on top of).
- `power-test-onboard` — for the 3-report SLA workflow
  that the upstream-vs-our pattern comes from.
