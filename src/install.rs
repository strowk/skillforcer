use crate::model::Harness;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const PRE_MATCHER: &str = "Write|Edit|MultiEdit|NotebookEdit";
pub const POST_MATCHER: &str = "Skill";
pub const PRE_MATCHER_CODEX: &str = "apply_patch|Edit|Write|Bash";
pub const POST_MATCHER_CODEX: &str = "Bash|apply_patch";

pub const LOCAL_CONFIG_NAME: &str = ".skillforcer.local.toml";

pub const CODEX_TRUST_NOTE: &str = "\
IMPORTANT: Codex requires hooks to be trusted before they run.\n\
Open Codex and run /hooks to review and approve the skillforcer hook.\n\
Until then, skillforcer enforces NOTHING — and in `codex exec` automation\n\
untrusted hooks are skipped silently.";

#[derive(Debug, Clone, Copy)]
pub enum Target {
    Project,
    Local,
    User,
}

impl Target {
    pub fn settings_path(&self, project_dir: &Path, harness: Harness) -> Result<PathBuf> {
        Ok(match (harness, self) {
            (Harness::Claude, Target::Project) => project_dir.join(".claude").join("settings.json"),
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
}

/// Detects harnesses in use by their marker directories, defaulting to Claude
/// when neither is present.
pub fn detect_harnesses(project_dir: &Path) -> Vec<Harness> {
    let mut found = Vec::new();
    if project_dir.join(".claude").is_dir() {
        found.push(Harness::Claude);
    }
    if project_dir.join(".codex").is_dir() || project_dir.join(".agents").join("skills").is_dir() {
        found.push(Harness::Codex);
    }
    if found.is_empty() {
        found.push(Harness::Claude);
    }
    found
}

fn is_ours(entry: &Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|arr| {
            arr.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains("skillforcer"))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

fn event_entry(matcher: &str, cmd: &str) -> Value {
    json!({
        "matcher": matcher,
        "hooks": [ { "type": "command", "command": cmd } ]
    })
}

fn ensure_event(root: &mut Value, event: &str, matcher: &str, cmd: &str) {
    let map = root.as_object_mut().unwrap();
    let hooks = map.entry("hooks").or_insert_with(|| json!({}));
    if !hooks.is_object() {
        *hooks = json!({});
    }
    let arr = hooks
        .as_object_mut()
        .expect("just ensured object")
        .entry(event)
        .or_insert_with(|| json!([]));
    if !arr.is_array() {
        *arr = json!([]);
    }
    let arr = arr.as_array_mut().expect("just ensured array");
    // idempotent: skip if one of ours with this matcher already exists
    let exists = arr
        .iter()
        .any(|e| is_ours(e) && e.get("matcher").and_then(|m| m.as_str()) == Some(matcher));
    if !exists {
        arr.push(event_entry(matcher, cmd));
    }
}

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

pub fn remove_hooks(root: &mut Value) {
    let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    for event in ["PreToolUse", "PostToolUse"] {
        if let Some(arr) = hooks.get_mut(event).and_then(|a| a.as_array_mut()) {
            arr.retain(|e| !is_ours(e));
        }
    }
}

fn read_root(path: &Path) -> Result<Value> {
    match std::fs::read_to_string(path) {
        Ok(s) if !s.trim().is_empty() => {
            Ok(serde_json::from_str(&s).context("parsing settings.json")?)
        }
        _ => Ok(json!({})),
    }
}

fn write_root(path: &Path, root: &Value) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(root)?)?;
    Ok(())
}

pub fn install(
    target: Target,
    exe: &str,
    project_dir: &Path,
    dry_run: bool,
    harness: Harness,
) -> Result<String> {
    let path = target.settings_path(project_dir, harness)?;
    let mut root = read_root(&path)?;
    add_hooks(&mut root, exe, harness);
    let pretty = serde_json::to_string_pretty(&root)?;
    if dry_run {
        return Ok(format!(
            "[dry-run] would write {}:\n{}",
            path.display(),
            pretty
        ));
    }
    write_root(&path, &root)?;
    Ok(format!("installed hooks into {}", path.display()))
}

pub fn uninstall(target: Target, project_dir: &Path, harness: Harness) -> Result<()> {
    let path = target.settings_path(project_dir, harness)?;
    let mut root = read_root(&path)?;
    remove_hooks(&mut root);
    write_root(&path, &root)
}

const SCAFFOLD: &str = r#"# skillforcer configuration.
# Docs: run `skillforcer list-presets` to see bundled presets.

[defaults]
fail_open = true          # a skillforcer error never blocks a write
combine_freshness = "all" # when multiple windows are set, all must hold

# Example: force the tech-writing skill before comment edits in Rust/TS.
# [[rule]]
# name = "comments-need-tech-writing"
# extends = ["code-comments"]
# path = ["src/**/*.rs", "**/*.ts"]
# requires = { any_skill = ["tech-writing:technical-writing"], minutes = 15 }
# message = "Editing comments in {file}. Load {skills} first — context may have rotted."
"#;

pub fn scaffold_config(project_dir: &Path) -> Result<bool> {
    let path = project_dir.join(".skillforcer.toml");
    if path.exists() {
        return Ok(false);
    }
    std::fs::write(&path, SCAFFOLD)?;
    Ok(true)
}

/// Ensure the personal `.skillforcer.local.toml` is git-ignored. Creates
/// `.gitignore` if absent, appends the entry only when missing. Idempotent;
/// returns true if it added the entry.
pub fn ensure_gitignored(project_dir: &Path) -> Result<bool> {
    let path = project_dir.join(".gitignore");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == LOCAL_CONFIG_NAME) {
        return Ok(false);
    }
    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(LOCAL_CONFIG_NAME);
    out.push('\n');
    std::fs::write(&path, out)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn add_hooks_is_idempotent() {
        let mut root = json!({});
        add_hooks(&mut root, "/usr/bin/skillforcer", Harness::Claude);
        add_hooks(&mut root, "/usr/bin/skillforcer", Harness::Claude);
        let pre = &root["hooks"]["PreToolUse"];
        assert_eq!(pre.as_array().unwrap().len(), 1);
        let post = &root["hooks"]["PostToolUse"];
        assert_eq!(post.as_array().unwrap().len(), 1);
        // matcher + command present
        assert_eq!(pre[0]["matcher"], "Write|Edit|MultiEdit|NotebookEdit");
        assert!(
            pre[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("skillforcer")
        );
    }

    #[test]
    fn add_hooks_coerces_malformed_hooks() {
        let mut root = json!({"hooks": "garbage"});
        add_hooks(&mut root, "/x/skillforcer", Harness::Claude);
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
    }

    #[test]
    fn remove_hooks_drops_only_ours() {
        let mut root = json!({
            "hooks": { "PreToolUse": [
                {"matcher":"Bash","hooks":[{"type":"command","command":"other-tool"}]}
            ]}
        });
        add_hooks(&mut root, "/x/skillforcer", Harness::Claude);
        remove_hooks(&mut root);
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["hooks"][0]["command"], "other-tool");
    }

    #[test]
    fn scaffold_creates_then_skips() {
        let proj = tempfile::tempdir().unwrap();
        assert!(scaffold_config(proj.path()).unwrap());
        assert!(!scaffold_config(proj.path()).unwrap());
        assert!(proj.path().join(".skillforcer.toml").exists());
    }

    #[test]
    fn codex_add_hooks_uses_codex_matchers_and_flag() {
        let mut root = json!({});
        add_hooks(&mut root, "/usr/bin/skillforcer", Harness::Codex);
        add_hooks(&mut root, "/usr/bin/skillforcer", Harness::Codex);
        let pre = root["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(pre.len(), 1);
        assert_eq!(pre[0]["matcher"], "apply_patch|Edit|Write|Bash");
        assert!(
            pre[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .ends_with("hook --harness codex")
        );
        let post = root["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post[0]["matcher"], "Bash|apply_patch");
    }

    #[test]
    fn codex_settings_paths() {
        let proj = std::path::Path::new("/proj");
        let p = Target::Project.settings_path(proj, Harness::Codex).unwrap();
        assert!(p.ends_with(std::path::Path::new(".codex/hooks.json")));
        assert!(Target::Local.settings_path(proj, Harness::Codex).is_err());
        let c = Target::Project
            .settings_path(proj, Harness::Claude)
            .unwrap();
        assert!(c.ends_with(std::path::Path::new(".claude/settings.json")));
    }

    #[test]
    fn detect_harnesses_by_marker_dirs() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(detect_harnesses(t.path()), vec![Harness::Claude]); // fallback
        std::fs::create_dir_all(t.path().join(".codex")).unwrap();
        assert_eq!(detect_harnesses(t.path()), vec![Harness::Codex]);
        std::fs::create_dir_all(t.path().join(".claude")).unwrap();
        assert_eq!(
            detect_harnesses(t.path()),
            vec![Harness::Claude, Harness::Codex]
        );
        let t2 = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t2.path().join(".agents").join("skills")).unwrap();
        assert_eq!(detect_harnesses(t2.path()), vec![Harness::Codex]);
    }

    #[test]
    fn codex_install_and_uninstall_roundtrip() {
        let proj = tempfile::tempdir().unwrap();
        let msg = install(
            Target::Project,
            "/x/skillforcer",
            proj.path(),
            false,
            Harness::Codex,
        )
        .unwrap();
        assert!(msg.contains("hooks.json"));
        let written =
            std::fs::read_to_string(proj.path().join(".codex").join("hooks.json")).unwrap();
        assert!(written.contains("--harness codex"));
        uninstall(Target::Project, proj.path(), Harness::Codex).unwrap();
        let after = std::fs::read_to_string(proj.path().join(".codex").join("hooks.json")).unwrap();
        assert!(!after.contains("skillforcer"));
    }

    #[test]
    fn ensure_gitignored_creates_and_is_idempotent() {
        let proj = tempfile::tempdir().unwrap();
        assert!(ensure_gitignored(proj.path()).unwrap());
        assert!(!ensure_gitignored(proj.path()).unwrap());
        let body = std::fs::read_to_string(proj.path().join(".gitignore")).unwrap();
        assert_eq!(
            body.lines().filter(|l| l.trim() == ".skillforcer.local.toml").count(),
            1
        );
    }

    #[test]
    fn ensure_gitignored_appends_to_existing() {
        let proj = tempfile::tempdir().unwrap();
        std::fs::write(proj.path().join(".gitignore"), "/target\n").unwrap();
        assert!(ensure_gitignored(proj.path()).unwrap());
        let body = std::fs::read_to_string(proj.path().join(".gitignore")).unwrap();
        assert!(body.contains("/target"));
        assert!(body.contains(".skillforcer.local.toml"));
    }
}
