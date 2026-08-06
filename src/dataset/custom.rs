//! Custom dataset loader. Accepts either:
//!
//! 1. A JSON array of objects: `[{"prompt": "..."}, ...]`
//! 2. A JSONL stream: one `{"prompt": "..."}` per line.
//!
//! Format is auto-detected: if the file's first non-whitespace character is
//! `[`, it is parsed as a JSON array; otherwise each line is parsed
//! independently as JSON. This is simple and matches what the spec asked
//! for ("auto-detect by file extension or sniff the first char").
//!
//! Lines that fail to parse are silently skipped (we log a warning), so a
//! half-broken file doesn't kill the whole run.

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde::Deserialize;

use super::DatasetItem;
use crate::error::{Error, Result};

#[derive(Debug, Deserialize)]
struct PromptEntry {
    prompt: String,
}

/// Load prompts from a custom dataset file at `path`.
pub fn load(path: &Path) -> Result<Vec<DatasetItem>> {
    if !path.exists() {
        return Err(Error::InvalidConfig(format!(
            "custom dataset file not found: {}",
            path.display()
        )));
    }
    let text = fs::read_to_string(path).map_err(|e| Error::io_at(path, e))?;
    let first = text.chars().find(|c| !c.is_whitespace());
    let is_json_array = matches!(first, Some('['));

    if is_json_array {
        parse_json_array(&text, path)
    } else {
        parse_jsonl(&text, path)
    }
}

fn parse_json_array(text: &str, path: &Path) -> Result<Vec<DatasetItem>> {
    let entries: Vec<PromptEntry> = serde_json::from_str(text).map_err(|e| {
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
        match serde_json::from_str::<PromptEntry>(trimmed) {
            Ok(entry) if !entry.prompt.trim().is_empty() => {
                let tokens = crate::config::estimate_tokens(&entry.prompt);
                out.push(DatasetItem {
                    prompt: entry.prompt,
                    estimated_prompt_tokens: tokens,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn write_tmp(body: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(body.as_bytes()).unwrap();
        f
    }

    #[test]
    fn parses_json_array() {
        let body = r#"[{"prompt": "a"}, {"prompt": "longer prompt here"}]"#;
        let f = write_tmp(body);
        let items = load(f.path()).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].prompt, "a");
        assert_eq!(items[1].prompt, "longer prompt here");
    }

    #[test]
    fn parses_jsonl() {
        let body = r#"{"prompt": "one"}
{"prompt": "two"}
{"prompt": "three"}"#;
        let f = write_tmp(body);
        let items = load(f.path()).unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[2].prompt, "three");
    }

    #[test]
    fn jsonl_skips_blank_lines() {
        let body = "{\"prompt\":\"a\"}\n\n   \n{\"prompt\":\"b\"}\n";
        let f = write_tmp(body);
        let items = load(f.path()).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn jsonl_skips_bad_lines_with_warning() {
        let body = "{\"prompt\":\"a\"}\nnot json at all\n{\"prompt\":\"b\"}\n";
        let f = write_tmp(body);
        let items = load(f.path()).unwrap();
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn jsonl_all_bad_errors() {
        let body = "not json\nalso not json\n";
        let f = write_tmp(body);
        let err = load(f.path()).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn missing_file_errors() {
        let err = load(Path::new("/no/such/file.jsonl")).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }

    #[test]
    fn malformed_json_array_errors() {
        let f = write_tmp("[{\"prompt\": \"x\""); // truncated
        let err = load(f.path()).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(_)));
    }
}
