//! Token-usage detection from Claude Code transcript files.
//!
//! Claude Code writes a JSONL transcript per run at
//! `~/.claude/projects/<encoded-cwd>/<session-id>.jsonl`, where the project dir
//! name is the absolute working directory with every non-alphanumeric character
//! replaced by `-`. Each `{"type":"assistant"}` line carries a `message.usage`
//! object; the latest one tells us how full the context window currently is.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use serde_json::Value;

/// Standard Claude context window. Sessions using the 1M-context beta cannot be
/// distinguished from the transcript, so their percentage may exceed 100%.
pub const CONTEXT_LIMIT: u64 = 200_000;

/// How many bytes to read from the tail of a transcript. Transcripts can reach
/// megabytes, but we only need the last assistant turn, which lives near the end.
const TAIL_BYTES: u64 = 128 * 1024;

/// Token usage read from a Claude Code transcript.
#[derive(Debug, Clone)]
pub struct TokenUsage {
    /// Prompt tokens of the latest assistant turn = current context occupancy.
    pub context_tokens: u64,
    /// Model id from the latest assistant turn, if present.
    pub model: Option<String>,
}

impl TokenUsage {
    /// Context occupancy as a percentage of `CONTEXT_LIMIT` (may exceed 100).
    pub fn percent(&self) -> u32 {
        ((self.context_tokens as f64 / CONTEXT_LIMIT as f64) * 100.0).round() as u32
    }
}

/// Detect token usage for a session's working directory.
///
/// Locates the project dir under `~/.claude/projects`, picks the most recently
/// modified `.jsonl` transcript (the active run), and reads the last assistant
/// usage from its tail. Returns `None` when no transcript or usage is available.
///
/// Heuristic notes: when a project dir holds several runs we use the newest by
/// mtime. Two Claude panes sharing one cwd (same repo, no worktree) map to the
/// same dir and therefore report the same figure; worktrees have distinct cwds
/// and are unaffected.
pub fn detect(working_directory: &Path) -> Option<TokenUsage> {
    let home = dirs::home_dir()?;
    let encoded = encode_project_dir(working_directory)?;
    let dir = home.join(".claude/projects").join(encoded);

    let newest = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|n| n.ends_with(".jsonl"))
        })
        .filter_map(|e| {
            let mtime = e.metadata().ok()?.modified().ok()?;
            Some((mtime, e.path()))
        })
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, path)| path)?;

    let tail = read_tail(&newest, TAIL_BYTES)?;
    parse_last_usage(&tail)
}

/// Encode an absolute path the way Claude Code names its project dirs: every
/// non-alphanumeric character becomes `-` (e.g. `/workspace/claude-tmux` ->
/// `-workspace-claude-tmux`).
fn encode_project_dir(path: &Path) -> Option<String> {
    let s = path.to_str()?;
    Some(
        s.chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect(),
    )
}

/// Read up to `max_bytes` from the end of a file as lossy UTF-8. A mid-line seek
/// may split the first returned line; callers must tolerate an unparseable lead.
fn read_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = std::fs::File::open(path).ok()?;
    let len = file.metadata().ok()?.len();
    let start = len.saturating_sub(max_bytes);
    file.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf).ok()?;
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// Parse the last assistant `message.usage` from transcript text. Iterates lines
/// in reverse and skips any that fail to parse (including a truncated lead line).
fn parse_last_usage(content: &str) -> Option<TokenUsage> {
    for line in content.lines().rev() {
        let Ok(value) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if value.get("type").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        let message = value.get("message")?;
        let usage = match message.get("usage") {
            Some(u) if u.is_object() => u,
            _ => continue,
        };

        let field = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0);
        let context_tokens = field("input_tokens")
            + field("cache_read_input_tokens")
            + field("cache_creation_input_tokens");

        let model = message
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string);

        return Some(TokenUsage {
            context_tokens,
            model,
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn encodes_project_dir() {
        assert_eq!(
            encode_project_dir(&PathBuf::from("/workspace/claude-tmux")).unwrap(),
            "-workspace-claude-tmux"
        );
        assert_eq!(
            encode_project_dir(&PathBuf::from("/home/mailu/.dotfiles/nvim")).unwrap(),
            "-home-mailu--dotfiles-nvim"
        );
    }

    #[test]
    fn parses_last_assistant_usage() {
        let content = concat!(
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":10,"cache_read_input_tokens":20,"cache_creation_input_tokens":5,"output_tokens":100}}}"#,
            "\n",
            r#"{"type":"user","message":{"content":"hi"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"claude-opus-4-8","usage":{"input_tokens":131,"cache_read_input_tokens":66930,"cache_creation_input_tokens":514,"output_tokens":147}}}"#,
            "\n",
        );
        let usage = parse_last_usage(content).unwrap();
        // Last turn: 131 + 66930 + 514
        assert_eq!(usage.context_tokens, 67575);
        assert_eq!(usage.model.as_deref(), Some("claude-opus-4-8"));
    }

    #[test]
    fn ignores_truncated_lead_line() {
        // A mid-line seek can leave a broken first line; it must be skipped.
        let content = concat!(
            r#"tokens":123,"output_tokens":9}}}"#,
            "\n",
            r#"{"type":"assistant","message":{"model":"m","usage":{"input_tokens":1,"cache_read_input_tokens":2,"cache_creation_input_tokens":3}}}"#,
            "\n",
        );
        let usage = parse_last_usage(content).unwrap();
        assert_eq!(usage.context_tokens, 6);
        assert_eq!(usage.model.as_deref(), Some("m"));
    }

    #[test]
    fn returns_none_without_usage() {
        let content = concat!(
            r#"{"type":"user","message":{"content":"hello"}}"#,
            "\n",
            r#"{"type":"summary","summary":"x"}"#,
            "\n",
        );
        assert!(parse_last_usage(content).is_none());
    }
}
