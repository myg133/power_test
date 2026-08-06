# Patterns × Datasets — when to reach for what

This is a quick lookup, not a tutorial. Use it after the SKILL.md
procedure has confirmed the user wants to actually run a test.

## Patterns

| Pattern | When to use | Required flags | Output behavior |
|---|---|---|---|
| `constant` (default) | Steady-state: "what's the achievable RPS at this load?" | `--rps`, `--duration` | RPS is fixed. Default. |
| `ramp` | Find the latency cliff: "at what RPS does p99 shoot up?" | `--rps-start`, `--rps-end`, `--duration` | RPS interpolates linearly from start to end. `--rps` is ignored. |
| `spike` | Recovery / back-pressure: "does the server crash under burst, and how fast does it recover?" | `--spike-at <SECS>` (repeatable), `--spike-rps`, `--spike-duration` | RPS is `--rps` (baseline) outside spikes; jumps to `--spike-rps` for `--spike-duration` at every `--spike-at`. |
| `soak` | Long-run stability: "does memory leak, queue length grow, or error rate creep up?" | `--rps`, `--duration`, optional `--soak-checkpoint` | Same as `constant` but writes a snapshot of `metrics.json` every `--soak-checkpoint` seconds (default 60, 0 disables). An interrupted soak is partially analyzable from the latest snapshot. |

### Picking the right duration

- Smoke / sanity: 30 s.
- Constant p99: 60 s minimum; 120 s preferred.
- Ramp: at least 60 s; the slope must be shallow enough that
  each second of duration covers a few requests per RPS step.
- Spike: cover at least 3 × the longest spike duration after the
  last spike, so recovery is captured.
- Soak: ≥ 30 min. Anything under 15 min will not catch slow
  memory growth.

## Datasets

| Dataset | When to use | Required flags | Notes |
|---|---|---|---|
| `literal` (default) | Single prompt, repeated. Quick smoke / regression. | `--prompt "..."` (optional; built-in fallback is the quantum-entanglement prompt) | All requests have the same prompt-tokens count. **For latency benchmarks, override the built-in prompt with `--prompt "Reply with the single word: ok"` and pair with `--max-tokens 16`**, otherwise you're benchmarking the model's essay-writing throughput, not the proxy's latency. |
| `token-budget` | One prompt, scaled to ~N tokens. Avoids the prompt-cache confound. | `--prompt-tokens 200` | Repeats a filler phrase until ~N tokens; cheap. |
| `built-in` | Realistic variation. Catches cache-dependent behavior. | `--dataset built-in` | 12-prompt pool of mixed English + Chinese. |
| `sharegpt` | Real conversation data. | `--dataset sharegpt --sharegpt-path <file>` | First `human` turn of each conversation. Skips conversations with no human turn. |
| `custom` | Your own prompts. | `--dataset custom --custom-path <file>` | File is JSON array or JSONL. `#` lines OK in JSONL; bad lines skipped with a warning. |

### Custom JSON / JSONL format

JSON array (`prompts.json`):
```json
[
  {"prompt": "Hello", "estimated_prompt_tokens": 1},
  {"prompt": "What is the capital of France?", "estimated_prompt_tokens": 7}
]
```

JSONL (`prompts.jsonl`), one prompt per line, `#` comments OK,
blank lines skipped, malformed lines skipped with a warning:
```jsonl
{"prompt": "Hello", "estimated_prompt_tokens": 1}
{"prompt": "What is the capital of France?", "estimated_prompt_tokens": 7}
# {"prompt": "ignored", "estimated_prompt_tokens": 9999}
```

`estimated_prompt_tokens` is for the prompt-distribution summary
in the HTML report only; it does not affect the actual request
body. If you don't know it, omit it (`{"prompt": "..."}` is fine).

## Picking a strategy

For multi-prompt datasets:

- `--request-strategy random` (default): xorshift64 PRNG, no
  extra dep. Visited set eventually covers all prompts.
- `--request-strategy round-robin`: deterministic order,
  useful when you want every prompt to be hit at a known cadence.

If you don't have a reason to prefer one, the default is fine.

## Composing patterns with datasets

| Combo | Typical use |
|---|---|
| `constant` + `literal` | Quick endpoint sanity check. |
| `constant` + `built-in` | "How does this server handle realistic prompt mix at 20 RPS?" |
| `ramp` + `built-in` | "Where does p99 TTFT start climbing?" — the prompt variety makes the ramp curve stable. |
| `spike` + `custom` | "What happens if a herd of users asks the same set of hard questions at once?" |
| `soak` + `sharegpt` | "Is there a slow leak under production-shaped traffic?" |

## TUI

`--tui` is orthogonal to pattern + dataset. Use it whenever a run
is expected to last longer than ~30 s and the operator is at a
terminal. Key bindings:

- `q` or `Esc`: cancel the run cleanly (drain in-flight, then
  save).
- `p`: pause / unpause (TUI-side, doesn't affect the runner
  semaphore).
- `c`: cancel, same as `q`.

If the user's terminal is < 80 × 24, the TUI may render poorly;
in that case drop `--tui` and watch the binary's `info` log
output instead.
