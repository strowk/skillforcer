//! Incremental, cursor-based scanner over a Claude session transcript (JSONL).
//!
//! `scan` reads the file starting at `from.offset`, one line at a time, and
//! returns any `Skill` tool-use loads found plus a `Cursor` for the new
//! position. Re-scanning from the returned cursor picks up only new lines,
//! so callers can poll a growing transcript file cheaply.
//!
//! Token accounting: `tokens` is the running sum of `message.usage.output_tokens`
//! across all assistant lines seen so far (cumulative, monotonically
//! increasing). It intentionally ignores `input_tokens`, which Anthropic's
//! API reports per-request rather than per-turn and would double-count
//! cached context across turns.

use crate::model::{Cursor, SkillLoad};
use anyhow::Result;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

/// Result of scanning a transcript from a given cursor: newly found skill
/// loads plus the cursor to resume from on the next scan.
pub struct Scan {
    pub skill_loads: Vec<SkillLoad>,
    pub end: Cursor,
}

/// Scans `path` starting at `from.offset`, returning skill loads discovered
/// and the cursor reflecting the new end-of-file position.
///
/// Lines that fail to parse as JSON are skipped (fail-open) rather than
/// treated as an error, since transcripts may be written concurrently.
pub fn scan(path: &Path, from: Cursor) -> Result<Scan> {
    let mut file = std::fs::File::open(path)?;
    file.seek(SeekFrom::Start(from.offset))?;
    let mut reader = BufReader::new(file);

    let mut turn = from.turn;
    let mut tokens = from.tokens;
    let mut offset = from.offset;
    let mut loads = Vec::new();

    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            break;
        }
        offset += n as u64;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue; // skip malformed lines (fail open)
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        if ty == "user" || ty == "assistant" {
            turn += 1;
        }
        if let Some(out) = v
            .pointer("/message/usage/output_tokens")
            .and_then(|t| t.as_u64())
        {
            tokens += out;
        }
        if ty == "assistant"
            && let Some(items) = v.pointer("/message/content").and_then(|c| c.as_array())
        {
            for item in items {
                let is_skill = item.get("type").and_then(|t| t.as_str()) == Some("tool_use")
                    && item.get("name").and_then(|t| t.as_str()) == Some("Skill");
                if is_skill
                    && let Some(skill) = item.pointer("/input/skill").and_then(|s| s.as_str())
                {
                    let at = v
                        .get("timestamp")
                        .and_then(|t| t.as_str())
                        .and_then(|s| s.parse::<jiff::Timestamp>().ok())
                        .unwrap_or_else(jiff::Timestamp::now);
                    loads.push(SkillLoad {
                        skill: skill.to_string(),
                        at,
                        turn,
                        tokens,
                    });
                }
            }
        }
    }

    Ok(Scan {
        skill_loads: loads,
        end: Cursor {
            offset,
            turn,
            tokens,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cursor;
    use std::path::Path;

    #[test]
    fn finds_skill_load_with_turn_and_tokens() {
        let scan = scan(
            Path::new("tests/fixtures/transcript.jsonl"),
            Cursor::default(),
        )
        .unwrap();
        assert_eq!(scan.skill_loads.len(), 1);
        let s = &scan.skill_loads[0];
        assert_eq!(s.skill, "tech-writing");
        assert_eq!(s.turn, 2); // user(1) then assistant(2)
        assert_eq!(s.tokens, 50);
        assert_eq!(scan.end.turn, 4);
        assert_eq!(scan.end.tokens, 80);
    }

    #[test]
    fn incremental_from_cursor_returns_no_old_loads() {
        let first = scan(
            Path::new("tests/fixtures/transcript.jsonl"),
            Cursor::default(),
        )
        .unwrap();
        let second = scan(Path::new("tests/fixtures/transcript.jsonl"), first.end).unwrap();
        assert!(second.skill_loads.is_empty());
        assert_eq!(second.end.turn, 4);
    }
}
