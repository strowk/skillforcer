use crate::adapter::agents;
use crate::adapter::claude::{self, HookInput};
use crate::config::{self, Config, RuleDef, SkillSet};
use crate::freshness;
use crate::model::{Decision, Harness, Now, WriteEvent};
use crate::rules;
use crate::state::{SessionState, Store};
use crate::transcript;
use std::path::Path;

pub fn message_for(rule: &RuleDef, ev: &WriteEvent, reason: &str, harness: Harness) -> String {
    let skills = match &rule.requires.skills {
        SkillSet::Any(s) | SkillSet::All(s) => s.join(", "),
    };
    let template = rule.message.clone().unwrap_or_else(|| match harness {
        Harness::Claude => "skillforcer: rule '{rule}' requires skill(s) {skills} before writing {file}. {reason}. Load it now: Skill(\"{skills}\")".to_string(),
        Harness::Codex => "skillforcer: rule '{rule}' requires skill(s) {skills} before writing {file}. {reason}. Load it now: invoke the skill(s) {skills} (a $-mention in Codex) or read its SKILL.md".to_string(),
    });
    template
        .replace("{file}", &ev.path.display().to_string())
        .replace("{rule}", &rule.name)
        .replace("{skills}", &skills)
        .replace("{reason}", reason)
}

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

pub fn run_hook(
    raw: &str,
    store: &Store,
    project_dir: &Path,
    global_path: Option<&Path>,
    harness: Harness,
) -> Decision {
    let input = match claude::parse_hook_input(raw) {
        Ok(i) => i,
        Err(e) => {
            eprintln!("skillforcer: could not parse hook input ({e}); allowing");
            return Decision::Allow;
        }
    };

    match input {
        HookInput::PostToolUse(p) => {
            let skills: Vec<String> = match harness {
                Harness::Claude => claude::skill_from_tool(&p.tool_name, &p.tool_input)
                    .into_iter()
                    .collect(),
                Harness::Codex => crate::adapter::codex::skill_loads_in_text(
                    &crate::adapter::codex::command_text(&p.tool_input),
                ),
            };
            if !skills.is_empty() {
                let key = claude::context_key(&p.session_id, &p.agent_id);
                let transcript = claude::context_transcript_path(&p.transcript_path, &p.agent_id);
                let mut st = store.load(&key);
                refresh_state(&mut st, &transcript, harness);
                // record the just-loaded skill(s) at the current cursor position
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
        HookInput::PreToolUse(p) => {
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
            // Resolve project dir from the hook's cwd when present.
            let proj = if p.cwd.as_os_str().is_empty() {
                project_dir.to_path_buf()
            } else {
                p.cwd.clone()
            };

            // Per-context enforcement gate (§3.3). Re-keying means a subagent
            // starts with an empty record, so its first governed write is denied
            // until it loads the skill in its own context — a deadlock for a
            // subagent that lacks the `Skill` tool. Enforce only for the root
            // (empty `agent_id`, which includes every Codex hook) and for
            // subagents positively confirmed to have the `Skill` tool;
            // everything uncertain passes without evaluating any rule.
            if !p.agent_id.is_empty() {
                let dirs = agents::default_dirs(&proj);
                if !agents::resolve(&p.agent_type, &dirs).should_enforce() {
                    return Decision::Allow;
                }
            }

            let cfg: Config = match config::load(&proj, global_path) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("skillforcer: could not load config ({e}); allowing");
                    return Decision::Allow;
                }
            };
            let fail_open = cfg.defaults.fail_open;
            let combine = cfg.defaults.combine_freshness;

            let key = claude::context_key(&p.session_id, &p.agent_id);
            let transcript = claude::context_transcript_path(&p.transcript_path, &p.agent_id);
            let mut st = store.load(&key);
            refresh_state(&mut st, &transcript, harness);
            let _ = store.save(&st);
            store.prune(std::time::Duration::from_secs(60 * 60 * 24 * 7));

            let now = Now {
                time: jiff::Timestamp::now(),
                turn: st.cursor.turn,
                tokens: st.cursor.tokens,
            };

            for ev in &events {
                for rule in &cfg.rules {
                    let compiled = match rules::compile(rule) {
                        Ok(c) => c,
                        Err(e) => {
                            if fail_open {
                                eprintln!(
                                    "skillforcer: rule '{}' failed to compile: {e}; skipping",
                                    rule.name
                                );
                                continue;
                            } else {
                                return Decision::Deny {
                                    reason: format!(
                                        "skillforcer: rule '{}' failed to compile",
                                        rule.name
                                    ),
                                };
                            }
                        }
                    };
                    if compiled.matches(ev) {
                        let res =
                            freshness::evaluate(&compiled.def.requires, &st.loads, &now, combine);
                        if !res.satisfied {
                            return Decision::Deny {
                                reason: message_for(&compiled.def, ev, &res.reason, harness),
                            };
                        }
                    }
                }
            }
            Decision::Allow
        }
        HookInput::Other => Decision::Allow,
    }
}

pub fn render_and_print(d: &Decision) {
    if let Some(v) = claude::render_decision(d) {
        println!("{v}");
    }
}

pub fn run_check(
    project_dir: &Path,
    file: &Path,
    content: &str,
    global: Option<&Path>,
) -> anyhow::Result<Vec<String>> {
    let cfg = config::load(project_dir, global)?;
    let ev = WriteEvent {
        path: file.to_path_buf(),
        content: content.to_string(),
    };
    let mut hits = Vec::new();
    for rule in &cfg.rules {
        match rules::compile(rule) {
            Ok(compiled) => {
                if compiled.matches(&ev) {
                    hits.push(rule.name.clone());
                }
            }
            Err(e) => {
                eprintln!(
                    "skillforcer: rule '{}' failed to compile: {e}; skipping",
                    rule.name
                );
            }
        }
    }
    for h in &hits {
        println!("match: {h}");
    }
    if hits.is_empty() {
        println!("no rules match {}", file.display());
    }
    Ok(hits)
}

pub fn run_list_presets() -> String {
    let mut out = String::new();
    for p in crate::presets::builtin() {
        out.push_str(&format!("{}  (paths: {})\n", p.name, p.path.join(", ")));
    }
    out
}

pub fn run_status(store: &Store, session_id: &str) -> String {
    let st = store.load(session_id);
    let mut out = format!(
        "session {}: {} skill load(s); cursor turn={} tokens={} offset={}\n",
        st.session_id,
        st.loads.len(),
        st.cursor.turn,
        st.cursor.tokens,
        st.cursor.offset
    );
    for l in &st.loads {
        out.push_str(&format!(
            "  - {} @ {} (turn {}, {} tokens)\n",
            l.skill, l.at, l.turn, l.tokens
        ));
    }
    out
}

#[cfg(test)]
mod cmd_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn check_reports_matching_rule() {
        let proj = tempfile::tempdir().unwrap();
        let mut f = std::fs::File::create(proj.path().join(".skillforcer.toml")).unwrap();
        f.write_all(
            br#"[[rule]]
            name = "comments"
            path = ["**/*.rs"]
            content = "//"
            requires = { any_skill = ["tech-writing"], session = true }"#,
        )
        .unwrap();
        let hits = run_check(proj.path(), std::path::Path::new("src/a.rs"), "// c", None).unwrap();
        assert_eq!(hits, vec!["comments".to_string()]);
    }

    #[test]
    fn list_presets_includes_code_comments() {
        assert!(run_list_presets().contains("code-comments"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Decision;
    use crate::state::Store;
    use std::io::Write;

    fn write_file(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.join(name);
        let mut f = std::fs::File::create(&p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
        p
    }

    const CFG: &str = r#"
        [[rule]]
        name = "comments"
        path = ["**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], session = true }
        message = "Load {skills} before editing {file}"
    "#;

    #[test]
    fn denies_write_when_skill_never_loaded() {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", ""); // empty transcript = no skill loads
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());

        let hook = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
            transcript.display().to_string().replace('\\', "\\\\"),
            proj.path().display().to_string().replace('\\', "\\\\"),
        );
        let d = run_hook(&hook, &store, proj.path(), None, Harness::Claude);
        match d {
            Decision::Deny { reason } => {
                assert!(reason.contains("tech-writing"));
                assert!(reason.contains("src/a.rs"));
            }
            _ => panic!("expected deny"),
        }
    }

    #[test]
    fn allows_write_after_skill_recorded_via_post_hook() {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", "");
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());

        let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
        let post = format!(
            r#"{{"hook_event_name":"PostToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Skill","tool_input":{{"skill":"tech-writing"}}}}"#,
            esc(&transcript),
            esc(proj.path()),
        );
        assert!(matches!(
            run_hook(&post, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));

        let pre = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
            esc(&transcript),
            esc(proj.path()),
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
    }

    #[test]
    fn allows_non_matching_write() {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", "");
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());
        let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
        let pre = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"README.md","content":"no comment"}}}}"#,
            esc(&transcript),
            esc(proj.path()),
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
    }

    #[test]
    fn malformed_input_fails_open() {
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());
        assert!(matches!(
            run_hook(
                "not json",
                &store,
                std::path::Path::new("."),
                None,
                Harness::Claude
            ),
            Decision::Allow
        ));
    }

    /// Builds a hook JSON payload, including `agent_id`/`agent_type` when
    /// `agent_id` is non-empty (a subagent context).
    #[allow(clippy::too_many_arguments)]
    fn hook_json(
        event: &str,
        session: &str,
        agent_id: &str,
        agent_type: &str,
        transcript: &std::path::Path,
        proj: &std::path::Path,
        tool_name: &str,
        tool_input: &str,
    ) -> String {
        let esc = |p: &std::path::Path| p.display().to_string().replace('\\', "\\\\");
        let agent_fields = if agent_id.is_empty() {
            String::new()
        } else {
            format!(r#","agent_id":"{agent_id}","agent_type":"{agent_type}""#)
        };
        format!(
            r#"{{"hook_event_name":"{event}","session_id":"{session}"{agent_fields},"transcript_path":"{}","cwd":"{}","tool_name":"{tool_name}","tool_input":{tool_input}}}"#,
            esc(transcript),
            esc(proj),
        )
    }

    const WRITE_INPUT: &str = r#"{"file_path":"src/a.rs","content":"// hi"}"#;
    const SKILL_INPUT: &str = r#"{"skill":"tech-writing"}"#;

    fn setup() -> (
        tempfile::TempDir,
        std::path::PathBuf,
        Store,
        tempfile::TempDir,
    ) {
        let proj = tempfile::tempdir().unwrap();
        write_file(proj.path(), ".skillforcer.toml", CFG);
        let transcript = write_file(proj.path(), "t.jsonl", "");
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());
        (proj, transcript, store, statedir)
    }

    #[test]
    fn subagent_with_skill_tool_is_enforced() {
        let (proj, transcript, store, _sd) = setup();
        // `general-purpose` is a built-in writing agent that has the `Skill`
        // tool, so the gate enforces and the unloaded skill denies the write.
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a1",
            "general-purpose",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn subagent_without_skill_tool_is_allowed_before_rules() {
        let (proj, transcript, store, _sd) = setup();
        // `statusline-setup` is a built-in agent without the `Skill` tool: the
        // gate returns Allow before any rule is evaluated, even though the
        // comment write would otherwise match the `comments` rule.
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a1",
            "statusline-setup",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
    }

    #[test]
    fn subagent_resolved_from_project_file_without_skill_is_allowed() {
        let (proj, transcript, store, _sd) = setup();
        // A project agent definition whose `tools` list excludes `Skill`: the
        // gate resolves it from the file and exempts the write.
        let agents_dir = proj.path().join(".claude").join("agents");
        std::fs::create_dir_all(&agents_dir).unwrap();
        std::fs::write(
            agents_dir.join("restricted-writer.md"),
            include_str!("../tests/fixtures/agent-restricted.md"),
        )
        .unwrap();
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a1",
            "restricted-writer",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
    }

    #[test]
    fn subagent_unresolved_agent_is_allowed() {
        let (proj, transcript, store, _sd) = setup();
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a1",
            "some-unknown-plugin-agent",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
    }

    #[test]
    fn subagent_load_does_not_credit_sibling_subagent() {
        let (proj, transcript, store, _sd) = setup();
        // Subagent a1 loads the skill — recorded under its own context key.
        let post = hook_json(
            "PostToolUse",
            "s",
            "a1",
            "general-purpose",
            &transcript,
            proj.path(),
            "Skill",
            SKILL_INPUT,
        );
        assert!(matches!(
            run_hook(&post, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
        // Sibling subagent a2 writes — its own key is empty, so it is denied.
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a2",
            "general-purpose",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn root_load_does_not_credit_subagent() {
        let (proj, transcript, store, _sd) = setup();
        // Root loads the skill.
        let post = hook_json(
            "PostToolUse",
            "s",
            "",
            "",
            &transcript,
            proj.path(),
            "Skill",
            SKILL_INPUT,
        );
        assert!(matches!(
            run_hook(&post, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
        // Subagent write is still denied — the root load never reached it.
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a1",
            "general-purpose",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn subagent_load_does_not_credit_later_root_write() {
        let (proj, transcript, store, _sd) = setup();
        // Subagent loads the skill.
        let post = hook_json(
            "PostToolUse",
            "s",
            "a1",
            "general-purpose",
            &transcript,
            proj.path(),
            "Skill",
            SKILL_INPUT,
        );
        assert!(matches!(
            run_hook(&post, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
        // A later root write is denied — the subagent load never reached root.
        let pre = hook_json(
            "PreToolUse",
            "s",
            "",
            "",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Deny { .. }
        ));
    }

    #[test]
    fn subagent_load_then_same_subagent_write_is_allowed() {
        let (proj, transcript, store, _sd) = setup();
        let post = hook_json(
            "PostToolUse",
            "s",
            "a1",
            "general-purpose",
            &transcript,
            proj.path(),
            "Skill",
            SKILL_INPUT,
        );
        assert!(matches!(
            run_hook(&post, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
        // Same subagent's governed write now passes against its own record.
        let pre = hook_json(
            "PreToolUse",
            "s",
            "a1",
            "general-purpose",
            &transcript,
            proj.path(),
            "Write",
            WRITE_INPUT,
        );
        assert!(matches!(
            run_hook(&pre, &store, proj.path(), None, Harness::Claude),
            Decision::Allow
        ));
    }

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
            enabled: true,
            extends: vec![],
            path: vec![],
            content: None,
            requires: crate::config::Requires {
                skills: crate::config::SkillSet::Any(vec!["tech-writing".into()]),
                windows: Default::default(),
            },
            message: None,
        };
        let ev = WriteEvent {
            path: "a.rs".into(),
            content: String::new(),
        };
        let msg = message_for(&rule, &ev, "never loaded", Harness::Codex);
        assert!(
            msg.contains("invoke the skill(s) tech-writing"),
            "got: {msg}"
        );
        assert!(msg.contains("$-mention"), "got: {msg}");
        assert!(msg.contains("SKILL.md"), "got: {msg}");
        let claude_msg = message_for(&rule, &ev, "never loaded", Harness::Claude);
        assert!(
            claude_msg.contains("Skill(\"tech-writing\")"),
            "got: {claude_msg}"
        );
    }
}
