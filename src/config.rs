use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Combine {
    #[default]
    All,
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkillSet {
    Any(Vec<String>),
    All(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Windows {
    pub session: bool,
    pub minutes: Option<u64>,
    pub turns: Option<u64>,
    pub tokens: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requires {
    pub skills: SkillSet,
    pub windows: Windows,
}

#[derive(Debug, Clone)]
pub struct Defaults {
    pub fail_open: bool,
    pub combine_freshness: Combine,
}
impl Default for Defaults {
    fn default() -> Self {
        Self {
            fail_open: true,
            combine_freshness: Combine::All,
        }
    }
}

#[derive(Debug, Clone)]
pub struct RuleDef {
    pub name: String,
    pub enabled: bool,
    pub extends: Vec<String>,
    pub path: Vec<String>,
    pub content: Option<String>,
    pub requires: Requires,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Config {
    pub defaults: Defaults,
    pub rules: Vec<RuleDef>,
}

// ---- raw serde layer ----
#[derive(Debug, Deserialize, Default)]
struct RawConfig {
    #[serde(default)]
    defaults: RawDefaults,
    #[serde(default, rename = "rule")]
    rules: Vec<RawRule>,
}
#[derive(Debug, Deserialize, Default)]
struct RawDefaults {
    fail_open: Option<bool>,
    combine_freshness: Option<Combine>,
}
#[derive(Debug, Deserialize)]
struct RawRule {
    name: String,
    #[serde(default)]
    extends: Vec<String>,
    #[serde(default)]
    path: Vec<String>,
    content: Option<String>,
    requires: Option<RawRequires>,
    message: Option<String>,
    enabled: Option<bool>,
}
#[derive(Debug, Deserialize)]
struct RawRequires {
    any_skill: Option<Vec<String>>,
    all_skills: Option<Vec<String>>,
    #[serde(default)]
    session: bool,
    minutes: Option<u64>,
    turns: Option<u64>,
    tokens: Option<u64>,
}

fn convert_rule(r: RawRule) -> Result<RuleDef> {
    let enabled = r.enabled.unwrap_or(true);

    // A disabled rule is a tombstone: it only carries a name (plus whatever
    // else was written) and overrides its same-named inherited rule. It never
    // runs, so it skips skill/window validation and gets a placeholder Requires.
    if !enabled {
        return Ok(RuleDef {
            name: r.name,
            enabled: false,
            extends: r.extends,
            path: r.path,
            content: r.content,
            requires: Requires {
                skills: SkillSet::Any(Vec::new()),
                windows: Windows::default(),
            },
            message: r.message,
        });
    }

    let req = r
        .requires
        .ok_or_else(|| anyhow::anyhow!("rule '{}': requires is required", r.name))?;

    let skills = match (req.any_skill, req.all_skills) {
        (Some(_), Some(_)) => bail!("rule '{}': set only one of any_skill/all_skills", r.name),
        (Some(a), None) => SkillSet::Any(a),
        (None, Some(a)) => SkillSet::All(a),
        (None, None) => bail!("rule '{}': one of any_skill/all_skills is required", r.name),
    };
    let windows = Windows {
        session: req.session,
        minutes: req.minutes,
        turns: req.turns,
        tokens: req.tokens,
    };
    if !windows.session
        && windows.minutes.is_none()
        && windows.turns.is_none()
        && windows.tokens.is_none()
    {
        bail!(
            "rule '{}': at least one freshness window (session/minutes/turns/tokens) is required",
            r.name
        );
    }
    Ok(RuleDef {
        name: r.name,
        enabled: true,
        extends: r.extends,
        path: r.path,
        content: r.content,
        requires: Requires { skills, windows },
        message: r.message,
    })
}

pub fn parse_str(project_toml: &str, global_toml: Option<&str>) -> Result<Config> {
    let global: RawConfig = match global_toml {
        Some(t) => toml::from_str(t).context("parsing global config")?,
        None => RawConfig::default(),
    };
    let project: RawConfig = toml::from_str(project_toml).context("parsing project config")?;

    let defaults = Defaults {
        fail_open: project
            .defaults
            .fail_open
            .or(global.defaults.fail_open)
            .unwrap_or(true),
        combine_freshness: project
            .defaults
            .combine_freshness
            .or(global.defaults.combine_freshness)
            .unwrap_or_default(),
    };

    let mut rules: Vec<RuleDef> = Vec::new();
    for raw in global.rules.into_iter().chain(project.rules) {
        let converted = convert_rule(raw)?;
        if let Some(existing) = rules.iter_mut().find(|x| x.name == converted.name) {
            *existing = converted; // project (later) wins on same name
        } else {
            rules.push(converted);
        }
    }
    Ok(Config { defaults, rules })
}

pub fn load(project_dir: &Path, global_path: Option<&Path>) -> Result<Config> {
    let proj_path = project_dir.join(".skillforcer.toml");
    let project_toml = match std::fs::read_to_string(&proj_path) {
        Ok(s) => s,
        Err(_) => return Ok(Config::default()),
    };
    let global_toml = global_path.and_then(|p| std::fs::read_to_string(p).ok());
    parse_str(&project_toml, global_toml.as_deref())
}

pub fn resolve_extends(rule: &RuleDef) -> Result<RuleDef> {
    let mut out = rule.clone();
    let mut preset_paths: Vec<String> = Vec::new();
    for name in &rule.extends {
        let p = crate::presets::get(name)
            .ok_or_else(|| anyhow::anyhow!("rule '{}': unknown preset '{}'", rule.name, name))?;
        if out.content.is_none() {
            out.content = p.content.map(str::to_string);
        }
        preset_paths.extend(p.path.iter().map(|s| s.to_string()));
    }
    // preset paths first, then the rule's own (both apply as a set)
    let mut merged = preset_paths;
    merged.extend(rule.path.iter().cloned());
    out.path = merged;
    out.extends = Vec::new();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
        [defaults]
        fail_open = false
        combine_freshness = "all"

        [[rule]]
        name = "comments"
        path = ["src/**/*.rs"]
        content = "//"
        requires = { any_skill = ["tech-writing"], minutes = 15, turns = 40 }
        message = "load {skills}"
    "#;

    #[test]
    fn parses_rule_and_requires() {
        let cfg = parse_str(SAMPLE, None).unwrap();
        assert!(!cfg.defaults.fail_open);
        assert_eq!(cfg.rules.len(), 1);
        let r = &cfg.rules[0];
        assert!(matches!(&r.requires.skills, SkillSet::Any(v) if v == &["tech-writing"]));
        assert_eq!(r.requires.windows.minutes, Some(15));
        assert_eq!(r.requires.windows.turns, Some(40));
    }

    #[test]
    fn rejects_both_skill_keys() {
        let bad = r#"[[rule]]
            name = "x"
            requires = { any_skill = ["a"], all_skills = ["b"], session = true }"#;
        assert!(parse_str(bad, None).is_err());
    }

    #[test]
    fn rejects_no_freshness() {
        let bad = r#"[[rule]]
            name = "x"
            requires = { any_skill = ["a"] }"#;
        assert!(parse_str(bad, None).is_err());
    }

    #[test]
    fn project_overrides_global_defaults() {
        let global = r#"[defaults]
            fail_open = true"#;
        let project = r#"[defaults]
            fail_open = false"#;
        let cfg = parse_str(project, Some(global)).unwrap();
        assert!(!cfg.defaults.fail_open);
    }

    #[test]
    fn rejects_when_neither_skill_key_set() {
        let bad = r#"[[rule]]
            name = "x"
            requires = { session = true }"#;
        assert!(parse_str(bad, None).is_err());
    }

    #[test]
    fn project_rule_replaces_same_named_global_rule() {
        let global = r#"[[rule]]
            name = "dup"
            requires = { all_skills = ["g"], session = true }"#;
        let project = r#"[[rule]]
            name = "dup"
            requires = { any_skill = ["p"], session = true }"#;
        let cfg = parse_str(project, Some(global)).unwrap();
        assert_eq!(cfg.rules.len(), 1);
        assert!(matches!(cfg.rules[0].requires.skills, SkillSet::Any(_)));
    }

    #[test]
    fn disabled_rule_converts_without_requires() {
        // A pure-disable stanza needs only a name; validation is skipped.
        let raw: RawConfig = toml::from_str(
            r#"[[rule]]
            name = "x"
            enabled = false"#,
        )
        .unwrap();
        let rule = convert_rule(raw.rules.into_iter().next().unwrap()).unwrap();
        assert_eq!(rule.name, "x");
        assert!(!rule.enabled);
    }

    #[test]
    fn enabled_rule_missing_requires_still_errors() {
        let raw: RawConfig = toml::from_str(
            r#"[[rule]]
            name = "x""#,
        )
        .unwrap();
        assert!(convert_rule(raw.rules.into_iter().next().unwrap()).is_err());
    }

    #[test]
    fn enabled_defaults_true() {
        let cfg = parse_str(SAMPLE, None).unwrap();
        assert!(cfg.rules[0].enabled);
    }
}

#[cfg(test)]
mod extends_tests {
    use super::*;
    #[test]
    fn extends_fills_content_from_preset() {
        let rule = RuleDef {
            name: "c".into(),
            enabled: true,
            extends: vec!["code-comments".into()],
            path: vec!["src/**/*.rs".into()],
            content: None,
            requires: Requires {
                skills: SkillSet::Any(vec!["s".into()]),
                windows: Windows {
                    session: true,
                    ..Default::default()
                },
            },
            message: None,
        };
        let resolved = resolve_extends(&rule).unwrap();
        assert!(resolved.content.is_some());
        assert!(resolved.path.iter().any(|p| p == "src/**/*.rs"));
    }
    #[test]
    fn unknown_preset_errors() {
        let rule = RuleDef {
            name: "c".into(),
            enabled: true,
            extends: vec!["nope".into()],
            path: vec![],
            content: Some("x".into()),
            requires: Requires {
                skills: SkillSet::All(vec!["s".into()]),
                windows: Windows {
                    session: true,
                    ..Default::default()
                },
            },
            message: None,
        };
        assert!(resolve_extends(&rule).is_err());
    }
}
