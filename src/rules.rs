use crate::config::{RuleDef, resolve_extends};
use crate::model::WriteEvent;
use anyhow::{Context, Result};
use globset::{Glob, GlobSet, GlobSetBuilder};
use regex::Regex;

pub struct CompiledRule {
    pub def: RuleDef,
    pub globset: Option<GlobSet>,
    pub content: Option<Regex>,
}

pub fn compile(rule: &RuleDef) -> Result<CompiledRule> {
    let def = resolve_extends(rule)?;
    let globset = if def.path.is_empty() {
        None
    } else {
        let mut b = GlobSetBuilder::new();
        for p in &def.path {
            b.add(Glob::new(p).with_context(|| format!("rule '{}': bad glob '{}'", def.name, p))?);
        }
        Some(b.build()?)
    };
    let content = match &def.content {
        Some(re) => {
            Some(Regex::new(re).with_context(|| format!("rule '{}': bad regex", def.name))?)
        }
        None => None,
    };
    Ok(CompiledRule {
        def,
        globset,
        content,
    })
}

impl CompiledRule {
    pub fn matches(&self, ev: &WriteEvent) -> bool {
        let path_ok = match &self.globset {
            Some(gs) => gs.is_match(&ev.path),
            None => true,
        };
        if !path_ok {
            return false;
        }
        match &self.content {
            Some(re) => re.is_match(&ev.content),
            None => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Requires, RuleDef, SkillSet, Windows};
    use crate::model::WriteEvent;
    use std::path::PathBuf;

    fn rule(path: Vec<&str>, content: Option<&str>) -> RuleDef {
        RuleDef {
            name: "t".into(),
            extends: vec![],
            path: path.into_iter().map(String::from).collect(),
            content: content.map(String::from),
            requires: Requires {
                skills: SkillSet::Any(vec!["s".into()]),
                windows: Windows {
                    session: true,
                    ..Default::default()
                },
            },
            message: None,
        }
    }
    fn ev(path: &str, content: &str) -> WriteEvent {
        WriteEvent {
            path: PathBuf::from(path),
            content: content.into(),
        }
    }

    #[test]
    fn matches_path_and_content() {
        let c = compile(&rule(vec!["src/**/*.rs"], Some("//"))).unwrap();
        assert!(c.matches(&ev("src/a/b.rs", "// hi")));
        assert!(!c.matches(&ev("src/a/b.rs", "no comment")));
        assert!(!c.matches(&ev("docs/x.md", "// hi")));
    }

    #[test]
    fn empty_path_matches_any() {
        let c = compile(&rule(vec![], Some("TODO"))).unwrap();
        assert!(c.matches(&ev("anywhere.txt", "TODO fix")));
    }
}
