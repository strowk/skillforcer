use crate::model::{Decision, WriteEvent};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;

pub fn render_decision(d: &Decision) -> Option<serde_json::Value> {
    match d {
        Decision::Allow => None,
        Decision::Deny { reason } => Some(json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "deny",
                "permissionDecisionReason": reason,
            }
        })),
    }
}

#[derive(Debug, Deserialize)]
struct RawHook {
    hook_event_name: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    transcript_path: String,
    #[serde(default)]
    cwd: String,
    #[serde(default)]
    tool_name: String,
    #[serde(default)]
    tool_input: serde_json::Value,
}

#[derive(Debug, Clone)]
pub struct PreToolUse {
    pub session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
}

pub type PostToolUse = PreToolUse;

#[derive(Debug, Clone)]
pub enum HookInput {
    PreToolUse(PreToolUse),
    PostToolUse(PostToolUse),
    Other,
}

pub fn parse_hook_input(raw: &str) -> anyhow::Result<HookInput> {
    let h: RawHook = serde_json::from_str(raw)?;
    let common = PreToolUse {
        session_id: h.session_id,
        transcript_path: PathBuf::from(h.transcript_path),
        cwd: PathBuf::from(h.cwd),
        tool_name: h.tool_name,
        tool_input: h.tool_input,
    };
    Ok(match h.hook_event_name.as_str() {
        "PreToolUse" => HookInput::PreToolUse(common),
        "PostToolUse" => HookInput::PostToolUse(common),
        _ => HookInput::Other,
    })
}

pub fn skill_from_tool(tool_name: &str, input: &serde_json::Value) -> Option<String> {
    if tool_name != "Skill" {
        return None;
    }
    input.get("skill")?.as_str().map(str::to_string)
}

pub fn write_event_from_tool(tool_name: &str, input: &serde_json::Value) -> Option<WriteEvent> {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let (path, content) = match tool_name {
        "Write" => (s("file_path")?, s("content")?),
        "Edit" => (s("file_path")?, s("new_string")?),
        "MultiEdit" => {
            let edits = input.get("edits")?.as_array()?;
            let joined = edits
                .iter()
                .filter_map(|e| e.get("new_string").and_then(|v| v.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (s("file_path")?, joined)
        }
        "NotebookEdit" => (s("notebook_path")?, s("new_source")?),
        _ => return None,
    };
    Some(WriteEvent {
        path: PathBuf::from(path),
        content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Decision;

    #[test]
    fn allow_renders_nothing() {
        assert_eq!(render_decision(&Decision::Allow), None);
    }

    #[test]
    fn deny_renders_permission_json() {
        let v = render_decision(&Decision::Deny {
            reason: "load it".into(),
        })
        .unwrap();
        assert_eq!(v["hookSpecificOutput"]["hookEventName"], "PreToolUse");
        assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
        assert_eq!(
            v["hookSpecificOutput"]["permissionDecisionReason"],
            "load it"
        );
    }
}

#[cfg(test)]
mod parse_tests {
    use super::*;

    #[test]
    fn parses_pre_write_and_extracts_content() {
        let raw = include_str!("../../tests/fixtures/pre_write.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
        assert_eq!(p.tool_name, "Write");
        let ev = write_event_from_tool(&p.tool_name, &p.tool_input).unwrap();
        assert_eq!(ev.path.to_str().unwrap(), "src/lib.rs");
        assert!(ev.content.contains("// a comment"));
    }

    #[test]
    fn edit_uses_new_string() {
        let raw = include_str!("../../tests/fixtures/pre_edit.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
        let ev = write_event_from_tool(&p.tool_name, &p.tool_input).unwrap();
        assert_eq!(ev.content, "// new comment");
    }

    #[test]
    fn notebook_uses_notebook_path_and_new_source() {
        let raw = include_str!("../../tests/fixtures/pre_notebook.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
        let ev = write_event_from_tool(&p.tool_name, &p.tool_input).unwrap();
        assert_eq!(ev.path.to_str().unwrap(), "nb.ipynb");
        assert_eq!(ev.content, "# heading");
    }

    #[test]
    fn unknown_tool_yields_none() {
        assert!(write_event_from_tool("Bash", &serde_json::json!({"command":"ls"})).is_none());
    }

    #[test]
    fn extracts_skill_from_post() {
        let raw = include_str!("../../tests/fixtures/post_skill.json");
        let HookInput::PostToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
        assert_eq!(
            skill_from_tool(&p.tool_name, &p.tool_input).unwrap(),
            "tech-writing:technical-writing"
        );
    }
}
