use crate::adapter::claude::{self, HookInput};
use crate::config::{self, Config, RuleDef, SkillSet};
use crate::freshness;
use crate::model::{Decision, Now, WriteEvent};
use crate::rules;
use crate::state::{SessionState, Store};
use crate::transcript;
use std::path::Path;

pub fn message_for(rule: &RuleDef, ev: &WriteEvent, reason: &str) -> String {
    let skills = match &rule.requires.skills {
        SkillSet::Any(s) | SkillSet::All(s) => s.join(", "),
    };
    let template = rule.message.clone().unwrap_or_else(|| {
        "skillforcer: rule '{rule}' requires skill(s) {skills} before writing {file}. {reason}. Load it with the Skill tool, then retry.".to_string()
    });
    template
        .replace("{file}", &ev.path.display().to_string())
        .replace("{rule}", &rule.name)
        .replace("{skills}", &skills)
        .replace("{reason}", reason)
}

fn refresh_state(st: &mut SessionState, transcript_path: &Path) {
    if let Ok(scan) = transcript::scan(transcript_path, st.cursor) {
        st.loads.extend(scan.skill_loads);
        st.cursor = scan.end;
    }
}

pub fn run_hook(raw: &str, store: &Store, project_dir: &Path, global_path: Option<&Path>) -> Decision {
    let input = match claude::parse_hook_input(raw) {
        Ok(i) => i,
        Err(_) => return Decision::Allow,
    };

    match input {
        HookInput::PostToolUse(p) => {
            if let Some(skill) = claude::skill_from_tool(&p.tool_name, &p.tool_input) {
                let mut st = store.load(&p.session_id);
                refresh_state(&mut st, &p.transcript_path);
                // record the just-loaded skill at the current cursor position
                st.loads.push(crate::model::SkillLoad {
                    skill,
                    at: jiff::Timestamp::now(),
                    turn: st.cursor.turn,
                    tokens: st.cursor.tokens,
                });
                let _ = store.save(&st);
                store.prune(std::time::Duration::from_secs(60 * 60 * 24 * 7));
            }
            Decision::Allow
        }
        HookInput::PreToolUse(p) => {
            let Some(ev) = claude::write_event_from_tool(&p.tool_name, &p.tool_input) else {
                return Decision::Allow;
            };
            // Resolve project dir from the hook's cwd when present.
            let proj = if p.cwd.as_os_str().is_empty() { project_dir.to_path_buf() } else { p.cwd.clone() };
            let cfg: Config = match config::load(&proj, global_path) {
                Ok(c) => c,
                Err(_) => return Decision::Allow,
            };
            let fail_open = cfg.defaults.fail_open;
            let combine = cfg.defaults.combine_freshness;

            let mut st = store.load(&p.session_id);
            refresh_state(&mut st, &p.transcript_path);
            let _ = store.save(&st);

            let now = Now { time: jiff::Timestamp::now(), turn: st.cursor.turn, tokens: st.cursor.tokens };

            for rule in &cfg.rules {
                let compiled = match rules::compile(rule) {
                    Ok(c) => c,
                    Err(_) => {
                        if fail_open { continue } else { return Decision::Deny { reason: format!("skillforcer: rule '{}' failed to compile", rule.name) } }
                    }
                };
                if compiled.matches(&ev) {
                    let res = freshness::evaluate(&compiled.def.requires, &st.loads, &now, combine);
                    if !res.satisfied {
                        return Decision::Deny { reason: message_for(&compiled.def, &ev, &res.reason) };
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
        let d = run_hook(&hook, &store, proj.path(), None);
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
            esc(&transcript), esc(proj.path()),
        );
        assert!(matches!(run_hook(&post, &store, proj.path(), None), Decision::Allow));

        let pre = format!(
            r#"{{"hook_event_name":"PreToolUse","session_id":"s","transcript_path":"{}","cwd":"{}","tool_name":"Write","tool_input":{{"file_path":"src/a.rs","content":"// hi"}}}}"#,
            esc(&transcript), esc(proj.path()),
        );
        assert!(matches!(run_hook(&pre, &store, proj.path(), None), Decision::Allow));
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
            esc(&transcript), esc(proj.path()),
        );
        assert!(matches!(run_hook(&pre, &store, proj.path(), None), Decision::Allow));
    }

    #[test]
    fn malformed_input_fails_open() {
        let statedir = tempfile::tempdir().unwrap();
        let store = Store::with_base(statedir.path().to_path_buf());
        assert!(matches!(run_hook("not json", &store, std::path::Path::new("."), None), Decision::Allow));
    }
}
