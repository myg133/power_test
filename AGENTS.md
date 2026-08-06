# power_test

HTTP-level stress testing for LLM inference endpoints. M1 ships a constant-RPS executor
with an OpenAI-compatible client, HdrHistogram-backed LLM metrics (TTFT/ITL/TPS), an HTML
report, and history persistence. M2 extends M1 with load patterns (ramp / spike / soak)
and datasets (built-in pool, ShareGPT, custom JSON/JSONL) without breaking the existing
37 tests.

## M2 architecture decisions

- `RunConfig` gains `pattern: LoadPattern`, `dataset: DatasetSpec`, `strategy: RequestStrategy`,
  and `prompt_distribution: PromptDistribution`. The old `prompt: PromptSource` is
  re-homed as an internal helper for resolving the literal/token-budget sources.
- Load patterns expose a small `LoadPattern` trait (async `tick()` + `current_rps()` +
  `peak_rps()`). The scheduler stores it as `Arc<tokio::sync::Mutex<Box<dyn LoadPattern>>>`
  so we can swap implementations without changing the executor.
- Datasets also use a trait object. A new `PoolDataset` wraps a `Vec<DatasetItem>` and
  picks via `Random` or `RoundRobin`. `Random` uses a tiny xorshift64 PRNG to avoid a
  new dependency.
- Spike takes repeatable `--spike-at <secs>` with shared `--spike-rps` and
  `--spike-duration`. Soak spawns a checkpoint task that flushes the aggregator to
  `metrics.json` every `--soak-checkpoint` seconds so an interrupted long run is
  partially analyzable.
- Prompt distribution (min/max/mean/count of `estimated_prompt_tokens`) is computed at
  config time so the HTML report and `summary.txt` can show it without re-loading files.

## Commands

```bash
cargo build              # build (also runs on every change)
cargo test               # all tests (34 unit + N new unit + 3 e2e + 1 new e2e)
cargo check              # fast type-check loop
```

`#![deny(warnings)]` is on; unused imports / dead code fail the build. When refactoring,
keep M1 fixtures compiling by adding the new fields with sensible defaults.

## Layout

```
src/
  cli.rs                  # clap CLI; M2 adds --pattern / --dataset / spike/soak flags
  config.rs               # RunConfig, LoadPattern, DatasetSpec, RequestStrategy,
                          # PromptDistribution, prompt helpers
  dataset/
    mod.rs                # Dataset trait + build(spec, strategy) dispatcher
    simple.rs             # Literal / TokenBudget (M1 logic, new shape)
    builtin.rs            # hardcoded ~10-prompt pool
    sharegpt.rs           # ShareGPT JSON loader
    custom.rs             # custom JSON / JSONL loader
    pool.rs               # wraps Vec<DatasetItem> with random / round-robin
  runner/
    pattern.rs            # ConstantRps, RampPattern, SpikePattern, SoakPattern
    executor.rs           # pattern-agnostic scheduler + soak checkpoint task
    metrics.rs            # unchanged
  report/
    html.rs               # adds Pattern / Dataset / Strategy / Prompt Distribution cards
    summary.rs            # adds pattern + dataset lines
  storage/
    history.rs            # HistoryEntry gains pattern_name for the index
tests/
  e2e.rs                  # adds e2e_ramp_pattern
```

## Test inventory

M1 baseline: 34 unit + 3 e2e = **37 tests**.

M2 actual: 73 unit + 4 e2e = **77 tests** (40 new). All pass; no warnings.

New tests added:

- 6 pattern unit tests: `ramp_current_rps_increases_monotonically`,
  `ramp_peak_is_max_of_endpoints`, `spike_current_rps_reflects_phase`,
  `spike_peak_is_max_of_burst_and_baseline`, `ramp_tick_count_matches_elapsed_time`,
  `soak_pattern_is_just_constant`.
- 1 soak-checkpoint unit test: `soak_checkpoint_writes_file_and_exits_on_cancel`.
- 3 pool unit tests: `round_robin_visits_all_in_order`,
  `random_eventually_visits_all`, `empty_pool_returns_fallback`.
- 2 built-in dataset unit tests: `pool_has_mixed_sizes`, `pool_contains_chinese`.
- 5 ShareGPT unit tests: `parses_first_human_turn`,
  `skips_conversations_with_no_human_turn`, `missing_file_errors`,
  `malformed_json_errors`, `caps_at_max_prompts`.
- 7 custom dataset unit tests: `parses_json_array`, `parses_jsonl`,
  `jsonl_skips_blank_lines`, `jsonl_skips_bad_lines_with_warning`,
  `jsonl_all_bad_errors`, `missing_file_errors`, `malformed_json_array_errors`.
- 11 CLI unit tests covering all new pattern / dataset / strategy flags
  plus a couple of end-to-end config builds that load real temp files.
- 3 config unit tests: `load_pattern_peak_rps`, `dataset_spec_name`,
  `prompt_distribution_from_slice`.
- 2 report unit tests: `html_includes_m2_fields_for_ramp_and_builtin`,
  `summary_shows_pattern_and_dataset_lines`.
- 1 new e2e test using `wiremock`: `e2e_ramp_pattern_2s_2_to_8_rps` asserts
  ≥4 requests completed and achieved RPS is between 2 and 8.

All 37 M1 tests still pass after the RunConfig refactor (we updated the
test fixtures to include the new fields).

## Constraints

- Stable Rust, all async I/O, `#![deny(warnings)]`.
- No new external deps (xorshift64 instead of `rand`).
- Do not break M1. All 37 existing tests must still pass.
- Stable history format: config.json round-trips. New fields are additive.
