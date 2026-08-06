# M6 design: dataset profile + multi-turn sessions

## Goal

Two related changes, designed together so they compose cleanly:

1. **Custom dataset profile** — accept a TOML profile file in
   addition to the existing JSON/JSONL, with three richer fields
   per item: `weight`, `tags`, and `messages: Vec<{role, content}>`.
2. **Multi-turn execution modes** — a single dataset file picks
   one of three modes. The mode is determined by the items, not
   by a CLI flag:

   - **Single-turn** — every item has a `text` (or legacy `prompt`).
     This is the M1/M2 behavior. **Unchanged.**
   - **Static multi-turn** — every item has a `messages` array.
     Each request sends the full `messages` as the body. The model
     returns one assistant response. No session.
   - **Dynamic multi-turn** — every item has a `messages` array
     (the seed) and a `follow_ups` array of user-side messages.
     K parallel sessions, each turns its item's seed + follow_ups
     into a chain of requests. **Turns within a session are
     serial.** Sessions are destroyed after the last turn.

## Non-goals

- No `--sessions` / `--session-strategy` CLI flags. The session
  pool size K is the existing `--concurrency` value: each
  active session holds one concurrency slot.
- No mixed-mode dataset. A file is either all single-turn, all
  static multi-turn, or all dynamic multi-turn. The loader
  enforces this with a fail-fast `Error::InvalidConfig` if any
  item is inconsistent.
- No cross-item sessions. One session serves exactly one item.
- No session-state persistence across runs. A run is single-shot.
- No automatic history truncation. Long histories are bounded
  by the model's context window; rejections are surfaced as
  per-request errors.

## File-by-file changes

### 1. `src/dataset/mod.rs` — `DatasetItem` extended

```rust
pub struct DatasetItem {
    /// Single-turn convenience. Used when `messages` is `None`.
    pub prompt: String,
    /// Optional token estimate (carried from the loader).
    pub estimated_prompt_tokens: u32,
    /// Optional sampling weight. `None` ≡ `1`. `0` items are
    /// skipped.
    pub weight: Option<u32>,
    /// Optional tags. Carried through for report grouping.
    pub tags: Vec<String>,
    /// Optional stable name. Defaults to a synthetic id.
    pub name: Option<String>,
    /// Optional full `messages` array. When `Some`, the request
    /// body uses this directly (static multi-turn), or seeds
    /// the session pool (dynamic multi-turn).
    pub messages: Option<Vec<ChatMessage>>,
    /// Optional per-turn follow-up user messages. Empty
    /// `follow_ups` on an item with `messages` = static
    /// multi-turn. Non-empty `follow_ups` = dynamic multi-turn
    /// (session pool).
    pub follow_ups: Vec<String>,
}
```

The `Dataset` trait's `next(&self) -> DatasetItem` is unchanged.
The executor reads `item.messages` and `item.follow_ups` to pick
the execution path.

### 2. `src/dataset/custom.rs` — TOML profile support + mode detection

Today: JSON array or JSONL, schema is `{prompt: String}`.
Tomorrow: also accept TOML, schema:

```toml
# Single-turn profile (mode = single)
[[prompt]]
name = "smoke-hello"
text = "Hello"
weight = 2
tags = ["smoke", "short"]

# Static multi-turn profile (mode = static_multi)
# Every item has messages, no follow_ups.
[[prompt]]
name = "few-shot-qa"
messages = [
  { role = "system", content = "You are terse." },
  { role = "user",   content = "What is 2+2?" },
  { role = "assistant", content = "4." },
  { role = "user",   content = "And 3+3?" },
]
tags = ["qa", "few-shot"]

# Dynamic multi-turn profile (mode = dynamic_multi)
# Every item has messages AND follow_ups.
[[prompt]]
name = "long-qa"
messages = [
  { role = "system", content = "You are an analyst." },
  { role = "user",   content = "Summarize the Q3 report." },
]
follow_ups = [
  "Now compare to Q2.",
  "What were the top 3 risks?",
  "Recommend mitigations.",
]
tags = ["long"]
```

Auto-detect by extension: `.toml` → TOML; `.json` / `.jsonl` →
existing parser.

After parsing, walk the items once and assign the file's mode:

- All items have `text` only → **single**
- All items have `messages` but no `follow_ups` → **static_multi**
- All items have `messages` and at least one item has a non-empty
  `follow_ups` → **dynamic_multi**
- Anything mixed → `Error::InvalidConfig` citing the offending
  item name + line.

The mode is part of the `Dataset` trait return or a new
`dataset.mode()` method:

```rust
pub enum DatasetMode {
    Single,
    StaticMulti,
    DynamicMulti,
}
```

### 3. `src/config_io.rs` — `DatasetToml::Profile` variant

```rust
enum DatasetToml {
    // …existing variants…
    Profile { path: PathBuf },
}
```

TOML usage:

```toml
[dataset]
kind = "profile"
path = "datasets/qa.toml"
```

The `--dataset-profile <path>` CLI flag is **not added** — the
profile is selected via the existing `--dataset custom
--custom-path <path>` with the path pointing to a `.toml`
file. The custom loader's extension sniff already discriminates
JSON from TOML.

### 4. `src/runner/session.rs` — NEW module (M6c only)

```rust
pub struct Session {
    pub id: Uuid,
    pub item_name: String,
    pub turn: u32,                 // 1-indexed
    pub messages: Vec<ChatMessage>,
    pub last_used: Instant,
}

pub struct SessionPool {
    sessions: Mutex<Vec<Session>>,
    pub max_sessions: usize,        // = --concurrency
    pub strategy: SessionStrategy,  // LeastRecentlyUsed
}

pub enum TurnAction {
    /// Run another turn on this session. The caller sends
    /// `session.messages` and appends the assistant response.
    Continue,
    /// This item has no more turns. Destroy the session.
    Done,
}

pub struct PoolHandle<'a> {
    pool: &'a SessionPool,
    index: usize,
}

impl SessionPool {
    pub fn acquire(&self, item: &DatasetItem) -> Option<PoolHandle>;
    pub fn complete_turn(
        &self,
        handle: PoolHandle,
        assistant: String,
    ) -> TurnAction;
}
```

Turns within a session are serial: `acquire` returns a handle;
`complete_turn` appends the assistant response and returns
either `Continue` (caller sends the next turn) or `Done`
(caller drops the handle and the pool destroys the session).
There is no async wait inside the pool — the executor drives
seriality by awaiting the HTTP call between `acquire` and
`complete_turn`.

### 5. `src/runner/executor.rs` — multi-mode scheduling

Today:

```rust
loop {
    tick().await;
    spawn worker -> {
        let item = dataset.next().await;
        let m = client.send(&item.prompt, ...).await;
        record_completed(&m);
    }
}
```

New:

```rust
loop {
    tick().await;
    let item = dataset.next().await;
    spawn worker -> {
        match dataset.mode() {
            DatasetMode::Single =>
                send_single(client, &item, ...).await,
            DatasetMode::StaticMulti =>
                send_static_multi(client, &item.messages, ...).await,
            DatasetMode::DynamicMulti =>
                run_session(pool, client, &item, ...).await,
        }
    };
}
```

`run_session` is the serial-turn loop:

```rust
async fn run_session(pool, client, item) {
    let mut handle = match pool.acquire(item) {
        Some(h) => h,
        None => { skipped_tick(); return; }
    };
    loop {
        let m = client.send_messages(&handle.messages, ...).await;
        record_completed(&m);
        if !m.ok() { drop_session(handle); return; }
        if let Some(asst) = m.assistant_text() {
            match pool.complete_turn(handle, asst) {
                TurnAction::Continue => { handle = ...; }
                TurnAction::Done => return,
            }
        } else {
            return;  // no assistant text -> end
        }
    }
}
```

**Failure modes** (same as today's):
- Pool exhausted (K=concurrency all busy, more requests in
  flight) → `skipped_ticks`. Log once per N to avoid spam.
- Model returns non-2xx or empty assistant → drop the session,
  mark `item_failed`, end the run loop early.
- Cancellation → drain in-flight, do NOT release sessions; the
  pool is per-run and gets dropped anyway.

### 6. `src/client/{openai,anthropic,raw}.rs` — `send_messages`

New method on `LlmClient`:

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn send(&self, prompt: &str, est: u32) -> RequestMetrics;

    /// Send a multi-turn `messages` array. Default impl falls
    /// back to `send` by joining messages into a single
    /// user-role prompt. Clients with first-class multi-turn
    /// support override.
    async fn send_messages(
        &self,
        messages: &[ChatMessage],
        est: u32,
    ) -> RequestMetrics {
        let joined = messages
            .iter()
            .map(|m| format!("[{}] {}", m.role, m.content))
            .collect::<Vec<_>>()
            .join("\n");
        self.send(&joined, est).await
    }
}
```

`OpenaiClient` and `AnthropicClient` override with a real
multi-turn body shape. `RawClient` keeps the default. The
default impl is the backward-compat escape hatch — if a
client doesn't override, multi-turn still works (just with
all messages collapsed into one user prompt).

The override for `OpenaiClient` and `AnthropicClient` simply
swaps `vec![ChatMessage { role: "user", content: prompt }]`
for the supplied `messages`. No other changes.

### 7. `src/runner/metrics.rs` — session-related fields

Per-record:

```rust
pub struct RequestRecord {
    // …existing fields…
    pub session_id: Option<Uuid>,
    pub session_turn: Option<u32>,
    pub session_continuation: bool,  // false for turn 1
}
```

Aggregates:

```rust
pub struct MetricsAggregator {
    // …existing…
    pub session_count: u64,           // distinct sessions used
    pub session_turn_total: u64,      // sum of turns across sessions
}
```

### 8. CLI: **no new flags**

- `--sessions` / `--session-strategy`: **NOT added**.
- `--concurrency` already exists. K = `--concurrency`.

### 9. `src/report/{summary,html}.rs` — session stats

Summary adds (only when `session_count > 0`):

```
sessions:        K active, M created, 0 reused
turns/session:   mean 3.5, p50 3, p99 5
session errors:  2 (session dropped, retried fresh)
```

HTML report adds a per-turn latency chart.

## Test plan

| Layer | New tests |
|---|---|
| `dataset/custom.rs` | parses TOML single-turn; parses TOML static multi-turn; parses TOML dynamic multi-turn; rejects mixed mode with the offending item name; backward-compat JSONL still works; weight=0 skipped; empty file errors. |
| `dataset/mod.rs` | `DatasetItem` fields with `Option`/default. |
| `config_io.rs` | `Profile` variant round-trips through `--print-config`. |
| `runner/session.rs` | acquire returns same session on second turn; pool full ⇒ `None`; LRU picks oldest; failed turn drops session; release with empty assistant is a no-op; concurrent acquires respect K. |
| `runner/executor.rs` | multi-mode dispatch (single / static / dynamic) tested with mock dataset; session pool full ⇒ skipped tick; cancellation drops pool without panics. |
| `client/{openai,anthropic}.rs` | `send_messages` builds correct body shape; default `send_messages` impl in `LlmClient` collapses messages for `RawClient`. |
| e2e (`tests/e2e.rs`) | `e2e_dynamic_multi_session_pool_4_sessions_2_turns`: wiremock, 4 sessions, 2 turns each, assert `session_count == 4`, `session_turn_total == 8`. |

Total: ~30 new tests. Final test count: 178 unit + 12 e2e = **190** (was 148).

## Backward compatibility

- `--dataset custom` with JSON/JSONL: **unchanged behavior**.
- `--dataset literal | token-budget | built-in | sharegpt`:
  **unchanged behavior**. These four never go through the
  profile path; they keep the current single-turn semantics.
- `DatasetItem` adds fields with `Option`/default; existing
  constructors still compile. Public API surface unchanged.
- `RunConfig` adds nothing for M6a; M6c adds optional session
  fields with `#[serde(default)]`. Old `config.json` files
  deserialize unchanged.
- `metrics.json` adds session fields; old fields unchanged. The
  load path uses `#[serde(default)]` for everything new.

## Rollout / PR split

Three reviewable chunks:

| PR | Scope | Tests added | Risk |
|---|---|---|---|
| **M6a** — dataset profile only | Items 1, 2, 3 in the file list. `DatasetItem` carries `messages` and `follow_ups`; TOML profile parses; mode is detected; `config_io.rs::Profile` variant. **No executor changes.** | ~10 unit | Low. Pure additive. |
| **M6b** — multi-turn body | Item 6 (`send_messages` real impl on OpenAI + Anthropic). New `LlmClient` method with override. Default impl in the trait. | ~6 unit | Low. Existing single-turn path unchanged. |
| **M6c** — session pool + executor | Items 4, 5, 7, 9. Pool, executor rewrite, metrics, reports. **No new CLI flags** — `--concurrency` is the only K source. | ~14 unit + 2 e2e | **Medium**. Touches the hot path but the existing single-turn code is preserved as one of three branches. |

M6a is the foundation. M6b and M6c can each be merged
independently. M6c without M6b still works (default
`send_messages` impl in `LlmClient` collapses messages — less
faithful to API shape, but functional).

## Resolved questions

1. **Session lifetime** — destroyed after the item's last
   turn. K slot released.
2. **Cross-item sessions** — not supported. One session per
   item.
3. **Failure on session error** — drop the session, skip the
   remaining turns, end the run.
4. **CLI flag spelling** — no new flags. K is `--concurrency`.
5. **Multi-turn streaming behavior** — per-turn stats unchanged
   (TTFT/ITL/TPS measured per HTTP call).
6. **Turn serialization** — turns within a session are serial.
   Each turn's HTTP call must complete before the next fires.
7. **Mixed-mode dataset** — **not supported**. The loader
   fail-fasts on any inconsistency, citing the offending item.

## Estimated effort

| PR | Lines added | Lines removed | Hours |
|---|---|---|---|
| M6a | ~280 | 0 | 1.5 |
| M6b | ~120 | 0 | 1 |
| M6c | ~450 | ~80 (executor rewrite) | 4 |
| **Total** | **~850** | **~80** | **~6.5** |

Tests: 30 new. Docs: `docs/cli.md` notes that
`--concurrency` now also caps the session pool in
multi-turn mode; `references/patterns-and-datasets.md` adds
the three-mode example.
