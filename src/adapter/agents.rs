//! Claude-specific agent-definition resolver.
//!
//! Answers one question for the enforcement gate (`commands.rs`, §3.3): does a
//! given `agent_type` have the `Skill` tool? Re-keying state per context means a
//! subagent starts with an empty record, so its first governed write is denied
//! until it loads the skill in its own context. A subagent that lacks the
//! `Skill` tool could never satisfy that denial, so it must be exempted.
//!
//! Resolution is fail-open: any uncertainty — no matching agent file, an unknown
//! built-in, malformed frontmatter — yields [`SkillAccess::Unresolved`], which
//! the gate treats as "allow". Only a positively confirmed `Skill` tool
//! enforces.

use serde::Deserialize;
use std::path::{Path, PathBuf};

/// Whether an agent can load skills, as far as resolution can determine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkillAccess {
    /// Resolved: the agent has the `Skill` tool. Enforce skillforcer's rules.
    Enforced,
    /// Resolved: the agent lacks the `Skill` tool. Exempt — allow the write.
    Exempt,
    /// Could not resolve the agent. Exempt — allow the write.
    Unresolved,
}

impl SkillAccess {
    /// True only when a `Skill` tool was positively confirmed.
    pub fn should_enforce(self) -> bool {
        matches!(self, SkillAccess::Enforced)
    }
}

/// The frontmatter fields the resolver reads from an agent `.md` file.
#[derive(Debug, Deserialize)]
struct Frontmatter {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    tools: Option<Tools>,
}

/// `tools:` may be a YAML sequence or a scalar (`*`, `all`, or a comma-separated
/// string), so it is parsed into either shape.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Tools {
    List(Vec<String>),
    Scalar(String),
}

/// The default directories searched for a project/user agent definition, in
/// precedence order: project agents override user agents.
pub fn default_dirs(cwd: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![cwd.join(".claude").join("agents")];
    if let Some(base) = directories::BaseDirs::new() {
        dirs.push(base.home_dir().join(".claude").join("agents"));
    }
    dirs
}

/// Resolves `agent_type` to its `Skill`-tool availability.
///
/// Searches `dirs` in order (project, then user); the first directory with a
/// matching agent file wins. A file is matched by filename stem first, then by
/// frontmatter `name:`. If no file matches anywhere, the static built-in table
/// is consulted. Anything unresolved — or a matched file with unreadable or
/// malformed frontmatter — yields [`SkillAccess::Unresolved`].
pub fn resolve(agent_type: &str, dirs: &[PathBuf]) -> SkillAccess {
    for dir in dirs {
        if let Some(path) = find_agent_file(dir, agent_type) {
            let Ok(content) = std::fs::read_to_string(&path) else {
                return SkillAccess::Unresolved;
            };
            return match parse_frontmatter(&content) {
                Some(fm) if frontmatter_has_skill(&fm) => SkillAccess::Enforced,
                Some(_) => SkillAccess::Exempt,
                None => SkillAccess::Unresolved,
            };
        }
    }
    builtin(agent_type)
}

/// The static built-in table (§3.4). The default writing agents have the
/// `Skill` tool; `statusline-setup` does not. Agents with no write tool
/// (`Explore`, `Plan`, `claude-code-guide`) never reach the guard, so they need
/// no entry and fall through to unresolved.
fn builtin(agent_type: &str) -> SkillAccess {
    match agent_type {
        "general-purpose" | "claude" | "fork" => SkillAccess::Enforced,
        "statusline-setup" => SkillAccess::Exempt,
        _ => SkillAccess::Unresolved,
    }
}

/// Finds the agent file in `dir` matching `agent_type`: by filename stem
/// (`<agent_type>.md`) first, then by scanning `.md` files for a frontmatter
/// `name:` equal to `agent_type`.
fn find_agent_file(dir: &Path, agent_type: &str) -> Option<PathBuf> {
    let direct = dir.join(format!("{agent_type}.md"));
    if direct.is_file() {
        return Some(direct);
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("md") {
            continue;
        }
        if let Ok(content) = std::fs::read_to_string(&path)
            && let Some(fm) = parse_frontmatter(&content)
            && fm.name.as_deref() == Some(agent_type)
        {
            return Some(path);
        }
    }
    None
}

/// Extracts the leading `---`-delimited YAML frontmatter block and parses it.
/// Returns `None` when there is no well-formed block or the YAML does not parse.
fn parse_frontmatter(content: &str) -> Option<Frontmatter> {
    let block = extract_frontmatter_block(content)?;
    serde_yaml_ng::from_str::<Frontmatter>(&block).ok()
}

/// Returns the YAML text between the opening `---` and the next `---` line, or
/// `None` if the content does not open with a frontmatter delimiter or never
/// closes it.
fn extract_frontmatter_block(content: &str) -> Option<String> {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let mut lines = content.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    let mut yaml = String::new();
    for line in lines {
        if line.trim_end() == "---" {
            return Some(yaml);
        }
        yaml.push_str(line);
        yaml.push('\n');
    }
    None
}

/// Whether the parsed frontmatter grants the `Skill` tool: an omitted `tools`
/// (or `*`/`all`) means every tool; an explicit list grants it only when it
/// contains `Skill`.
fn frontmatter_has_skill(fm: &Frontmatter) -> bool {
    match &fm.tools {
        None => true,
        Some(Tools::Scalar(s)) => {
            let t = s.trim();
            if t == "*" || t.eq_ignore_ascii_case("all") {
                true
            } else {
                t.split(',').any(|item| item.trim() == "Skill")
            }
        }
        Some(Tools::List(items)) => items.iter().any(|item| item.trim() == "Skill"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_agent(dir: &Path, filename: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let mut f = std::fs::File::create(dir.join(filename)).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn project_file_with_skill_in_tools_enforces() {
        let d = tempfile::tempdir().unwrap();
        write_agent(
            d.path(),
            "worker.md",
            "---\nname: worker\ntools: [Read, Edit, Skill, Bash]\n---\nbody\n",
        );
        assert_eq!(
            resolve("worker", &[d.path().to_path_buf()]),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn project_file_without_skill_is_exempt() {
        let d = tempfile::tempdir().unwrap();
        write_agent(
            d.path(),
            "scribe.md",
            "---\nname: scribe\ntools: [Read, Edit, Bash]\n---\nbody\n",
        );
        assert_eq!(
            resolve("scribe", &[d.path().to_path_buf()]),
            SkillAccess::Exempt
        );
    }

    #[test]
    fn tools_omitted_means_all_tools_enforces() {
        let d = tempfile::tempdir().unwrap();
        write_agent(d.path(), "any.md", "---\nname: any\n---\nbody\n");
        assert_eq!(
            resolve("any", &[d.path().to_path_buf()]),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn tools_star_means_all_tools_enforces() {
        let d = tempfile::tempdir().unwrap();
        write_agent(d.path(), "star.md", "---\nname: star\ntools: '*'\n---\n");
        assert_eq!(
            resolve("star", &[d.path().to_path_buf()]),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn tools_all_keyword_enforces() {
        let d = tempfile::tempdir().unwrap();
        write_agent(d.path(), "every.md", "---\nname: every\ntools: all\n---\n");
        assert_eq!(
            resolve("every", &[d.path().to_path_buf()]),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn comma_separated_tools_scalar_is_parsed() {
        let d = tempfile::tempdir().unwrap();
        write_agent(
            d.path(),
            "csv.md",
            "---\nname: csv\ntools: Read, Skill, Bash\n---\n",
        );
        assert_eq!(
            resolve("csv", &[d.path().to_path_buf()]),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn matches_by_frontmatter_name_when_stem_differs() {
        let d = tempfile::tempdir().unwrap();
        write_agent(
            d.path(),
            "file-stem.md",
            "---\nname: real-name\ntools: [Skill]\n---\n",
        );
        assert_eq!(
            resolve("real-name", &[d.path().to_path_buf()]),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn project_dir_overrides_user_dir() {
        let proj = tempfile::tempdir().unwrap();
        let user = tempfile::tempdir().unwrap();
        write_agent(
            proj.path(),
            "dup.md",
            "---\nname: dup\ntools: [Skill]\n---\n",
        );
        write_agent(
            user.path(),
            "dup.md",
            "---\nname: dup\ntools: [Read]\n---\n",
        );
        assert_eq!(
            resolve(
                "dup",
                &[proj.path().to_path_buf(), user.path().to_path_buf()]
            ),
            SkillAccess::Enforced
        );
    }

    #[test]
    fn builtin_general_purpose_enforces() {
        assert_eq!(resolve("general-purpose", &[]), SkillAccess::Enforced);
        assert_eq!(resolve("claude", &[]), SkillAccess::Enforced);
        assert_eq!(resolve("fork", &[]), SkillAccess::Enforced);
    }

    #[test]
    fn builtin_statusline_setup_is_exempt() {
        assert_eq!(resolve("statusline-setup", &[]), SkillAccess::Exempt);
    }

    #[test]
    fn unknown_agent_is_unresolved() {
        assert_eq!(resolve("nope-not-here", &[]), SkillAccess::Unresolved);
    }

    #[test]
    fn malformed_frontmatter_is_unresolved() {
        let d = tempfile::tempdir().unwrap();
        // Matched by stem, but the frontmatter block never closes.
        write_agent(d.path(), "broken.md", "---\nname: broken\ntools: [\n");
        assert_eq!(
            resolve("broken", &[d.path().to_path_buf()]),
            SkillAccess::Unresolved
        );
    }

    #[test]
    fn should_enforce_only_for_enforced() {
        assert!(SkillAccess::Enforced.should_enforce());
        assert!(!SkillAccess::Exempt.should_enforce());
        assert!(!SkillAccess::Unresolved.should_enforce());
    }
}
