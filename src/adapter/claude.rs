use crate::model::{Decision, WriteEvent};
use serde::Deserialize;
use serde_json::json;
use std::path::{Path, PathBuf};

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
    /// Present only inside a subagent context; absent/empty on root events.
    #[serde(default)]
    agent_id: String,
    /// The subagent's agent type (its definition name); empty on root events.
    #[serde(default)]
    agent_type: String,
}

#[derive(Debug, Clone)]
pub struct PreToolUse {
    pub session_id: String,
    pub transcript_path: PathBuf,
    pub cwd: PathBuf,
    pub tool_name: String,
    pub tool_input: serde_json::Value,
    /// Empty for a root session; set to the subagent instance id otherwise.
    pub agent_id: String,
    /// Empty for a root session; the subagent's agent type otherwise.
    pub agent_type: String,
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
        agent_id: h.agent_id,
        agent_type: h.agent_type,
    };
    Ok(match h.hook_event_name.as_str() {
        "PreToolUse" => HookInput::PreToolUse(common),
        "PostToolUse" => HookInput::PostToolUse(common),
        _ => HookInput::Other,
    })
}

/// Identifies one context window — the unit skillforcer keys its state by.
///
/// A root session and every subagent it spawns share one `session_id` but run
/// in separate context windows, so a skill loaded in one is never in another.
/// The context key separates them: the root keeps the bare `session_id` (so
/// existing state files keep matching), and each subagent instance gets its own
/// `{session_id}--agent-{agent_id}` key. The store's filename sanitizer maps any
/// stray character to `_`, so the result is always a safe stem.
pub fn context_key(session_id: &str, agent_id: &str) -> String {
    if agent_id.is_empty() {
        session_id.to_string()
    } else {
        format!("{session_id}--agent-{agent_id}")
    }
}

/// Resolves the transcript a context actually owns.
///
/// A subagent's `PreToolUse`/`PostToolUse` payload hands over the *root*
/// transcript path, but the subagent's own turns and skill loads live in a
/// separate file. The convention (verified on disk, undocumented) is
/// `<transcript without .jsonl>/subagents/agent-<agent_id>.jsonl`. The root
/// context (empty `agent_id`) uses the payload path unchanged.
pub fn context_transcript_path(transcript_path: &Path, agent_id: &str) -> PathBuf {
    if agent_id.is_empty() {
        transcript_path.to_path_buf()
    } else {
        transcript_path
            .with_extension("")
            .join("subagents")
            .join(format!("agent-{agent_id}.jsonl"))
    }
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

    #[test]
    fn parses_agent_id_and_type_from_subagent_payload() {
        let raw = include_str!("../../tests/fixtures/pre_write_subagent.json");
        let HookInput::PreToolUse(p) = parse_hook_input(raw).unwrap() else {
            panic!()
        };
        assert_eq!(p.agent_id, "a1b2");
        assert_eq!(p.agent_type, "general-purpose");
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;

    #[test]
    fn root_context_key_is_session_id() {
        assert_eq!(context_key("sess-1", ""), "sess-1");
    }

    #[test]
    fn subagent_context_key_combines_session_and_agent() {
        assert_eq!(context_key("sess-1", "a1b2"), "sess-1--agent-a1b2");
    }

    #[test]
    fn root_transcript_path_is_unchanged() {
        let p = Path::new("/home/.claude/projects/proj/sess.jsonl");
        assert_eq!(context_transcript_path(p, ""), p);
    }

    #[test]
    fn subagent_transcript_path_is_derived() {
        let p = Path::new("/home/.claude/projects/proj/sess.jsonl");
        assert_eq!(
            context_transcript_path(p, "a1b2"),
            Path::new("/home/.claude/projects/proj/sess/subagents/agent-a1b2.jsonl")
        );
    }
}
