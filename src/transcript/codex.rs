//! Incremental, cursor-based scanner over a Codex rollout file (JSONL).
//!
//! Explicit `$skill` invocations appear as `skill_instructions` response
//! items and never as tool calls, so this scanner — not the PostToolUse
//! recorder — is the only place they can be detected. Implicit loads are
//! shell calls whose arguments reference a SKILL.md path. The rollout format
//! is not documented as stable: unknown item kinds and unparseable lines are
//! skipped, and token/turn accounting degrades gracefully.

use crate::adapter::codex::skill_reads_in_text;
use crate::model::{Cursor, SkillLoad};
use crate::transcript::Scan;
use anyhow::Result;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

const TOKEN_POINTERS: [&str; 3] = [
    "/payload/info/last_token_usage/output_tokens",
    "/payload/last_token_usage/output_tokens",
    "/payload/output_tokens",
];

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
        if ty == "turn_context" {
            turn += 1;
        }
        for ptr in TOKEN_POINTERS {
            if let Some(out) = v.pointer(ptr).and_then(|t| t.as_u64()) {
                tokens += out;
                break;
            }
        }
        if ty != "response_item" {
            continue;
        }
        let at = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(|s| s.parse::<jiff::Timestamp>().ok())
            .unwrap_or_else(jiff::Timestamp::now);
        match v.pointer("/payload/type").and_then(|t| t.as_str()) {
            Some("skill_instructions") => {
                let name = v
                    .pointer("/payload/name")
                    .and_then(|s| s.as_str())
                    .map(str::to_string)
                    .or_else(|| {
                        v.pointer("/payload/path")
                            .and_then(|s| s.as_str())
                            .and_then(|p| skill_reads_in_text(p).pop())
                    });
                if let Some(skill) = name {
                    loads.push(SkillLoad {
                        skill,
                        at,
                        turn,
                        tokens,
                    });
                }
            }
            Some("function_call") => {
                let args = v
                    .pointer("/payload/arguments")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                for skill in skill_reads_in_text(args) {
                    loads.push(SkillLoad {
                        skill,
                        at,
                        turn,
                        tokens,
                    });
                }
            }
            _ => {}
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
    fn finds_explicit_and_implicit_loads_with_cursor() {
        let s = scan(
            Path::new("tests/fixtures/codex_rollout.jsonl"),
            Cursor::default(),
        )
        .unwrap();
        assert_eq!(s.skill_loads.len(), 2);
        assert_eq!(s.skill_loads[0].skill, "technical-writing");
        assert_eq!(s.skill_loads[0].turn, 1); // after the turn_context line
        assert_eq!(s.skill_loads[1].skill, "code-review");
        assert_eq!(s.end.turn, 1);
        assert_eq!(s.end.tokens, 120);
        assert_eq!(
            s.skill_loads[0].at,
            "2026-09-12T10:00:05Z".parse::<jiff::Timestamp>().unwrap()
        );
    }

    #[test]
    fn incremental_scan_returns_no_old_loads() {
        let first = scan(
            Path::new("tests/fixtures/codex_rollout.jsonl"),
            Cursor::default(),
        )
        .unwrap();
        let second = scan(Path::new("tests/fixtures/codex_rollout.jsonl"), first.end).unwrap();
        assert!(second.skill_loads.is_empty());
        assert_eq!(second.end, first.end);
    }

    #[test]
    fn malformed_and_unknown_lines_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("r.jsonl");
        std::fs::write(&p, "not json\n{\"timestamp\":\"2026-09-12T10:00:00Z\",\"type\":\"mystery_item\",\"payload\":{}}\n").unwrap();
        let s = scan(&p, Cursor::default()).unwrap();
        assert!(s.skill_loads.is_empty());
        assert_eq!(s.end.turn, 0);
    }
}
