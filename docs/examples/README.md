# Examples

Copy-paste-ready templates for `power_test`. Every file is
intended to be edited before use — nothing here is wired into
the build or treated as a fixture.

## Layout

```
examples/
├── README.md                              ← this file
├── power_test.toml                        ← full config template (CLI + TOML)
└── datasets/
    ├── multi-turn-conversation.toml       ← M6 dynamic_multi (with follow_ups)
    ├── static-multi-conversation.toml     ← M6 static_multi (messages[] only)
    ├── single-turn-prompts.json           ← custom JSON array
    ├── single-turn-prompts.jsonl          ← custom JSONL (with comments)
    └── sharegpt-mini.json                 ← ShareGPT format
```

## `power_test.toml`

Full schema dump of every top-level field, every pattern
kind, every dataset kind, and the auth / alias / strategy
config. Copy to `./power_test.toml` or `~/.power_test/config.toml`
and edit.

`power_test run --config power_test.toml` then reads it;
CLI flags still win on every field.

## `datasets/multi-turn-conversation.toml` (M6 dynamic_multi)

A TOML profile where each item is a *conversation* — a seed
`messages` array plus a `follow_ups` array. The session pool
runs K (= `--concurrency`) parallel sessions, each driving
its own serial chain of turns. Use this when the back-and-forth
matters: cache behavior, follow-up latency, multi-turn
throughput.

3 example conversations:
- `weather-followup` — 3 turns, short English
- `math-tutor` — 4 turns, reasoning
- `code-review` — 2 turns, code

## `datasets/static-multi-conversation.toml` (M6 static_multi)

A TOML profile where each item is a single multi-message
request — no follow-ups, no session. Use this to benchmark
the model with realistic prompt shapes (system + few-shot
+ question) when the conversation's dynamics aren't the
point.

3 example items:
- `fewshot-translation` — system + 2-shot examples + real question
- `long-system-qa` — long system prompt + single user question
- `code-lookup` — single short user question

## `datasets/single-turn-prompts.json` and `.jsonl`

The classic single-turn custom dataset. Both files contain
the same 5 prompts. Use `.jsonl` for large datasets (streaming
load, no full-file parse), `.json` for small ones you might
hand-edit.

`estimated_prompt_tokens` is for the prompt-distribution
summary in the HTML report only — it does not affect the
request body. Omit if you don't know it.

## `datasets/sharegpt-mini.json`

A 3-conversation ShareGPT-format file. The loader takes only
the first `human` turn per conversation, so the size of
`gpt` turns is irrelevant. Use the real ShareGPT dump from
[ShareGPT.com](https://sharegpt.com) (or any of the public
archives) for realistic chat-shaped prompts.

## See also

- `src/config_io.rs` — the canonical `TomlConfig` struct
- `src/dataset/custom.rs` — TOML profile parser
- `src/dataset/sharegpt.rs` — ShareGPT loader
- `docs/cli.md` — every CLI flag with one-line description
- The Mavis skill `power-test` for the end-to-end procedure
