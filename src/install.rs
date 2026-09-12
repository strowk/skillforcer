use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub const PRE_MATCHER: &str = "Write|Edit|MultiEdit|NotebookEdit";
pub const POST_MATCHER: &str = "Skill";

#[derive(Debug, Clone, Copy)]
pub enum Target {
    Project,
    Local,
    User,
}

impl Target {
    pub fn settings_path(&self, project_dir: &Path) -> Result<PathBuf> {
        Ok(match self {
            Target::Project => project_dir.join(".claude").join("settings.json"),
            Target::Local => project_dir.join(".claude").join("settings.local.json"),
            Target::User => {
                let home = directories::BaseDirs::new().context("no home dir")?;
                home.home_dir().join(".claude").join("settings.json")
            }
        })
    }
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

fn event_entry(matcher: &str, exe: &str) -> Value {
    json!({
        "matcher": matcher,
        "hooks": [ { "type": "command", "command": format!("{exe} hook") } ]
    })
}

fn ensure_event(root: &mut Value, event: &str, matcher: &str, exe: &str) {
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
        arr.push(event_entry(matcher, exe));
    }
}

pub fn add_hooks(root: &mut Value, exe: &str) {
    if !root.is_object() {
        *root = json!({});
    }
    ensure_event(root, "PreToolUse", PRE_MATCHER, exe);
    ensure_event(root, "PostToolUse", POST_MATCHER, exe);
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

pub fn install(target: Target, exe: &str, project_dir: &Path, dry_run: bool) -> Result<String> {
    let path = target.settings_path(project_dir)?;
    let mut root = read_root(&path)?;
    add_hooks(&mut root, exe);
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

pub fn uninstall(target: Target, project_dir: &Path) -> Result<()> {
    let path = target.settings_path(project_dir)?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn add_hooks_is_idempotent() {
        let mut root = json!({});
        add_hooks(&mut root, "/usr/bin/skillforcer");
        add_hooks(&mut root, "/usr/bin/skillforcer");
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
        add_hooks(&mut root, "/x/skillforcer");
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
        add_hooks(&mut root, "/x/skillforcer");
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
}
