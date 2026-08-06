//! Custom dataset loader. Accepts any of:
//!
//! 1. A JSON array of objects: `[{"prompt": "..."}, ...]`
//! 2. A JSONL stream: one `{"prompt": "..."}` per line.
//! 3. A TOML profile: `[[prompt]]` entries with `text` and/or
//!    `messages`, optional `weight` / `tags` / `name` / `follow_ups`.
//!
//! Format is auto-detected by extension: `.toml` → TOML; otherwise
//! the first non-whitespace character decides JSON-array vs JSONL.
//!
//! After parsing, the loader walks the items once and detects the
//! file's [`DatasetMode`]: single, static_multi, or dynamic_multi.
//! Mixing modes in one file is rejected with a fail-fast
//! `Error::InvalidConfig` citing the offending item name + line.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use super::{DatasetItem, DatasetMode, OwnedChatMessage};
use crate::error::{Error, Result};

/// Backward-compatible entry point. Returns the items assuming
/// `Single` mode — used by tests and any code path that doesn't
/// care about multi-turn execution.
pub fn load(path: &Path) -> Result<Vec<DatasetItem>> {
    let (items, _mode) = load_with_mode(path)?;
    Ok(items)
}

/// Full entry point. Returns the items and the detected mode.
pub fn load_with_mode(path: &Path) -> Result<(Vec<DatasetItem>, DatasetMode)> {
    if !path.exists() {
        return Err(Error::InvalidConfig(format!(
            "custom dataset file not found: {}",
            path.display()
        )));
    }
    let text = fs::read_to_string(path).map_err(|e| Error::io_at(path, e))?;
    let items = if path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("toml"))
        .unwrap_or(false)
    {
        parse_toml_profile(&text, path)?
    } else {
        let first = text.chars().find(|c| !c.is_whitespace());
        if matches!(first, Some('[')) {
            parse_json_array(&text, path)?
        } else {
            parse_jsonl(&text, path)?
        }
    };
    if items.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "custom: no valid prompts parsed from {}",
            path.display()
        )));
    }
    let mode = detect_mode(&items, path)?;
    Ok((items, mode))
}

// ---------------------------------------------------------------------------
// JSON / JSONL (M2 formats — preserved 100% for backward compat)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct PromptEntryJson {
    prompt: String,
}

fn parse_json_array(text: &str, path: &Path) -> Result<Vec<DatasetItem>> {
    let entries: Vec<PromptEntryJson> = serde_json::from_str(text).map_err(|e| {
        Error::InvalidConfig(format!(
            "custom: failed to parse {} as JSON array: {e}",
            path.display()
        ))
    })?;
    Ok(entries
        .into_iter()
        .filter(|e| !e.prompt.trim().is_empty())
        .map(|e| {
            let tokens = crate::config::estimate_tokens(&e.prompt);
            DatasetItem {
                prompt: e.prompt,
                estimated_prompt_tokens: tokens,
                weight: None,
                tags: Vec::new(),
                name: None,
                messages: None,
                follow_ups: Vec::new(),
            }
        })
        .collect())
}

fn parse_jsonl(text: &str, path: &Path) -> Result<Vec<DatasetItem>> {
    let reader = BufReader::new(text.as_bytes());
    let mut out = Vec::new();
    for (lineno, line) in reader.lines().enumerate() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        match serde_json::from_str::<PromptEntryJson>(trimmed) {
            Ok(entry) if !entry.prompt.trim().is_empty() => {
                let tokens = crate::config::estimate_tokens(&entry.prompt);
                out.push(DatasetItem {
                    prompt: entry.prompt,
                    estimated_prompt_tokens: tokens,
                    weight: None,
                    tags: Vec::new(),
                    name: None,
                    messages: None,
                    follow_ups: Vec::new(),
                });
            }
            Ok(_) => {} // empty prompt — skip
            Err(e) => {
                tracing::warn!(
                    "custom: skipping line {} of {}: {e}",
                    lineno + 1,
                    path.display()
                );
            }
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "custom: no valid prompts parsed from {}",
            path.display()
        )));
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// TOML profile (M6)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct ProfileFile {
    /// The TOML profile table-array. Both `[[prompt]]` (the
    /// original M6 schema) and `[[items]]` (the user-facing
    /// name in `docs/examples/datasets/`) are accepted.
    /// `[[items]]` reads more naturally for multi-message
    /// conversation profiles where the entries are not
    /// really "prompts" in the literal sense.
    #[serde(default, alias = "items")]
    prompt: Vec<ProfileItem>,
}

#[derive(Debug, Deserialize)]
struct ProfileItem {
    /// Optional stable name. Defaults to a synthetic id.
    #[serde(default)]
    name: Option<String>,
    /// Single-turn shortcut. `text` and `messages` are mutually
    /// exclusive at validation time (see `validate_item`).
    #[serde(default)]
    text: Option<String>,
    /// Multi-turn seed.
    #[serde(default)]
    messages: Option<Vec<ProfileMessage>>,
    /// Per-turn follow-up user messages. Non-empty ⇒ dynamic multi-turn.
    #[serde(default)]
    follow_ups: Vec<String>,
    /// Optional sampling weight. `0` items are skipped.
    /// `f64` so users can write `0.5` / `1.0` / `2.5` in the
    /// TOML profile naturally.
    #[serde(default)]
    weight: Option<f64>,
    /// Optional tags.
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct ProfileMessage {
    role: String,
    content: String,
}

fn parse_toml_profile(text: &str, path: &Path) -> Result<Vec<DatasetItem>> {
    let file: ProfileFile = toml::from_str(text).map_err(|e| {
        Error::InvalidConfig(format!(
            "custom: failed to parse {} as TOML profile: {e}",
            path.display()
        ))
    })?;
    let mut out = Vec::new();
    for (i, raw) in file.prompt.into_iter().enumerate() {
        let name = raw.name.clone().unwrap_or_else(|| {
            // Position-based name so report rows stay readable
            // when the user didn't set one.
            format!("item-{i}")
        });
        if let Some(w) = raw.weight {
            if w == 0.0 {
                tracing::debug!("custom: skipping item {name} (weight=0)");
                continue;
            }
        }
        let item = build_item(raw, name.clone()).map_err(|e| match e {
            Error::InvalidConfig(msg) => Error::InvalidConfig(format!(
                "custom: {} item `{name}` in {}: {msg}",
                if i == 0 { "first" } else { "later" },
                path.display()
            )),
            other => other,
        })?;
        out.push(item);
    }
    Ok(out)
}

fn build_item(raw: ProfileItem, name: String) -> Result<DatasetItem> {
    match (raw.text, raw.messages) {
        (Some(text), None) => {
            // Single-turn.
            if !raw.follow_ups.is_empty() {
                return Err(Error::InvalidConfig(
                    "follow_ups is only valid on multi-turn items (with messages[])"
                        .into(),
                ));
            }
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return Err(Error::InvalidConfig("text is empty".into()));
            }
            let tokens = crate::config::estimate_tokens(trimmed);
            Ok(DatasetItem {
                prompt: trimmed.to_string(),
                estimated_prompt_tokens: tokens,
                weight: raw.weight,
                tags: raw.tags,
                name: Some(name),
                messages: None,
                follow_ups: Vec::new(),
            })
        }
        (None, Some(messages)) => {
            // Multi-turn (static or dynamic, decided by follow_ups).
            if messages.is_empty() {
                return Err(Error::InvalidConfig("messages[] is empty".into()));
            }
            let mut owned_msgs: Vec<OwnedChatMessage> = Vec::with_capacity(messages.len());
            for (mi, m) in messages.into_iter().enumerate() {
                let role = m.role.trim();
                if role.is_empty() {
                    return Err(Error::InvalidConfig(format!(
                        "messages[{mi}].role is empty"
                    )));
                }
                if !["system", "user", "assistant"].contains(&role) {
                    return Err(Error::InvalidConfig(format!(
                        "messages[{mi}].role `{role}` is not one of system/user/assistant"
                    )));
                }
                owned_msgs.push(OwnedChatMessage::new(role, m.content));
            }
            // prompt is the joined text for distribution / report
            // purposes only; not sent as a request.
            let joined: String = owned_msgs
                .iter()
                .map(|m| format!("[{}] {}", m.role, m.content))
                .collect::<Vec<_>>()
                .join("\n");
            let tokens = crate::config::estimate_tokens(&joined);
            for (fi, fu) in raw.follow_ups.iter().enumerate() {
                if fu.trim().is_empty() {
                    return Err(Error::InvalidConfig(format!(
                        "follow_ups[{fi}] is empty"
                    )));
                }
            }
            Ok(DatasetItem {
                prompt: joined,
                estimated_prompt_tokens: tokens,
                weight: raw.weight,
                tags: raw.tags,
                name: Some(name),
                messages: Some(owned_msgs),
                follow_ups: raw.follow_ups,
            })
        }
        (Some(_), Some(_)) => Err(Error::InvalidConfig(
            "an item cannot have both `text` and `messages[]`; pick one".into(),
        )),
        (None, None) => Err(Error::InvalidConfig(
            "an item must have either `text` or `messages[]`".into(),
        )),
    }
}

// ---------------------------------------------------------------------------
// Mode detection (fail-fast on mixed files)
// ---------------------------------------------------------------------------

fn detect_mode(items: &[DatasetItem], path: &Path) -> Result<DatasetMode> {
    // Per-item shape is already validated; we just need to make sure
    // every item agrees on the same mode.
    let mut mode: Option<DatasetMode> = None;
    for item in items {
        let item_mode = match (&item.messages, item.follow_ups.is_empty()) {
            (None, _) => DatasetMode::Single,
            (Some(_), true) => DatasetMode::StaticMulti,
            (Some(_), false) => DatasetMode::DynamicMulti,
        };
        match mode {
            None => mode = Some(item_mode),
            Some(prev) if prev == item_mode => {}
            Some(prev) => {
                let prev_name = format!("{prev:?}");
                let item_name = format!("{item_mode:?}");
                return Err(Error::InvalidConfig(format!(
                    "custom: dataset {} mixes modes ({} and {}). One file = one mode. \
                     Either all items are `text` (single), all have `messages[]` with no \
                     `follow_ups` (static_multi), or all have `messages[]` + `follow_ups` \
                     (dynamic_multi).",
                    path.display(),
                    prev_name,
                    item_name
                )));
            }
        }
    }
    Ok(mode.unwrap_or(DatasetMode::Single))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::path::PathBuf;
    use tempfile::TempDir;

    /// Write `body` to a fresh file inside a tempdir, using `name` so
    /// the extension-based format sniffer sees `.toml` / `.json` /
    /// `.jsonl`. The returned path lives until `dir` is dropped at
    /// the end of the test (TempDir is held in the test's stack).
    fn write_tmp(name: &str, body: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join(name);
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f.sync_all().ok();
        (dir, path)
    }

    // ----- JSON / JSONL backward compat -----

    #[test]
    fn parses_json_array() {
        let body = r#"[{"prompt": "a"}, {"prompt": "longer prompt here"}]"#;
        let (_d, p) = write_tmp("a.json", body);
        let items = load(&p).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].prompt, "a");
        assert_eq!(items[1].prompt, "longer prompt here");
        let (_, mode) = load_with_mode(&p).unwrap();
        assert_eq!(mode, DatasetMode::Single);
    }

    #[test]
    fn parses_jsonl() {
        let body = r#"{"prompt": "one"}
{"prompt": "two"}
{"prompt": "three"}"#;
        let (_d, p) = write_tmp("a.jsonl", body);
        let items = load(&p).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].prompt, "three");
    }

    #[test]
    fn jsonl_skips_blank_lines() {
        let body = "{\"prompt\":\"a\"}\n\n   \n{\"prompt\":\"b\"}\n";
        let (_d, p) = write_tmp("a.jsonl", body);
        let items = load(&p).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn jsonl_skips_bad_lines_with_warning() {
        let body = "{\"prompt\":\"a\"}\nnot json at all\n{\"prompt\":\"b\"}\n";
        let (_d, p) = write_tmp("a.jsonl", body);
        let items = load(&p).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn jsonl_all_bad_errors() {
        let body = "not json\nalso not json\n";
        let (_d, p) = write_tmp("a.jsonl", body);
        let err = load(&p).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn missing_file_errors() {
        let err = load(Path::new("/no/such/file.jsonl")).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn malformed_json_array_errors() {
        let body = "[{\"prompt\": \"x\""; // truncated
        let (_d, p) = write_tmp("a.json", body);
        let err = load(&p).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    // ----- TOML profile -----

    #[test]
    fn parses_toml_single_turn() {
        let body = r#"
[[prompt]]
text = "Hello"

[[prompt]]
name = "q2"
text = "World"
weight = 3
tags = ["smoke"]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let (items, mode) = load_with_mode(&p).unwrap();
        assert_eq!(mode, DatasetMode::Single);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].prompt, "Hello");
        assert_eq!(items[1].name.as_deref(), Some("q2"));
        assert_eq!(items[1].weight, Some(3.0));
        assert_eq!(items[1].tags, vec!["smoke".to_string()]);
    }

    #[test]
    fn parses_toml_static_multi_turn() {
        let body = r#"
[[prompt]]
name = "few-shot"
messages = [
  { role = "system", content = "Be terse." },
  { role = "user", content = "What is 2+2?" },
  { role = "assistant", content = "4." },
  { role = "user", content = "And 3+3?" },
]
tags = ["qa"]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let (items, mode) = load_with_mode(&p).unwrap();
        assert_eq!(mode, DatasetMode::StaticMulti);
        assert_eq!(items.len(), 1);
        let msgs = items[0].messages.as_ref().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0].role, "system");
        assert_eq!(msgs[3].role, "user");
        assert!(items[0].follow_ups.is_empty());
    }

    #[test]
    fn parses_toml_dynamic_multi_turn() {
        let body = r#"
[[prompt]]
name = "long-qa"
messages = [
  { role = "system", content = "You are an analyst." },
  { role = "user", content = "Summarize the Q3 report." },
]
follow_ups = [
  "Now compare to Q2.",
  "What were the top 3 risks?",
]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let (items, mode) = load_with_mode(&p).unwrap();
        assert_eq!(mode, DatasetMode::DynamicMulti);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].follow_ups.len(), 2);
        assert_eq!(items[0].follow_ups[0], "Now compare to Q2.");
    }

    /// M6h: the user-facing schema name in
    /// `docs/examples/datasets/multi-turn-conversation.toml`
    /// is `[[items]]` (more natural for multi-message
    /// conversation profiles). The parser must accept
    /// `[[items]]` as an alias for the original M6
    /// `[[prompt]]` table-array name.
    #[test]
    fn toml_items_alias_accepted() {
        let body = r#"
[[items]]
name = "via-items-alias"
messages = [
  { role = "user", content = "hi" },
]
follow_ups = [
  "follow up",
]
"#;
        let (_d, p) = write_tmp("b.toml", body);
        let (items, mode) = load_with_mode(&p).unwrap();
        assert_eq!(mode, DatasetMode::DynamicMulti);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name.as_deref(), Some("via-items-alias"));
        assert_eq!(items[0].follow_ups.len(), 1);
    }

    #[test]
    fn toml_mixed_modes_errors() {
        let body = r#"
[[prompt]]
text = "single"

[[prompt]]
messages = [{ role = "user", content = "multi" }]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let err = load_with_mode(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("mixes modes"), "got: {msg}");
    }

    #[test]
    fn toml_both_text_and_messages_errors() {
        let body = r#"
[[prompt]]
text = "single"
messages = [{ role = "user", content = "multi" }]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let err = load_with_mode(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("text") && msg.contains("messages"), "got: {msg}");
    }

    #[test]
    fn toml_neither_text_nor_messages_errors() {
        let body = r#"
[[prompt]]
name = "empty"
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let err = load_with_mode(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("text") && msg.contains("messages"), "got: {msg}");
    }

    #[test]
    fn toml_follow_ups_without_messages_errors() {
        let body = r#"
[[prompt]]
text = "single"
follow_ups = ["more"]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let err = load_with_mode(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("follow_ups"), "got: {msg}");
    }

    #[test]
    fn toml_invalid_role_errors() {
        let body = r#"
[[prompt]]
messages = [{ role = "admin", content = "go" }]
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let err = load_with_mode(&p).unwrap_err();
        let msg = format!("{err}");
        assert!(msg.contains("admin"), "got: {msg}");
    }

    #[test]
    fn toml_weight_zero_skipped() {
        let body = r#"
[[prompt]]
text = "kept"
weight = 1

[[prompt]]
text = "skipped"
weight = 0
"#;
        let (_d, p) = write_tmp("a.toml", body);
        let (items, _) = load_with_mode(&p).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].prompt, "kept");
    }

    #[test]
    fn toml_malformed_errors() {
        let body = "not valid toml = [[["; // unbalanced brackets
        let (_d, p) = write_tmp("a.toml", body);
        let err = load(&p).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }
}
