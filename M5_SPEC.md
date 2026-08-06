# M5 spec — TOML config + docs + polish (DRAFT)

## Goal
Add a TOML config file (so users can save their run setup), polish UX, and write user-facing docs. This is the last milestone.

## M5 deliverables

### 1. TOML config (`src/config_io.rs`)

Add a `config_io` module that reads a TOML file and builds a `RunConfig` from it. CLI flags override TOML fields (CLI wins).

File format (`power_test.toml` example):
```toml
# Defaults applied to every run. CLI flags override these.
target = "https://api.openai.com/v1/chat/completions"
api = "openai"  # or "anthropic", "raw"
model = "gpt-3.5-turbo"
rps = 10
duration = 60
concurrency = 256
max_tokens = 256
stream = true
tag = "baseline"
api_key_env = "OPENAI_API_KEY"  # name of env var to read (NOT the key itself)

[pattern]
kind = "constant"  # or "ramp" / "spike" / "soak"
# for ramp:
# start = 2.0
# end = 8.0
# for spike:
# baseline = 1.0
# spikes = [{ at_secs = 5.0, rps = 50.0, duration_secs = 2.0 }]

[dataset]
kind = "literal"  # or "token-budget" / "built-in" / "sharegpt" / "custom"
# text = "Explain quantum entanglement"
# target_tokens = 200
# path = "data/sharegpt.json"  # for sharegpt / custom

strategy = "random"  # or "round-robin"

# Raw HTTP only
# raw_body_file = "body.json"
# raw_content_type = "application/json"
```

Public API:
- `pub fn load(path: &Path) -> Result<TomlConfig>` — parse TOML
- `pub fn merge_into_run_args(base: TomlConfig, cli: &mut RunArgs)` — apply defaults to a `RunArgs` only where the user didn't pass the flag
- `pub fn find_default() -> Option<PathBuf>` — look in `./power_test.toml` then `~/.power_test/config.toml`

### 2. CLI wiring (`src/cli.rs` + `src/main.rs`)

- Add `--config <path>` to the top-level `Cli` (not per-subcommand).
- Add `--print-config` to `run`: dump the effective merged config (TOML + CLI overrides) as a TOML snippet and exit.
- In `cmd_run`:
  1. Load `TomlConfig` from `--config` or the default path (errors clearly if file is malformed).
  2. Merge into a fresh `RunArgs` clone before parsing.
  3. Proceed as before.

Validation:
- `target` is required after merge (CLI or TOML)
- `api` must be one of the known values
- For `ramp`/`spike`/`soak` patterns, the per-pattern fields must be present after merge

Tests:
- `load_valid_toml_parses_all_fields`
- `load_missing_file_errors`
- `malformed_toml_errors`
- `merge_preserves_cli_overrides` (CLI flag wins)
- `merge_applies_toml_defaults_when_cli_omits` (e.g. duration, model)
- `merge_pattern_ramp_requires_endpoints` (errors if neither CLI nor TOML has them)
- `find_default_prefers_cwd_over_home`
- `print_config_dumps_effective_settings` (call `--print-config` end-to-end via assert_cmd or by invoking the binary; if assert_cmd is too heavy, just unit-test the function)
- e2e: `e2e_toml_config_loaded` — write a TOML, run via `power_test --config tmp.toml run --duration 1`, verify it works

### 3. Cargo.toml

Add one new dep:
```toml
toml = "0.8"
```

(That's it. No new dep for M5 beyond the parser.)

### 4. Polish

- `--quiet` flag on `run`: suppress info-level tracing (still show errors)
- `--log-level <level>` flag on the top-level `Cli`: `error`/`warn`/`info`/`debug`/`trace`, default `info`
- `power_test --version` should print cleanly (clap default behavior; verify)
- `power_test run --help` should fit in 80x24 and list all flags grouped by milestone
- On unknown CLI flags, clap error should suggest the closest match (already on with `clap::Command` derive; verify)

### 5. Docs

`README.md` (rewrite existing or write fresh):
- One-paragraph elevator pitch
- Quickstart: 5-line example with a real public endpoint
- "What it measures": TTFT, ITL, TPS, latency percentiles
- "Patterns": constant / ramp / spike / soak with one example each
- "Datasets": literal / token-budget / built-in / ShareGPT / custom with example file formats
- "Compare": how to diff two historical runs, what the deltas mean
- "TUI": when to use it, key bindings
- "Config": the TOML format and how CLI overrides work
- "API support": which endpoints work with which `--api` flag
- "Limitations": what M5 doesn't do (no Prometheus export, no auth beyond bearer, etc.)
- "Building from source": `cargo build`, `cargo test`
- "License": MIT

`docs/cli.md` — generated from `clap` markdown help, or hand-written if generation is too fiddly. Top-level commands + per-command flag tables.

### 6. Audit checklist for the verifier

The verifier should be able to confirm:
- `cargo build` 0 warnings
- `cargo test` 100% pass (existing 91 + new M5 tests + M4 tests added by the prior coder task)
- `power_test --help` renders cleanly
- `power_test run --help` lists every flag, grouped sensibly
- `power_test --print-config` with a TOML file dumps the merged settings
- `power_test --config tmp.toml run --duration 1 --rps 1 --target http://localhost:1` errors with a clear message (network unreachable)
- `power_test compare <a> <b>` still works (M3 regression)
- `power_test list` still works
- `power_test report <id>` still works
- README has all required sections
- No `unwrap()` in library code (linter scan)
- No new external deps beyond `toml = "0.8"`

## Constraints (carry over from M1-M4)

- `#![deny(warnings)]`
- Stable Rust
- No new external deps beyond `toml = "0.8"`
- All M1+M2+M3+M4 tests must still pass
- RunConfig new fields stay `#[serde(default)]`
- Don't touch `target/`, `~/.power_test/history/`, `Cargo.lock`

## Deliverable format

Same as M4:
- "BUILD: pass" or "BUILD: fail" + the error
- "TESTS: <n> passed, <n> failed" from the last `cargo test`
- "NEW FILES: config_io.rs" + docs
- "MODIFIED FILES: ..."
- "OPEN ITEMS: ..."
