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
