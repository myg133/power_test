//! Stateful session pool for the `dynamic_multi` dataset mode.
//!
//! A session keeps a `messages` array across multiple turns:
//!
//! - Turn 1: send the seed `messages` (e.g. `[user] What is 2+2?`),
//!   append the assistant response to the session.
//! - Turn 2: send the session's grown `messages` plus the next
//!   `follow_ups[i]`, append the response, etc.
//!
//! Turns within a session are strictly serial: turn N+1 does not
//! fire until turn N's HTTP call has returned. Sessions are
//! parallel: at most `max_sessions` (= `--concurrency`) are
//! active at once.
//!
//! The pool is a small in-process state machine. No async wait
//! happens inside the pool — the executor drives seriality by
//! awaiting the HTTP call between `acquire` and `complete_turn`.
//!
//! ## Why `HashMap<Uuid, …>` instead of `Vec<…>`
//!
//! Multiple workers can race: worker A holds `PoolHandle { index = 0 }`,
//! worker B holds `PoolHandle { index = 1 }`. If A calls
//! `drop_session()` it calls `swap_remove(0)` and shifts B's
//! entry down to index 0. B's next access to `index = 1` is then
//! out of bounds. Storing sessions by stable `Uuid` key avoids
//! the index-shift problem entirely.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use uuid::Uuid;

use crate::dataset::{DatasetItem, OwnedChatMessage};

/// One in-flight conversation. The session owns its `messages` so
/// appending a turn can never invalidate borrowed references.
#[derive(Debug)]
pub struct Session {
    pub item_name: String,
    /// 1-indexed. Turn 1 sends the seed messages; turn N > 1 sends
    /// the grown messages plus `follow_ups[N-2]` as a new user
    /// message.
    pub turn: u32,
    pub messages: Vec<OwnedChatMessage>,
    pub last_used: Instant,
    /// M9: server-side response id from the last successful turn
    /// (only meaningful for the `/v1/responses` API; `None` for
    /// the other clients and on the very first turn). The executor
    /// reads this on turn N+1 and passes it as
    /// `previous_response_id` to the LlmClient, enabling stateful
    /// multi-turn conversation without re-sending prior `input`
    /// items.
    pub response_id: Option<String>,
}

impl Session {
    fn new(item_name: String, seed: Vec<OwnedChatMessage>) -> Self {
        Self {
            item_name,
            turn: 0,
            messages: seed,
            last_used: Instant::now(),
            response_id: None,
        }
    }

    /// After a successful turn, append the assistant response and
    /// bump the turn counter. Returns `true` if there are more
    /// turns to run, `false` if the session is done.
    fn complete_turn(&mut self, assistant: String, has_more_follow_ups: bool) -> bool {
        self.messages
            .push(OwnedChatMessage::new("assistant", assistant));
        self.turn += 1;
        self.last_used = Instant::now();
        has_more_follow_ups
    }
}

/// What the caller should do after appending a turn's assistant
/// response. `Continue` means: send the next turn. `Done` means:
/// destroy the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnAction {
    Continue,
    Done,
}

/// Outcome of a turn that should be passed to the pool after the
/// HTTP call returns.
pub struct TurnResult {
    pub action: TurnAction,
    /// The session's final turn count (post-completion). `0` if the
    /// session was destroyed mid-turn.
    pub turns_completed: u32,
    /// `true` when the session was destroyed early because the
    /// turn failed.
    pub dropped: bool,
}

/// A handle the executor holds while it owns a session. The handle
/// is a `Uuid` key into the pool's `HashMap<Uuid, Session>`;
/// `drop_session` removes the entry, and the handle becomes
/// stale. All accessors panic (with a clear message) if the
/// session was already dropped — that's the only way to misuse
/// the handle.
pub struct PoolHandle<'a> {
    pool: &'a SessionPool,
    id: Uuid,
}

/// Pool of sessions, capped at `max_sessions`. The pool is
/// short-lived: it lives for one run and is dropped on completion.
pub struct SessionPool {
    /// All sessions, busy or idle, keyed by stable UUID. The
    /// `Mutex<HashMap<…>>` is held for every operation, but
    /// contention is bounded by `max_sessions` (typical 8-256),
    /// so a coarse lock is fine.
    sessions: Mutex<HashMap<Uuid, Session>>,
    /// Configured upper bound on the number of simultaneous
    /// sessions. Equals the run's `--concurrency` value.
    pub max_sessions: usize,
    /// Total sessions ever created. Used by the test suite to
    /// check acquisition behavior without leaking the internal
    /// map.
    total_created: Mutex<u64>,
}

impl SessionPool {
    pub fn new(max_sessions: usize) -> Self {
        Self {
            sessions: Mutex::new(HashMap::with_capacity(max_sessions.min(64))),
            max_sessions,
            total_created: Mutex::new(0),
        }
    }

    /// Diagnostic: total number of sessions ever created during
    /// this run.
    pub fn total_created(&self) -> u64 {
        *self.total_created.lock().expect("pool total mutex")
    }

    /// Try to reserve a session for `item`. Returns `Some(handle)`
    /// when the caller now owns a session and should send the
    /// first turn. Returns `None` when the pool is at capacity
    /// (caller should treat this as `skipped_ticks`).
    pub fn acquire(&self, item: &DatasetItem) -> Option<PoolHandle<'_>> {
        if item.messages.is_none() {
            return None;
        }
        let seed = item.messages.clone().unwrap();
        let mut sessions = self.sessions.lock().expect("pool mutex");
        if sessions.len() < self.max_sessions {
            let id = Uuid::new_v4();
            let new = Session::new(item.name.clone().unwrap_or_else(|| "item".into()), seed);
            sessions.insert(id, new);
            *self.total_created.lock().expect("pool total mutex") += 1;
            Some(PoolHandle { pool: self, id })
        } else {
            None
        }
    }

    /// Acquire with LRU eviction. Use this when the executor
    /// wants to keep pushing work even when the pool is at
    /// capacity. Returns `Some(handle)` always; if the pool was
    /// full, the oldest idle session was destroyed to make room.
    pub fn acquire_evict_lru(&self, item: &DatasetItem) -> Option<PoolHandle<'_>> {
        if item.messages.is_none() {
            return None;
        }
        let seed = item.messages.clone().unwrap();
        let mut sessions = self.sessions.lock().expect("pool mutex");
        if sessions.len() < self.max_sessions {
            let id = Uuid::new_v4();
            let new = Session::new(item.name.clone().unwrap_or_else(|| "item".into()), seed);
            sessions.insert(id, new);
            *self.total_created.lock().expect("pool total mutex") += 1;
            return Some(PoolHandle { pool: self, id });
        }
        // Pool full — find LRU session and replace it.
        let lru = sessions
            .iter()
            .min_by_key(|(_, s)| s.last_used)
            .map(|(id, _)| *id);
        if let Some(id) = lru {
            let new = Session::new(item.name.clone().unwrap_or_else(|| "item".into()), seed);
            sessions.insert(id, new);
            *self.total_created.lock().expect("pool total mutex") += 1;
            Some(PoolHandle { pool: self, id })
        } else {
            None
        }
    }

    /// Read-only view of the current session list (under the lock).
    pub fn snapshot(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.lock().expect("pool mutex");
        sessions
            .iter()
            .map(|(id, s)| SessionInfo {
                id: id.to_string(),
                item_name: s.item_name.clone(),
                turn: s.turn,
                message_count: s.messages.len(),
            })
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub item_name: String,
    pub turn: u32,
    pub message_count: usize,
}

impl<'a> PoolHandle<'a> {
    fn lookup<'b>(&'b self, sessions: &'b HashMap<Uuid, Session>) -> &'b Session {
        sessions.get(&self.id).expect(
            "session handle is stale: the session was dropped or evicted. \
             This is a bug — PoolHandle methods must not be called after \
             drop_session() on the same handle.",
        )
    }

    /// Get a snapshot of the session's current `messages` for
    /// building the next request body. The session retains
    /// ownership; the caller borrows.
    pub fn messages(&self) -> Vec<OwnedChatMessage> {
        let sessions = self.pool.sessions.lock().expect("pool mutex");
        self.lookup(&sessions).messages.clone()
    }

    /// M9: read the session's stored `response_id` (from the
    /// last successful turn). `None` on the first turn or for
    /// clients that don't populate it.
    pub fn response_id(&self) -> Option<String> {
        let sessions = self.pool.sessions.lock().expect("pool mutex");
        self.lookup(&sessions).response_id.clone()
    }

    /// M9: record the `response_id` returned by a successful
    /// turn so the next turn can use it as
    /// `previous_response_id`. The executor calls this with the
    /// value from `RequestMetrics.response_id` after each turn.
    pub fn set_response_id(&self, id: Option<String>) {
        let mut sessions = self.pool.sessions.lock().expect("pool mutex");
        let s = sessions
            .get_mut(&self.id)
            .expect("session handle is stale: the session was dropped or evicted.");
        s.response_id = id;
    }

    /// Append `assistant` to the session's `messages`, bump the
    /// turn counter, and return whether the next turn should be
    /// sent. `follow_ups_remaining` is `true` if the item's
    /// `follow_ups` array still has entries after the one we
    /// just consumed.
    pub fn complete(&self, assistant: String, follow_ups_remaining: bool) -> TurnResult {
        let mut sessions = self.pool.sessions.lock().expect("pool mutex");
        let s = sessions
            .get_mut(&self.id)
            .expect("session handle is stale: the session was dropped or evicted.");
        let action = if s.complete_turn(assistant, follow_ups_remaining) {
            TurnAction::Continue
        } else {
            TurnAction::Done
        };
        let turns = s.turn;
        TurnResult {
            action,
            turns_completed: turns,
            dropped: false,
        }
    }

    /// Drop the session without recording a successful turn
    /// (used when a turn failed). The slot is freed.
    pub fn drop_session(&self) {
        let mut sessions = self.pool.sessions.lock().expect("pool mutex");
        sessions.remove(&self.id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item_with_follow_ups(name: &str, follow_ups: usize) -> DatasetItem {
        let mut msgs = vec![OwnedChatMessage::new("user", "hello")];
        for i in 0..follow_ups {
            msgs.push(OwnedChatMessage::new("user", format!("turn-{}", i + 2)));
        }
        DatasetItem {
            prompt: "[user] hello".into(),
            estimated_prompt_tokens: 1,
            weight: None,
            tags: Vec::new(),
            name: Some(name.into()),
            messages: Some(vec![OwnedChatMessage::new("user", "hello")]),
            follow_ups: (1..=follow_ups)
                .map(|i| format!("turn-{}", i + 1))
                .collect(),
        }
    }

    #[test]
    fn acquire_returns_handle_for_multi_turn_item() {
        let pool = SessionPool::new(4);
        let item = item_with_follow_ups("q1", 2);
        let h = pool.acquire(&item).expect("acquire");
        assert_eq!(h.messages().len(), 1);
    }

    #[test]
    fn acquire_returns_none_for_single_turn_item() {
        let pool = SessionPool::new(4);
        let item = DatasetItem {
            prompt: "x".into(),
            estimated_prompt_tokens: 1,
            weight: None,
            tags: Vec::new(),
            name: None,
            messages: None,
            follow_ups: Vec::new(),
        };
        assert!(pool.acquire(&item).is_none());
    }

    #[test]
    fn complete_turn_appends_assistant_and_returns_continue() {
        let pool = SessionPool::new(4);
        let item = item_with_follow_ups("q1", 2);
        let h = pool.acquire(&item).unwrap();
        // Turn 1: seed sent, response received. follow_ups_remaining
        // is true because we have 2 follow-ups and used 0 of them
        // for this turn.
        let r = h.complete("first answer".into(), true);
        assert_eq!(r.action, TurnAction::Continue);
        assert_eq!(r.turns_completed, 1);
        let msgs = h.messages();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1].role, "assistant");
        assert_eq!(msgs[1].content, "first answer");
    }

    #[test]
    fn complete_turn_returns_done_when_no_more_follow_ups() {
        let pool = SessionPool::new(4);
        let item = item_with_follow_ups("q1", 1);
        let h = pool.acquire(&item).unwrap();
        let r = h.complete("done".into(), false);
        assert_eq!(r.action, TurnAction::Done);
        assert_eq!(r.turns_completed, 1);
    }

    #[test]
    fn drop_session_releases_slot() {
        let pool = SessionPool::new(2);
        let i1 = item_with_follow_ups("a", 1);
        let i2 = item_with_follow_ups("b", 1);
        let i3 = item_with_follow_ups("c", 1);
        let h1 = pool.acquire(&i1).unwrap();
        let _h2 = pool.acquire(&i2).unwrap();
        // Pool full: third acquire should fail.
        assert!(pool.acquire(&i3).is_none());
        h1.drop_session();
        // Now a third session can fit.
        let h3 = pool.acquire(&i3).unwrap();
        assert_eq!(h3.messages().len(), 1);
    }

    #[test]
    fn pool_respects_max_sessions() {
        let pool = SessionPool::new(2);
        let i1 = item_with_follow_ups("a", 1);
        let i2 = item_with_follow_ups("b", 1);
        let i3 = item_with_follow_ups("c", 1);
        let _ = pool.acquire(&i1).unwrap();
        let _ = pool.acquire(&i2).unwrap();
        assert!(pool.acquire(&i3).is_none());
        assert_eq!(pool.total_created(), 2);
    }

    #[test]
    fn acquire_evict_lru_replaces_oldest_when_full() {
        let pool = SessionPool::new(1);
        let i1 = item_with_follow_ups("first", 1);
        let i2 = item_with_follow_ups("second", 1);
        let _h1 = pool.acquire(&i1).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(5));
        let h2 = pool.acquire_evict_lru(&i2).unwrap();
        assert_eq!(h2.messages().len(), 1);
        assert_eq!(pool.total_created(), 2);
    }
}
