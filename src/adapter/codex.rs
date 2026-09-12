//! Codex CLI adapter: apply_patch envelope parsing, write-event extraction,
//! and SKILL.md read detection. Hook stdin parsing and deny rendering are
//! shared with the Claude adapter — Codex uses the same field names and the
//! same `hookSpecificOutput` deny JSON.

use crate::adapter::claude;
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
        } else if let Some(a) = line.strip_prefix('+')
            && path.is_some()
        {
            added.push(a.to_string());
        }
    }
    flush(&mut path, &mut added, &mut events);
    events
}

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

fn all_strings(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.iter().map(all_strings).collect::<Vec<_>>().join("\n"),
        serde_json::Value::Object(o) => o.values().map(all_strings).collect::<Vec<_>>().join("\n"),
        _ => String::new(),
    }
}

pub fn write_events_from_tool(tool_name: &str, input: &serde_json::Value) -> Vec<WriteEvent> {
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

/// Returns skill names for `SKILL.md` references in `text` that are actually
/// being read, excluding ones that are write targets in the same text: an
/// `apply_patch` Add/Update target, a shell redirect (`>`/`>>`) destination,
/// or the argument of a removal command (`rm`, `Remove-Item`, `del`). A write
/// to a skill's file must never register as a load of that skill.
///
/// Conservative by design: an ambiguous reference is dropped, not counted —
/// an extra deny is acceptable, a wrong allow is not.
pub fn skill_loads_in_text(text: &str) -> Vec<String> {
    let write_targets: Vec<String> = parse_apply_patch(text)
        .into_iter()
        .map(|ev| ev.path.to_string_lossy().replace('\\', "/"))
        .collect();

    let re = regex::Regex::new(r"[A-Za-z0-9_@.~:\-/\\]+[/\\]SKILL\.md").expect("static regex");
    let mut out: Vec<String> = Vec::new();
    for m in re.find_iter(text) {
        let normalized = m.as_str().replace('\\', "/");

        if write_targets
            .iter()
            .any(|w| normalized.ends_with(w.as_str()))
        {
            continue;
        }
        if text[..m.start()].trim_end().ends_with('>') {
            continue;
        }
        let line_start = text[..m.start()].rfind('\n').map(|i| i + 1).unwrap_or(0);
        let removal_cmd = text[line_start..m.start()]
            .split_whitespace()
            .next()
            .unwrap_or("");
        if matches!(removal_cmd, "rm" | "Remove-Item" | "del") {
            continue;
        }

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

#[cfg(test)]
mod patch_tests {
    use super::*;

    #[test]
    fn add_file_collects_all_plus_lines() {
        let p =
            "*** Begin Patch\n*** Add File: src/new.rs\n+// hello\n+fn main() {}\n*** End Patch";
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
        assert!(
            parse_apply_patch("*** Begin Patch\n*** Delete File: gone.rs\n*** End Patch")
                .is_empty()
        );
        assert!(parse_apply_patch("echo hello").is_empty());
    }

    #[test]
    fn envelope_embedded_in_shell_command_is_found() {
        let p =
            "apply_patch <<'EOF'\n*** Begin Patch\n*** Add File: c.ts\n+// ts\n*** End Patch\nEOF";
        let evs = parse_apply_patch(p);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].path.to_str().unwrap(), "c.ts");
    }
}

#[cfg(test)]
mod extract_tests {
    use super::{skill_loads_in_text, skill_reads_in_text, write_events_from_tool};
    use crate::adapter::claude::{HookInput, parse_hook_input};

    #[test]
    fn apply_patch_tool_input_extracts_events() {
        let raw = include_str!("../../tests/fixtures/codex_pre_apply_patch.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
        assert_eq!(p.tool_name, "apply_patch");
        let evs = write_events_from_tool(&p.tool_name, &p.tool_input);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].path.to_str().unwrap(), "src/auth.rs");
        assert_eq!(evs[0].content, "// refresh token");
    }

    #[test]
    fn shell_embedded_apply_patch_extracts_events() {
        let raw = include_str!("../../tests/fixtures/codex_pre_shell_patch.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
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
        assert_eq!(
            skill_reads_in_text(cmd),
            vec!["technical-writing".to_string()]
        );
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

    #[test]
    fn apply_patch_update_of_skill_md_is_not_a_load() {
        let p = "*** Begin Patch\n*** Update File: x/foo/SKILL.md\n+new body\n*** End Patch";
        assert!(skill_loads_in_text(p).is_empty());
    }

    #[test]
    fn shell_redirect_write_to_skill_md_is_not_a_load() {
        assert!(skill_loads_in_text("cat > x/foo/SKILL.md <<'EOF'\nbody\nEOF").is_empty());
        assert!(skill_loads_in_text("echo body >> x/foo/SKILL.md").is_empty());
    }

    #[test]
    fn removal_of_skill_md_is_not_a_load() {
        assert!(skill_loads_in_text("rm x/foo/SKILL.md").is_empty());
        assert!(skill_loads_in_text("Remove-Item x/foo/SKILL.md").is_empty());
        assert!(skill_loads_in_text("del x/foo/SKILL.md").is_empty());
    }

    #[test]
    fn genuine_read_of_skill_md_is_still_a_load() {
        assert_eq!(
            skill_loads_in_text("cat x/foo/SKILL.md"),
            vec!["foo".to_string()]
        );
    }
}
