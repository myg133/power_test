# Build & test evidence

Snapshots of `cargo build` and `cargo test` from the M1 → M5 work.
The raw log files (`build_m3.log`, `test_m3c.log`) are kept on
local disk for one release cycle but not committed.

## M3 (compare subcommand landed)

`test_m3c.log` snapshot at the end of M3 — proves 86 unit + 5 e2e
tests passed with zero warnings:

```
test result: ok. 86 passed; 0 failed
test result: ok. 5 passed; 0 failed
```

## M5 final (post-thinking-fix)

`test_thinking_full.log` snapshot — 140 unit + 8 e2e tests pass
with zero warnings after the TTFT-includes-thinking change:

```
test result: ok. 140 passed; 0 failed
test result: ok. 8 passed; 0 failed
```

The two added tests:

- `client::openai::full_streaming_with_reasoning_content_counts_as_ttft`
- `client::anthropic::full_streaming_with_thinking_delta_counts_as_ttft`

## Reproducing

```bash
cargo build                  # 0 warnings, builds in ~5s (incremental)
cargo test --no-fail-fast    # 148 tests in ~7s
```

`#![deny(warnings)]` is on, so any unused import or dead code
breaks the build. `cargo build` failing is the canonical signal
that the workspace is not clean.
