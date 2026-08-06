//! ShareGPT-format JSON loader.
//!
//! ## Expected format
//!
//! ```json
//! [
//!   {
//!     "conversations": [
//!       {"from": "human", "value": "first user message"},
//!       {"from": "gpt", "value": "first assistant reply"},
//!       ...
//!     ]
//!   },
//!   ...
//! ]
//! ```
//!
//! For each conversation we use the **first human turn** as the prompt.
//! Conversations with no human turn are skipped. The file is loaded
//! entirely into memory; we cap at 1000 prompts to keep things sane.
//!
//! The loader tolerates the field name being `human` OR `user` (some
//! ShareGPT dumps use `user`).

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::DatasetItem;
use crate::error::{Error, Result};

/// Maximum number of prompts to keep. Loading megabytes of ShareGPT
/// dumps into a stress test is not the goal.
pub const MAX_PROMPTS: usize = 1000;

#[derive(Debug, Deserialize)]
struct ConversationFile(Vec<Conversation>);

#[derive(Debug, Deserialize)]
struct Conversation {
    #[serde(default)]
    conversations: Vec<Turn>,
}

#[derive(Debug, Deserialize)]
struct Turn {
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    value: Option<String>,
}

/// Load prompts from a ShareGPT file at `path`.
pub fn load(path: &Path) -> Result<Vec<DatasetItem>> {
    if !path.exists() {
        return Err(Error::InvalidConfig(format!(
            "sharegpt file not found: {}",
            path.display()
        )));
    }
    let text = fs::read_to_string(path).map_err(|e| Error::io_at(path, e))?;
    let parsed: ConversationFile = serde_json::from_str(&text).map_err(|e| {
        Error::InvalidConfig(format!(
            "sharegpt: failed to parse {}: {e}",
            path.display()
        ))
    })?;
    let mut out = Vec::with_capacity(parsed.0.len().min(MAX_PROMPTS));
    for conv in &parsed.0 {
        if out.len() >= MAX_PROMPTS {
            break;
        }
        if let Some(turn) = conv.conversations.iter().find(|t| is_human(t)) {
            if let Some(value) = &turn.value {
                let trimmed = value.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let tokens = crate::config::estimate_tokens(trimmed);
                out.push(DatasetItem {
                    prompt: trimmed.to_string(),
                    estimated_prompt_tokens: tokens,
                    weight: None,
                    tags: Vec::new(),
                    name: None,
                    messages: None,
                    follow_ups: Vec::new(),
                });
            }
        }
    }
    if out.is_empty() {
        return Err(Error::InvalidConfig(format!(
            "sharegpt: no human turns found in {}",
            path.display()
        )));
    }
    Ok(out)
}

fn is_human(t: &Turn) -> bool {
    matches!(
        t.from.as_deref().map(str::to_ascii_lowercase).as_deref(),
        Some("human") | Some("user")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_tmp(json: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(json.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parses_first_human_turn() {
        let body = r#"[
          {"conversations": [
            {"from": "human", "value": "What is the capital of France?"},
            {"from": "gpt", "value": "Paris."}
          ]},
          {"conversations": [
            {"from": "user", "value": "Translate hi to German."},
            {"from": "assistant", "value": "Hallo."}
          ]}
        ]"#;
        let f = write_tmp(body);
        let items = load(f.path()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].prompt, "What is the capital of France?");
        assert_eq!(items[1].prompt, "Translate hi to German.");
    }

    #[test]
    fn skips_conversations_with_no_human_turn() {
        let body = r#"[{"conversations": [{"from": "gpt", "value": "no human here"}]}]"#;
        let f = write_tmp(body);
        let err = load(f.path()).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn missing_file_errors() {
        let err = load(Path::new("/no/such/file.json")).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn malformed_json_errors() {
        let f = write_tmp("not json at all");
        let err = load(f.path()).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn caps_at_max_prompts() {
        // Build 1500 tiny conversations. We only want 1000.
        let mut s = String::from("[");
        for i in 0..1500 {
            if i > 0 {
                s.push(',');
            }
            s.push_str(&format!(
                r#"{{"conversations":[{{"from":"human","value":"q-{i}"}}]}}"#
            ));
        }
        s.push(']');
        let f = write_tmp(&s);
        let items = load(f.path()).unwrap();
        assert_eq!(items.len(), MAX_PROMPTS);
    }
}
