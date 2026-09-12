# Codex CLI Support Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Full-parity skill-freshness enforcement for OpenAI Codex CLI: deny governed writes via Codex's PreToolUse hook until the required skill has loaded recently enough, plus publishing the `skillforcer-config` skill for Codex users.

**Architecture:** The existing agent-agnostic core (rules, freshness, config, state) is untouched except for suffix skill-matching and multi-event evaluation. A new `adapter/codex.rs` parses Codex hook stdin (same field names as Claude's) and extracts `WriteEvent`s from `apply_patch` envelopes; a new `transcript/codex.rs` scans Codex rollout JSONL for explicit (`SkillInstructions`) and implicit (shell reads of `SKILL.md`) skill loads plus turn/token cursors. The installer gains harness detection and a `.codex/hooks.json` target.

**Tech Stack:** Rust (edition per existing `Cargo.toml`), existing deps only: `argh`, `serde`/`serde_json`, `toml`, `regex`, `globset`, `directories`, `jiff`, `anyhow`. **No new dependencies.**

**Spec:** `docs/superpowers/specs/2026-09-12-codex-support-design.md`

## Global Constraints

- **No new crate dependencies.** Everything needed is already in `Cargo.toml`.
- **Fail-open everywhere:** an internal skillforcer error must never produce `Decision::Deny`; unparseable input → `Allow` + stderr note (matches existing style in `commands.rs`).
- **No live Codex capture.** All fixtures are hand-authored from the documented formats (spec §3, §11). Do not install Codex CLI or fetch rollouts.
- Before every commit: `cargo fmt` then `cargo test` — all tests green.
- Dev machine is Windows: in tests that embed paths in JSON, escape backslashes as in existing tests (`.replace('\\', "\\\\")`).
- Follow existing code style: no doc comments on test fns, `anyhow::Result`, tolerant `serde_json::Value` navigation with `.pointer()`/`.get()`.
- Deny JSON shape is identical for both harnesses: `{"hookSpecificOutput":{"hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":"..."}}` — reuse `adapter::claude::render_decision` for Codex; do not duplicate it.

---

### Task 1: Core primitives — `Harness` enum and suffix skill matching

**Files:**
- Modify: `src/model.rs` (add `Harness` enum at end of file)
- Modify: `src/freshness.rs` (add `skill_matches`, use it in `latest`)

**Interfaces:**
- Consumes: existing `SkillLoad` (`src/model.rs:10`).
- Produces: `model::Harness` — `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum Harness { Claude, Codex }` with `impl Harness { pub fn from_flag(s: &str) -> Harness }` (returns `Codex` for `"codex"` case-insensitive, else `Claude`). `freshness::skill_matches(rule_skill: &str, load_skill: &str) -> bool` — public, used by later tasks' tests.

- [ ] **Step 1: Write the failing tests** (in `src/freshness.rs` `mod tests`; enum test in `src/model.rs` new `mod tests`)

```rust
// src/freshness.rs tests
#[test]
fn suffix_match_across_harnesses() {
    assert!(skill_matches("tech-writing:technical-writing", "technical-writing"));
    assert!(skill_matches("technical-writing", "tech-writing:technical-writing"));
    assert!(skill_matches("a/b/technical-writing", "technical-writing"));
    assert!(skill_matches("x:same", "y/same"));
    assert!(!skill_matches("technical-writing", "other-skill"));
    assert!(!skill_matches("a:b", "a:c"));
}

#[test]
fn evaluate_matches_codex_load_by_suffix() {
    let req = Requires {
        skills: SkillSet::Any(vec!["tech-writing:technical-writing".into()]),
        windows: Windows { session: true, ..Default::default() },
    };
    // Codex records the bare directory name
    let loads = vec![load("technical-writing", "2026-09-12T10:00:00Z", 1, 100)];
    let r = evaluate(&req, &loads, &now("2026-09-12T10:05:00Z", 3, 150), Combine::All);
    assert!(r.satisfied);
}
```

```rust
// src/model.rs tests
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn harness_from_flag() {
        assert_eq!(Harness::from_flag("codex"), Harness::Codex);
        assert_eq!(Harness::from_flag("Codex"), Harness::Codex);
        assert_eq!(Harness::from_flag("claude"), Harness::Claude);
        assert_eq!(Harness::from_flag("anything-else"), Harness::Claude); // fail-open default
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test suffix_match harness_from_flag evaluate_matches_codex`
Expected: compile error — `skill_matches` and `Harness` not defined.

- [ ] **Step 3: Implement**

```rust
// src/model.rs — append
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn from_flag(s: &str) -> Harness {
        if s.eq_ignore_ascii_case("codex") {
            Harness::Codex
        } else {
            Harness::Claude
        }
    }
}
```

```rust
// src/freshness.rs — add near the top; then change `latest`'s filter
/// A rule's skill name matches a recorded load if they are equal or share the
/// same final segment (after the last ':' or '/'), so one rule spans both
/// Claude ("plugin:skill") and Codex (bare directory name) spellings.
pub fn skill_matches(rule_skill: &str, load_skill: &str) -> bool {
    if rule_skill == load_skill {
        return true;
    }
    fn tail(s: &str) -> &str {
        s.rsplit([':', '/']).next().unwrap_or(s)
    }
    tail(rule_skill) == tail(load_skill)
}
```

In `latest` (src/freshness.rs:9), change `.filter(|l| l.skill == skill)` to `.filter(|l| skill_matches(skill, &l.skill))`.

- [ ] **Step 4: Run the full test suite**

Run: `cargo test`
Expected: all PASS (existing freshness tests use exact names, which still match).

- [ ] **Step 5: Commit**

```bash
git add src/model.rs src/freshness.rs
git commit -m "feat: Harness enum and cross-harness suffix skill matching"
```

---

### Task 2: `apply_patch` envelope parser

**Files:**
- Create: `src/adapter/codex.rs`
- Modify: `src/adapter/mod.rs` (add `pub mod codex;`)

**Interfaces:**
- Consumes: `model::WriteEvent` (`src/model.rs:4`).
- Produces: `adapter::codex::parse_apply_patch(text: &str) -> Vec<WriteEvent>` — extracts one event per `*** Add File:` / `*** Update File:` section; `content` is the added lines (leading `+` stripped) joined with `\n`. Returns `Vec::new()` when no `*** Begin Patch` envelope is present.

- [ ] **Step 1: Write the failing tests** (in `src/adapter/codex.rs`, `#[cfg(test)] mod patch_tests`)

```rust
use super::*;

#[test]
fn add_file_collects_all_plus_lines() {
    let p = "*** Begin Patch\n*** Add File: src/new.rs\n+// hello\n+fn main() {}\n*** End Patch";
    let evs = parse_apply_patch(p);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].path.to_str().unwrap(), "src/new.rs");
    assert_eq!(evs[0].content, "// hello\nfn main() {}");
}

#[test]
fn update_file_collects_only_added_lines() {
    let p = "*** Begin Patch\n*** Update File: src/lib.rs\n@@ fn old\n context line\n-removed line\n+// added comment\n+added code\n*** End Patch";
    let evs = parse_apply_patch(p);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].content, "// added comment\nadded code");
}

#[test]
fn multi_file_patch_yields_multiple_events() {
    let p = "*** Begin Patch\n*** Update File: a.rs\n+// a\n*** Add File: b.md\n+# b\n*** End Patch";
    let evs = parse_apply_patch(p);
    assert_eq!(evs.len(), 2);
    assert_eq!(evs[0].path.to_str().unwrap(), "a.rs");
    assert_eq!(evs[1].path.to_str().unwrap(), "b.md");
    assert_eq!(evs[1].content, "# b");
}

#[test]
fn move_to_keeps_update_section_open() {
    let p = "*** Begin Patch\n*** Update File: old.rs\n*** Move to: new.rs\n+// moved\n*** End Patch";
    let evs = parse_apply_patch(p);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].path.to_str().unwrap(), "old.rs");
    assert_eq!(evs[0].content, "// moved");
}

#[test]
fn delete_only_and_no_envelope_yield_nothing() {
    assert!(parse_apply_patch("*** Begin Patch\n*** Delete File: gone.rs\n*** End Patch").is_empty());
    assert!(parse_apply_patch("echo hello").is_empty());
}

#[test]
fn envelope_embedded_in_shell_command_is_found() {
    let p = "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: c.ts\n+// ts\n*** End Patch\nEOF";
    let evs = parse_apply_patch(p);
    assert_eq!(evs.len(), 1);
    assert_eq!(evs[0].path.to_str().unwrap(), "c.ts");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test patch_tests`
Expected: compile error — module/function not defined.

- [ ] **Step 3: Implement** (`src/adapter/codex.rs`; add `pub mod codex;` to `src/adapter/mod.rs`)

```rust
//! Codex CLI adapter: apply_patch envelope parsing, write-event extraction,
//! and SKILL.md read detection. Hook stdin parsing and deny rendering are
//! shared with the Claude adapter — Codex uses the same field names and the
//! same `hookSpecificOutput` deny JSON.

use crate::model::WriteEvent;
use std::path::PathBuf;

/// Extracts one WriteEvent per file section from an apply_patch envelope
/// anywhere inside `text`. Content is the added lines (leading '+' stripped).
pub fn parse_apply_patch(text: &str) -> Vec<WriteEvent> {
    let Some(start) = text.find("*** Begin Patch") else {
        return Vec::new();
    };
    let body = &text[start..];
    let body = body.split("*** End Patch").next().unwrap_or(body);

    let mut events: Vec<WriteEvent> = Vec::new();
    let mut path: Option<String> = None;
    let mut added: Vec<String> = Vec::new();

    fn flush(path: &mut Option<String>, added: &mut Vec<String>, events: &mut Vec<WriteEvent>) {
        if let Some(p) = path.take() {
            events.push(WriteEvent {
                path: PathBuf::from(p),
                content: std::mem::take(added).join("\n"),
            });
        }
    }

    for line in body.lines() {
        if let Some(p) = line
            .strip_prefix("*** Add File:")
            .or_else(|| line.strip_prefix("*** Update File:"))
        {
            flush(&mut path, &mut added, &mut events);
            path = Some(p.trim().to_string());
        } else if line.starts_with("*** Move to:") {
            // rename of the current Update section — keep collecting under the old path
        } else if line.starts_with("*** ") {
            // Begin Patch header, Delete File, or any future marker: closes the section
            flush(&mut path, &mut added, &mut events);
        } else if let Some(a) = line.strip_prefix('+') {
            if path.is_some() {
                added.push(a.to_string());
            }
        }
    }
    flush(&mut path, &mut added, &mut events);
    events
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test patch_tests`
Expected: 6 PASS.

- [ ] **Step 5: Commit**

```bash
git add src/adapter/codex.rs src/adapter/mod.rs
git commit -m "feat: apply_patch envelope parser for Codex adapter"
```

---

### Task 3: Codex write-event extraction and SKILL.md read detection

**Files:**
- Modify: `src/adapter/codex.rs`
- Create: `tests/fixtures/codex_pre_apply_patch.json`
- Create: `tests/fixtures/codex_pre_shell_patch.json`

**Interfaces:**
- Consumes: `parse_apply_patch` (Task 2), `adapter::claude::write_event_from_tool` (`src/adapter/claude.rs:75`), `adapter::claude::parse_hook_input` (reused as-is for Codex stdin — same field names).
- Produces:
  - `adapter::codex::write_events_from_tool(tool_name: &str, input: &serde_json::Value) -> Vec<WriteEvent>`
  - `adapter::codex::command_text(input: &serde_json::Value) -> String`
  - `adapter::codex::skill_reads_in_text(text: &str) -> Vec<String>` — deduped skill names (parent directory of each `SKILL.md` path found).

- [ ] **Step 1: Create fixtures**

`tests/fixtures/codex_pre_apply_patch.json`:

```json
{"hook_event_name":"PreToolUse","session_id":"cdx-1","transcript_path":"/tmp/rollout.jsonl","cwd":"/proj","tool_name":"apply_patch","tool_input":{"input":"*** Begin Patch\n*** Update File: src/auth.rs\n+// refresh token\n*** End Patch"},"turn_id":"t1","permission_mode":"default"}
```

`tests/fixtures/codex_pre_shell_patch.json`:

```json
{"hook_event_name":"PreToolUse","session_id":"cdx-1","transcript_path":"/tmp/rollout.jsonl","cwd":"/proj","tool_name":"Bash","tool_input":{"command":["bash","-lc","apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: docs/x.md\n+# Heading\n*** End Patch\nEOF"]}}
```

- [ ] **Step 2: Write the failing tests** (append `mod extract_tests` to `src/adapter/codex.rs`)

```rust
#[cfg(test)]
mod extract_tests {
    use super::*;
    use crate::adapter::claude::{parse_hook_input, HookInput};

    #[test]
    fn apply_patch_tool_input_extracts_events() {
        let raw = include_str!("../../tests/fixtures/codex_pre_apply_patch.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else { panic!() };
        assert_eq!(p.tool_name, "apply_patch");
        let evs = write_events_from_tool(&p.tool_name, &p.tool_input);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].path.to_str().unwrap(), "src/auth.rs");
        assert_eq!(evs[0].content, "// refresh token");
    }

    #[test]
    fn shell_embedded_apply_patch_extracts_events() {
        let raw = include_str!("../../tests/fixtures/codex_pre_shell_patch.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else { panic!() };
        let evs = write_events_from_tool(&p.tool_name, &p.tool_input);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].path.to_str().unwrap(), "docs/x.md");
        assert_eq!(evs[0].content, "# Heading");
    }

    #[test]
    fn plain_shell_command_yields_no_events() {
        let input = serde_json::json!({"command": "cargo test"});
        assert!(write_events_from_tool("Bash", &input).is_empty());
    }

    #[test]
    fn claude_style_write_tool_still_extracts() {
        let input = serde_json::json!({"file_path": "a.rs", "content": "// hi"});
        let evs = write_events_from_tool("Write", &input);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].content, "// hi");
    }

    #[test]
    fn skill_reads_found_and_deduped() {
        let cmd = "cat /home/u/.agents/skills/technical-writing/SKILL.md && cat .agents/skills/technical-writing/SKILL.md";
        assert_eq!(skill_reads_in_text(cmd), vec!["technical-writing".to_string()]);
    }

    #[test]
    fn windows_style_skill_path_detected() {
        let cmd = r"type C:\Users\u\.agents\skills\tech-writing\SKILL.md";
        assert_eq!(skill_reads_in_text(cmd), vec!["tech-writing".to_string()]);
    }

    #[test]
    fn no_skill_reads_in_ordinary_command() {
        assert!(skill_reads_in_text("cargo build --release").is_empty());
    }
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test extract_tests`
Expected: compile error — functions not defined.

- [ ] **Step 4: Implement** (append to `src/adapter/codex.rs`)

```rust
use crate::adapter::claude;

/// Best-effort extraction of the shell command string from a tool input.
pub fn command_text(input: &serde_json::Value) -> String {
    match input.get("command") {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        _ => input.as_str().map(str::to_string).unwrap_or_default(),
    }
}

/// Concatenation of every string leaf in the value — the apply_patch input
/// shape is version-gated, so we look for the envelope anywhere.
fn all_strings(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().map(all_strings).collect::<Vec<_>>().join("\n"),
        serde_json::Value::Object(o) => o.values().map(all_strings).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

pub fn write_events_from_tool(tool_name: &str, input: &serde_json::Value) -> Vec<WriteEvent> {
    // Codex matchers also list Edit/Write; those payloads mirror Claude's.
    if let Some(ev) = claude::write_event_from_tool(tool_name, input) {
        return vec![ev];
    }
    let text = match tool_name {
        "apply_patch" => all_strings(input),
        "Bash" | "shell" | "local_shell" | "exec_command" => command_text(input),
        _ => return Vec::new(),
    };
    parse_apply_patch(&text)
}

/// Skill names implied by SKILL.md paths referenced in `text` (parent
/// directory name), deduped in order of first appearance. This mirrors the
/// heuristic Codex itself uses to detect implicit skill invocation.
pub fn skill_reads_in_text(text: &str) -> Vec<String> {
    let re = regex::Regex::new(r"[A-Za-z0-9_@.~:\-/\\]+[/\\]SKILL\.md").expect("static regex");
    let mut out: Vec<String> = Vec::new();
    for m in re.find_iter(text) {
        let normalized = m.as_str().replace('\\', "/");
        let mut parts = normalized.rsplit('/');
        parts.next(); // SKILL.md
        if let Some(name) = parts.next()
            && !name.is_empty()
            && !out.iter().any(|n| n == name)
        {
            out.push(name.to_string());
        }
    }
    out
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test extract_tests`
Expected: 7 PASS.

- [ ] **Step 6: Commit**

```bash
git add src/adapter/codex.rs tests/fixtures/codex_pre_apply_patch.json tests/fixtures/codex_pre_shell_patch.json
git commit -m "feat: Codex write-event extraction and SKILL.md read detection"
```

---

### Task 4: Codex rollout scanner

**Files:**
- Rename: `src/transcript.rs` → `src/transcript/mod.rs` (content unchanged; use `git mv`)
- Create: `src/transcript/codex.rs`
- Create: `tests/fixtures/codex_rollout.jsonl`

**Interfaces:**
- Consumes: `transcript::Scan`, `model::{Cursor, SkillLoad}`, `adapter::codex::skill_reads_in_text` (Task 3).
- Produces: `transcript::codex::scan(path: &Path, from: Cursor) -> anyhow::Result<Scan>` — same contract as `transcript::scan`: incremental from `from.offset`, returns new loads + end cursor.

**Format note (hand-authored per spec §3/§11):** each rollout line is `{"timestamp": "...", "type": "<kind>", "payload": {...}}`. Kinds handled: `turn_context` (turn boundary), `response_item` with `payload.type == "skill_instructions"` (explicit `$skill` load; `payload.name` or `payload.path`) or `payload.type == "function_call"` (shell call; `payload.arguments` is a JSON-encoded string searched for SKILL.md paths), token counts via pointer fallbacks (`/payload/info/last_token_usage/output_tokens`, `/payload/last_token_usage/output_tokens`, `/payload/output_tokens`). Everything else is skipped. The parser must be tolerant: unknown `type` → skip; unparseable line → skip.

- [ ] **Step 1: Move the module and re-wire**

```bash
git mv src/transcript.rs src/transcript/mod.rs
```

Add at the top of `src/transcript/mod.rs` (after the module doc comment):

```rust
pub mod codex;
```

Create an empty `src/transcript/codex.rs` placeholder so it compiles:

```rust
//! Incremental, cursor-based scanner over a Codex rollout file (JSONL).
```

Run: `cargo test`
Expected: all existing tests still PASS.

- [ ] **Step 2: Create the fixture** `tests/fixtures/codex_rollout.jsonl` (5 lines):

```jsonl
{"timestamp":"2026-09-12T10:00:00Z","type":"session_meta","payload":{"id":"cdx-1"}}
{"timestamp":"2026-09-12T10:00:01Z","type":"turn_context","payload":{"cwd":"/proj"}}
{"timestamp":"2026-09-12T10:00:05Z","type":"response_item","payload":{"type":"skill_instructions","name":"technical-writing","path":"/home/u/.agents/skills/technical-writing/SKILL.md"}}
{"timestamp":"2026-09-12T10:00:09Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":[\"bash\",\"-lc\",\"cat .agents/skills/code-review/SKILL.md\"]}"}}
{"timestamp":"2026-09-12T10:00:10Z","type":"token_usage","payload":{"last_token_usage":{"output_tokens":120}}}
```

(Write each JSON object on its own single line — no wrapping.)

- [ ] **Step 3: Write the failing tests** (in `src/transcript/codex.rs`)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Cursor;
    use std::path::Path;

    #[test]
    fn finds_explicit_and_implicit_loads_with_cursor() {
        let s = scan(Path::new("tests/fixtures/codex_rollout.jsonl"), Cursor::default()).unwrap();
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
        let first = scan(Path::new("tests/fixtures/codex_rollout.jsonl"), Cursor::default()).unwrap();
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
```

- [ ] **Step 4: Run tests to verify they fail**

Run: `cargo test transcript::codex`
Expected: compile error — `scan` not defined.

- [ ] **Step 5: Implement** (`src/transcript/codex.rs`)

```rust
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
                    loads.push(SkillLoad { skill, at, turn, tokens });
                }
            }
            Some("function_call") => {
                let args = v
                    .pointer("/payload/arguments")
                    .and_then(|s| s.as_str())
                    .unwrap_or("");
                for skill in skill_reads_in_text(args) {
                    loads.push(SkillLoad { skill, at, turn, tokens });
                }
            }
            _ => {}
        }
    }

    Ok(Scan {
        skill_loads: loads,
        end: Cursor { offset, turn, tokens },
    })
}
```

- [ ] **Step 6: Run the full test suite**

Run: `cargo test`
Expected: all PASS (including the two existing `transcript` tests, untouched by the move).

- [ ] **Step 7: Commit**

```bash
git add -A src/transcript src/transcript.rs tests/fixtures/codex_rollout.jsonl
git commit -m "feat: Codex rollout scanner with explicit and implicit skill loads"
```

---

### Task 5: Harness dispatch in `run_hook` — multi-event evaluation, Codex recorder, per-harness message

**Files:**
- Modify: `src/commands.rs`
- Modify: `src/cli.rs` (add `--harness` to `HookCmd`, thread through)
- Create: `tests/fixtures/codex_post_skill_read.json`

**Interfaces:**
- Consumes: `Harness` (Task 1), `codex::{write_events_from_tool, command_text, skill_reads_in_text}` (Task 3), `transcript::codex::scan` (Task 4).
- Produces (later tasks and tests rely on these exact signatures):
  - `commands::run_hook(raw: &str, store: &Store, project_dir: &Path, global_path: Option<&Path>, harness: Harness) -> Decision`
  - `commands::message_for(rule: &RuleDef, ev: &WriteEvent, reason: &str, harness: Harness) -> String`
  - `cli::HookCmd` gains `pub harness: String` (argh option, default `"claude"`).

- [ ] **Step 1: Create fixture** `tests/fixtures/codex_post_skill_read.json`:

```json
{"hook_event_name":"PostToolUse","session_id":"cdx-1","transcript_path":"/tmp/rollout.jsonl","cwd":"/proj","tool_name":"Bash","tool_input":{"command":["bash","-lc","cat .agents/skills/tech-writing/SKILL.md"]}}
```

- [ ] **Step 2: Write the failing tests** (append to `src/commands.rs` `mod tests`; reuse the existing `write_file` helper and `CFG` const at `src/commands.rs:236-250`)

```rust
use crate::model::Harness;

#[test]
fn codex_denies_apply_patch_when_skill_never_loaded() {
    let proj = tempfile::tempdir().unwrap();
    write_file(proj.path(), ".skillforcer.toml", CFG);
    let transcript = write_file(proj.path(), "r.jsonl", "");
    let statedir = tempfile::tempdir().unwrap();
    let store = Store::with_base(statedir.path().to_path_buf());
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let hook = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"c","transcript_path":"{}","cwd":"{}","tool_name":"apply_patch","tool_input":{{"input":"*** Begin Patch\n*** Update File: src/a.rs\n+// hi\n*** End Patch"}}}}"#,
        esc(&transcript),
        esc(proj.path()),
    );
    let d = run_hook(&hook, &store, proj.path(), None, Harness::Codex);
    match d {
        Decision::Deny { reason } => assert!(reason.contains("tech-writing")),
        _ => panic!("expected deny"),
    }
}

#[test]
fn codex_allows_after_implicit_skill_read_recorded() {
    let proj = tempfile::tempdir().unwrap();
    write_file(proj.path(), ".skillforcer.toml", CFG);
    let transcript = write_file(proj.path(), "r.jsonl", "");
    let statedir = tempfile::tempdir().unwrap();
    let store = Store::with_base(statedir.path().to_path_buf());
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let post = format!(
        r#"{{"hook_event_name":"PostToolUse","session_id":"c","transcript_path":"{}","cwd":"{}","tool_name":"Bash","tool_input":{{"command":["bash","-lc","cat .agents/skills/tech-writing/SKILL.md"]}}}}"#,
        esc(&transcript),
        esc(proj.path()),
    );
    assert!(matches!(
        run_hook(&post, &store, proj.path(), None, Harness::Codex),
        Decision::Allow
    ));
    let pre = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"c","transcript_path":"{}","cwd":"{}","tool_name":"apply_patch","tool_input":{{"input":"*** Begin Patch\n*** Update File: src/a.rs\n+// hi\n*** End Patch"}}}}"#,
        esc(&transcript),
        esc(proj.path()),
    );
    assert!(matches!(
        run_hook(&pre, &store, proj.path(), None, Harness::Codex),
        Decision::Allow
    ));
}

#[test]
fn codex_scans_rollout_for_explicit_load() {
    let proj = tempfile::tempdir().unwrap();
    write_file(proj.path(), ".skillforcer.toml", CFG);
    let rollout = write_file(
        proj.path(),
        "r.jsonl",
        r#"{"timestamp":"2026-09-12T10:00:05Z","type":"response_item","payload":{"type":"skill_instructions","name":"tech-writing"}}"#,
    );
    let statedir = tempfile::tempdir().unwrap();
    let store = Store::with_base(statedir.path().to_path_buf());
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let pre = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"c","transcript_path":"{}","cwd":"{}","tool_name":"apply_patch","tool_input":{{"input":"*** Begin Patch\n*** Update File: src/a.rs\n+// hi\n*** End Patch"}}}}"#,
        esc(&rollout),
        esc(proj.path()),
    );
    assert!(matches!(
        run_hook(&pre, &store, proj.path(), None, Harness::Codex),
        Decision::Allow
    ));
}

#[test]
fn codex_default_message_names_dollar_invocation() {
    let rule = crate::config::RuleDef {
        name: "r".into(),
        extends: vec![],
        path: vec![],
        content: None,
        requires: crate::config::Requires {
            skills: crate::config::SkillSet::Any(vec!["tech-writing".into()]),
            windows: Default::default(),
        },
        message: None,
    };
    let ev = WriteEvent { path: "a.rs".into(), content: String::new() };
    let msg = message_for(&rule, &ev, "never loaded", Harness::Codex);
    assert!(msg.contains("$tech-writing"), "got: {msg}");
    assert!(msg.contains("SKILL.md"), "got: {msg}");
    let claude_msg = message_for(&rule, &ev, "never loaded", Harness::Claude);
    assert!(claude_msg.contains("Skill(\"tech-writing\")"), "got: {claude_msg}");
}
```

Note: if `RuleDef`'s fields differ from the literal above (check `src/config.rs`), construct it by deserializing a minimal TOML rule instead — match whatever compiles cleanly with the real struct.

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test codex_`
Expected: compile errors — `run_hook`/`message_for` arity.

- [ ] **Step 4: Implement in `src/commands.rs`**

1. `message_for(rule, ev, reason, harness)`: default template becomes per-harness. Replace the `unwrap_or_else` body (src/commands.rs:14-16):

```rust
    let template = rule.message.clone().unwrap_or_else(|| match harness {
        Harness::Claude => "skillforcer: rule '{rule}' requires skill(s) {skills} before writing {file}. {reason}. Load it now: Skill(\"{skills}\")".to_string(),
        Harness::Codex => "skillforcer: rule '{rule}' requires skill(s) {skills} before writing {file}. {reason}. Load it now: invoke ${skills} or read its SKILL.md".to_string(),
    });
```

2. `refresh_state` gains harness and dispatches the scanner:

```rust
fn refresh_state(st: &mut SessionState, transcript_path: &Path, harness: Harness) {
    let scan = match harness {
        Harness::Claude => transcript::scan(transcript_path, st.cursor),
        Harness::Codex => transcript::codex::scan(transcript_path, st.cursor),
    };
    match scan {
        Ok(scan) => {
            st.loads.extend(scan.skill_loads);
            st.cursor = scan.end;
        }
        Err(e) => {
            eprintln!("skillforcer: transcript scan failed ({e}); continuing");
        }
    }
}
```

3. `run_hook(..., harness: Harness)`:
   - PostToolUse: after the existing Claude `skill_from_tool` block, add the Codex branch (record implicit reads; explicit loads are backfilled by `refresh_state`'s scanner on the next PreToolUse):

```rust
        HookInput::PostToolUse(p) => {
            let skills: Vec<String> = match harness {
                Harness::Claude => claude::skill_from_tool(&p.tool_name, &p.tool_input)
                    .into_iter()
                    .collect(),
                Harness::Codex => crate::adapter::codex::skill_reads_in_text(
                    &crate::adapter::codex::command_text(&p.tool_input),
                ),
            };
            if !skills.is_empty() {
                let mut st = store.load(&p.session_id);
                refresh_state(&mut st, &p.transcript_path, harness);
                for skill in skills {
                    st.loads.push(crate::model::SkillLoad {
                        skill,
                        at: jiff::Timestamp::now(),
                        turn: st.cursor.turn,
                        tokens: st.cursor.tokens,
                    });
                }
                let _ = store.save(&st);
                store.prune(std::time::Duration::from_secs(60 * 60 * 24 * 7));
            }
            Decision::Allow
        }
```

   - PreToolUse: extract a `Vec<WriteEvent>`:

```rust
            let events: Vec<WriteEvent> = match harness {
                Harness::Claude => claude::write_event_from_tool(&p.tool_name, &p.tool_input)
                    .into_iter()
                    .collect(),
                Harness::Codex => {
                    crate::adapter::codex::write_events_from_tool(&p.tool_name, &p.tool_input)
                }
            };
            if events.is_empty() {
                return Decision::Allow;
            }
```

     then wrap the existing rule loop in `for ev in &events { ... }` (the loop body is unchanged except `&ev` → `ev` adjustments and `message_for(&compiled.def, ev, &res.reason, harness)`). First deny wins; if no event trips a rule → `Allow`. Call `refresh_state(&mut st, &p.transcript_path, harness)`.

4. Update all existing callers/tests to pass `Harness::Claude` (four tests in `src/commands.rs`, plus `cli.rs` — next step).

5. `src/cli.rs`: add to `HookCmd`:

```rust
/// Run as an agent hook (reads hook JSON on stdin).
#[derive(FromArgs, Debug)]
#[argh(subcommand, name = "hook")]
pub struct HookCmd {
    /// agent harness the hook serves: claude (default) or codex
    #[argh(option, default = "String::from(\"claude\")")]
    pub harness: String,
}
```

and in `dispatch`, `Sub::Hook(h)` passes `crate::model::Harness::from_flag(&h.harness)` as the new `run_hook` argument.

- [ ] **Step 5: Run the full test suite**

Run: `cargo test`
Expected: all PASS, including the 4 new codex tests and updated Claude ones.

- [ ] **Step 6: Commit**

```bash
git add src/commands.rs src/cli.rs tests/fixtures/codex_post_skill_read.json
git commit -m "feat: harness dispatch in run_hook with Codex guard and recorder"
```

---

### Task 6: Installer — Codex hooks.json target, harness detection, CLI flags

**Files:**
- Modify: `src/install.rs`
- Modify: `src/cli.rs` (`InstallCmd`/`UninstallCmd` flags + dispatch)

**Interfaces:**
- Consumes: `Harness` (Task 1), existing `ensure_event`/`remove_hooks`/`read_root`/`write_root` machinery in `src/install.rs`.
- Produces:
  - `install::PRE_MATCHER_CODEX: &str = "apply_patch|Edit|Write|Bash"`, `install::POST_MATCHER_CODEX: &str = "Bash|apply_patch"`
  - `Target::settings_path(&self, project_dir: &Path, harness: Harness) -> Result<PathBuf>` (signature change; Codex: Project → `.codex/hooks.json`, User → `~/.codex/hooks.json`, Local → error `"--local is Claude-only"`)
  - `install::add_hooks(root: &mut Value, exe: &str, harness: Harness)` (Codex command string: `{exe} hook --harness codex`)
  - `install::detect_harnesses(project_dir: &Path) -> Vec<Harness>`
  - `install::install(target, exe, project_dir, dry_run, harness) -> Result<String>`, `install::uninstall(target, project_dir, harness) -> Result<()>`
  - `install::CODEX_TRUST_NOTE: &str` — the post-install warning text.

**Format note:** `.codex/hooks.json` uses the same `{"hooks": {"<Event>": [{"matcher": ..., "hooks": [{"type": "command", "command": ...}]}]}}` structure as Claude settings (Codex's hook system mirrors Claude's; spec §9 accepts pinning this at implementation — this plan pins it to the mirrored shape). The existing `ensure_event`/`is_ours`/`remove_hooks` therefore work unchanged on the Codex root.

- [ ] **Step 1: Write the failing tests** (append to `src/install.rs` `mod tests`)

```rust
use crate::model::Harness;

#[test]
fn codex_add_hooks_uses_codex_matchers_and_flag() {
    let mut root = json!({});
    add_hooks(&mut root, "/usr/bin/skillforcer", Harness::Codex);
    add_hooks(&mut root, "/usr/bin/skillforcer", Harness::Codex);
    let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
    assert_eq!(pre.len(), 1);
    assert_eq!(pre[0]["matcher"], "apply_patch|Edit|Write|Bash");
    assert!(pre[0]["hooks"][0]["command"]
        .as_str()
        .unwrap()
        .ends_with("hook --harness codex"));
    let post = root["hooks"]["PostToolUse"].as_array().unwrap();
    assert_eq!(post[0]["matcher"], "Bash|apply_patch");
}

#[test]
fn codex_settings_paths() {
    let proj = std::path::Path::new("/proj");
    let p = Target::Project.settings_path(proj, Harness::Codex).unwrap();
    assert!(p.ends_with(std::path::Path::new(".codex/hooks.json")));
    assert!(Target::Local.settings_path(proj, Harness::Codex).is_err());
    let c = Target::Project.settings_path(proj, Harness::Claude).unwrap();
    assert!(c.ends_with(std::path::Path::new(".claude/settings.json")));
}

#[test]
fn detect_harnesses_by_marker_dirs() {
    let t = tempfile::tempdir().unwrap();
    assert_eq!(detect_harnesses(t.path()), vec![Harness::Claude]); // fallback
    std::fs::create_dir_all(t.path().join(".codex")).unwrap();
    assert_eq!(detect_harnesses(t.path()), vec![Harness::Codex]);
    std::fs::create_dir_all(t.path().join(".claude")).unwrap();
    assert_eq!(detect_harnesses(t.path()), vec![Harness::Claude, Harness::Codex]);
    let t2 = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(t2.path().join(".agents").join("skills")).unwrap();
    assert_eq!(detect_harnesses(t2.path()), vec![Harness::Codex]);
}

#[test]
fn codex_install_and_uninstall_roundtrip() {
    let proj = tempfile::tempdir().unwrap();
    let msg = install(Target::Project, "/x/skillforcer", proj.path(), false, Harness::Codex).unwrap();
    assert!(msg.contains("hooks.json"));
    let written = std::fs::read_to_string(proj.path().join(".codex").join("hooks.json")).unwrap();
    assert!(written.contains("--harness codex"));
    uninstall(Target::Project, proj.path(), Harness::Codex).unwrap();
    let after = std::fs::read_to_string(proj.path().join(".codex").join("hooks.json")).unwrap();
    assert!(!after.contains("skillforcer"));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test install`
Expected: compile errors (arity/missing items).

- [ ] **Step 3: Implement in `src/install.rs`**

```rust
use crate::model::Harness;

pub const PRE_MATCHER_CODEX: &str = "apply_patch|Edit|Write|Bash";
pub const POST_MATCHER_CODEX: &str = "Bash|apply_patch";

pub const CODEX_TRUST_NOTE: &str = "\
IMPORTANT: Codex requires hooks to be trusted before they run.\n\
Open Codex and run /hooks to review and approve the skillforcer hook.\n\
Until then, skillforcer enforces NOTHING — and in `codex exec` automation\n\
untrusted hooks are skipped silently.";

pub fn detect_harnesses(project_dir: &Path) -> Vec<Harness> {
    let mut found = Vec::new();
    if project_dir.join(".claude").is_dir() {
        found.push(Harness::Claude);
    }
    if project_dir.join(".codex").is_dir()
        || project_dir.join(".agents").join("skills").is_dir()
    {
        found.push(Harness::Codex);
    }
    if found.is_empty() {
        found.push(Harness::Claude); // fallback default
    }
    found
}
```

`Target::settings_path` gains `harness: Harness`:

```rust
    pub fn settings_path(&self, project_dir: &Path, harness: Harness) -> Result<PathBuf> {
        Ok(match (harness, self) {
            (Harness::Claude, Target::Project) => {
                project_dir.join(".claude").join("settings.json")
            }
            (Harness::Claude, Target::Local) => {
                project_dir.join(".claude").join("settings.local.json")
            }
            (Harness::Claude, Target::User) => {
                let home = directories::BaseDirs::new().context("no home dir")?;
                home.home_dir().join(".claude").join("settings.json")
            }
            (Harness::Codex, Target::Project) => project_dir.join(".codex").join("hooks.json"),
            (Harness::Codex, Target::User) => {
                let home = directories::BaseDirs::new().context("no home dir")?;
                home.home_dir().join(".codex").join("hooks.json")
            }
            (Harness::Codex, Target::Local) => {
                anyhow::bail!("--local is Claude-only; Codex has no settings.local analogue")
            }
        })
    }
```

`event_entry` gains the full command string as a parameter (or `add_hooks` computes it):

```rust
pub fn add_hooks(root: &mut Value, exe: &str, harness: Harness) {
    if !root.is_object() {
        *root = json!({});
    }
    let (pre, post, cmd) = match harness {
        Harness::Claude => (PRE_MATCHER, POST_MATCHER, format!("{exe} hook")),
        Harness::Codex => (
            PRE_MATCHER_CODEX,
            POST_MATCHER_CODEX,
            format!("{exe} hook --harness codex"),
        ),
    };
    ensure_event(root, "PreToolUse", pre, &cmd);
    ensure_event(root, "PostToolUse", post, &cmd);
}
```

(`ensure_event`/`event_entry` change from taking `exe` to taking the finished command string; `is_ours` and `remove_hooks` are unchanged.) `install`/`uninstall` gain the `harness` parameter and pass it through to `settings_path`/`add_hooks`.

- [ ] **Step 4: Wire the CLI** (`src/cli.rs`)

`InstallCmd` and `UninstallCmd` each gain:

```rust
    /// force installing for Claude Code
    #[argh(switch)]
    pub claude: bool,
    /// force installing for Codex CLI
    #[argh(switch)]
    pub codex: bool,
```

Dispatch for both install and uninstall:

```rust
            let harnesses: Vec<crate::model::Harness> = if c.claude || c.codex {
                let mut v = Vec::new();
                if c.claude { v.push(crate::model::Harness::Claude); }
                if c.codex { v.push(crate::model::Harness::Codex); }
                v
            } else {
                crate::install::detect_harnesses(&cwd)
            };
            if c.local && harnesses.contains(&crate::model::Harness::Codex) {
                anyhow::bail!("--local is Claude-only; use --codex without --local");
            }
            for h in harnesses {
                let summary = crate::install::install(target, &exe, &cwd, c.dry_run, h)?;
                println!("{summary}");
                if h == crate::model::Harness::Codex && !c.dry_run {
                    println!("{}", crate::install::CODEX_TRUST_NOTE);
                }
            }
```

(`UninstallCmd` has no `local` conflict check beyond the same bail; scaffolding of `.skillforcer.toml` stays outside the loop, once.) Existing Claude-only call sites in `cli.rs` are replaced by this loop.

- [ ] **Step 5: Run the full test suite, then a manual smoke**

Run: `cargo test`
Expected: all PASS.

Run: `cargo run -- install --codex --dry-run` (from the repo root)
Expected: dry-run JSON containing `"--harness codex"` and matchers `apply_patch|Edit|Write|Bash`; no files written.

- [ ] **Step 6: Commit**

```bash
git add src/install.rs src/cli.rs
git commit -m "feat: Codex hooks.json installer with harness detection and trust warning"
```

---

### Task 7: End-to-end smoke test (Codex)

**Files:**
- Modify: `tests/e2e.rs`

**Interfaces:**
- Consumes: the built binary (existing e2e tests spawn it via `Command`); `hook --harness codex` (Task 5).

- [ ] **Step 1: Write the test** — mirror the structure of the existing `hook_denies_uncovered_comment_write` in `tests/e2e.rs` (same binary-spawn pattern via `env!("CARGO_BIN_EXE_skillforcer")` or however the existing test locates the binary — copy its exact mechanism):

```rust
#[test]
fn codex_hook_deny_then_allow_after_skill_read() {
    let proj = tempfile::tempdir().unwrap();
    std::fs::write(
        proj.path().join(".skillforcer.toml"),
        r#"[[rule]]
        name = "comments"
        path = ["**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], session = true }
        message = "Load {skills} first""#,
    )
    .unwrap();
    std::fs::write(proj.path().join("r.jsonl"), "").unwrap();
    let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
    let state = tempfile::tempdir().unwrap();

    let pre = format!(
        r#"{{"hook_event_name":"PreToolUse","session_id":"e2e-cdx","transcript_path":"{}","cwd":"{}","tool_name":"apply_patch","tool_input":{{"input":"*** Begin Patch\n*** Update File: src/a.rs\n+// hi\n*** End Patch"}}}}"#,
        esc(&proj.path().join("r.jsonl")),
        esc(proj.path()),
    );
    // 1. deny before any load
    let out = run_hook_binary(&pre, proj.path(), state.path());
    assert!(out.contains("\"permissionDecision\":\"deny\"") || out.contains("permissionDecision"), "expected deny JSON, got: {out}");

    // 2. record an implicit skill read
    let post = format!(
        r#"{{"hook_event_name":"PostToolUse","session_id":"e2e-cdx","transcript_path":"{}","cwd":"{}","tool_name":"Bash","tool_input":{{"command":["bash","-lc","cat .agents/skills/tech-writing/SKILL.md"]}}}}"#,
        esc(&proj.path().join("r.jsonl")),
        esc(proj.path()),
    );
    run_hook_binary(&post, proj.path(), state.path());

    // 3. retried write passes (empty stdout = allow)
    let out = run_hook_binary(&pre, proj.path(), state.path());
    assert!(!out.contains("deny"), "expected allow, got: {out}");
}
```

Where `run_hook_binary(input, cwd, state_dir)` is a small helper: spawn the binary with args `["hook", "--harness", "codex"]`, `current_dir(cwd)`, stdin piped `input`, and capture stdout. **Important:** the state store discovers a user-level directory (`Store::discover`) — check how the existing e2e test isolates state; if it doesn't (session ids are unique per test), just use a unique `session_id` (as above, `e2e-cdx`) and drop the `state_dir` parameter to match the existing helper pattern exactly. Follow whatever the existing e2e test does.

- [ ] **Step 2: Run the test**

Run: `cargo test --test e2e codex_hook`
Expected: PASS (deny JSON on first call, empty stdout on third).

- [ ] **Step 3: Commit**

```bash
git add tests/e2e.rs
git commit -m "test: end-to-end Codex hook deny/record/allow smoke"
```

---

### Task 8: `skillforcer-config` skill goes harness-aware + Codex plugin manifest

**Files:**
- Modify: `skills/skillforcer-config/SKILL.md`
- Create: Codex plugin manifest (path per fetched docs — see Step 2; likely `.codex-plugin/plugin.json` or an entry in an existing manifest format)

- [ ] **Step 1: Update SKILL.md**

Frontmatter `description`: replace "…or installing/uninstalling its Claude Code hooks." with "…or installing/uninstalling its hooks (Claude Code and Codex CLI)."

In the **Overview** section, replace the sentence naming Claude tools with harness-neutral phrasing plus one sentence per harness:

```markdown
skillforcer forces a skill to have loaded recently enough before it lets a governed
write through. It runs as a `PreToolUse` hook in your coding agent — Claude Code and
Codex CLI are both supported: when a write matches a rule and none of the required
skills loaded within the rule's freshness window, skillforcer denies the write and
returns a message telling the agent which skill to load. A `PostToolUse` hook records
skill loads (Claude's `Skill` tool; in Codex, reads of a skill's `SKILL.md`) so the
next write can check freshness. In Codex, explicit `$skill` invocations are detected
from the session rollout.
```

In the **Install** section, after the existing `skillforcer install` paragraph, add:

```markdown
`install` detects the harness from the project (`.claude/` → Claude Code, `.codex/` or
`.agents/skills/` → Codex; both → both; neither → Claude Code). Force one with
`--claude` or `--codex`. For Codex, hooks go to `.codex/hooks.json` (project) or
`~/.codex/hooks.json` (`--user`); `--local` is Claude-only.

**Codex trust gate:** after installing, run `/hooks` inside Codex and approve the
skillforcer hook. Until it is trusted it enforces nothing, and `codex exec` skips
untrusted hooks silently.
```

In the rules documentation, add one line about skill naming:

```markdown
Skill names in rules match by suffix across harnesses: `tech-writing:technical-writing`
(Claude plugin:skill form) also matches a Codex load of the `technical-writing` skill
directory, and vice versa.
```

- [ ] **Step 2: Codex plugin manifest**

First fetch the current manifest format: `https://developers.openai.com/codex/plugins` (and the "Build plugins" page it links). Create a **skill-only** plugin manifest for this repo — name `skillforcer`, description matching `.claude-plugin/plugin.json`, exposing `skills/skillforcer-config` as its only skill. **Do not include any hooks in the bundle** (hook registration stays with `skillforcer install`).

If the docs are unreachable from the sandbox: create `.codex-plugin/plugin.json` mirroring `.claude-plugin/plugin.json`'s fields plus `"skills": ["skills/skillforcer-config"]` — the README (Task 9) already hedges the plugin route as version-gated, so no extra README note is needed. Record in the commit message that the schema was best-effort.

- [ ] **Step 3: Verify**

Run: `cargo test` (docs-only change — confirm nothing broke) and proofread SKILL.md rendered (`cat` it; check no broken markdown).

- [ ] **Step 4: Commit**

```bash
git add skills/skillforcer-config/SKILL.md .codex-plugin 2>/dev/null || git add skills/skillforcer-config/SKILL.md
git commit -m "docs(skill): harness-aware skillforcer-config; Codex plugin manifest"
```

---

### Task 9: README — both harnesses, "coding agent" phrasing

**Files:**
- Modify: `README.md`

**Copy rules (from the user, verbatim intent):** say once up front that both Claude Code and Codex CLI are supported; elsewhere prefer "coding agent" (or "Claude/Codex" where a concrete name is clearer); don't overcomplicate, don't lie.

- [ ] **Step 1: Apply the edits**

1. Tagline (line 3): `**Require a skill to be loaded before the writes that depend on it — in Claude Code and OpenAI Codex CLI.**`
2. Intro paragraphs: replace "Claude Code hook" with "hook in your coding agent (Claude Code and Codex CLI are supported)" on first mention; subsequent mentions become "the agent" / "the coding agent". The "In action" example stays Claude-flavored (it is a genuine Claude session) — add one sentence after it: `The same loop works in Codex: the write is denied with the reason, the model reads the skill, and the retry passes.`
3. "How it works": rephrase to name both recorders: `Two hooks, installed by the CLI. A PreToolUse guard decides each write (Claude's Write/Edit tools; Codex's apply_patch); a PostToolUse recorder logs skill loads (Claude's Skill tool; Codex SKILL.md reads) to per-session state. The session transcript — Claude's transcript or Codex's rollout — is the source of truth, so detection holds even across the recorder. Explicit $skill invocations in Codex are picked up from the rollout.`
4. "Register the hooks" section: document detection + flags + trust note:

```markdown
### Register the hooks

From your project's root directory:

​```sh
skillforcer install
​```

`install` detects your coding agent from the project: `.claude/` → Claude Code,
`.codex/` or `.agents/skills/` → Codex CLI, both → both, neither → Claude Code.
Force one with `--claude` or `--codex`.

- Claude Code: hooks go to `.claude/settings.json` (`--local` for
  `settings.local.json`, `--user` for `~/.claude/settings.json`).
- Codex: hooks go to `.codex/hooks.json` (`--user` for `~/.codex/hooks.json`).
  **After installing, run `/hooks` inside Codex and approve the skillforcer hook —
  until it is trusted it enforces nothing, and `codex exec` skips untrusted hooks
  silently.**

`install` also writes a starter `.skillforcer.toml` if one doesn't exist. `uninstall`
removes the hooks it added; it never touches hooks belonging to other tools.
```

5. "Configuration skill" section: keep the Claude plugin instructions, then add:

```markdown
For Codex, three routes:

​```text
# via Codex's plugin system (add this repo as a marketplace, then install)
/plugins            # add strowk/skillforcer as a marketplace and install skillforcer

# or via the built-in skill-installer skill
$skill-installer install the skill from strowk/skillforcer, path skills/skillforcer-config
​```

Or copy it manually: `cp -r skills/skillforcer-config ~/.agents/skills/`. Codex plugin
tooling is still evolving; if the plugin route fails on your version, use either of the
other two.
```

6. Configuration section: add one sentence after the rule-anatomy paragraph: `Skill names match by suffix across agents: tech-writing:technical-writing also matches a Codex load of the technical-writing skill directory.` Replace remaining incidental "Claude" mentions in Configuration/Debugging/Fail-open with "the agent" where they read naturally; leave exact tool names (`Write`/`Edit`, `apply_patch`) as-is since they're factual.

- [ ] **Step 2: Proofread**

Read the full README top to bottom once: no remaining claim that implies Claude-only behavior, no invented Codex commands beyond the three routes above, code fences balanced.

- [ ] **Step 3: Commit**

```bash
git add README.md
git commit -m "docs: README covers Claude Code and Codex CLI"
```

---

### Task 10: Final sweep

- [ ] **Step 1:** `cargo fmt` then `cargo test` — everything green, no fmt diff.
- [ ] **Step 2:** `cargo run -- install --dry-run` in a temp dir containing only `.codex/` — confirm it targets hooks.json; and in a dir with neither marker — confirm it targets `.claude/settings.json`.
- [ ] **Step 3:** Commit any stragglers; `git log --oneline` should show one commit per task.
