use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteEvent {
    pub path: PathBuf,
    pub content: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SkillLoad {
    pub skill: String,
    pub at: jiff::Timestamp,
    pub turn: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Allow,
    Deny { reason: String },
}

#[derive(Debug, Clone, Copy, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct Cursor {
    pub offset: u64,
    pub turn: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Now {
    pub time: jiff::Timestamp,
    pub turn: u64,
    pub tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Codex,
}

impl Harness {
    pub fn from_flag(s: &str) -> Harness {
        if s.eq_ignore_ascii_case("codex") {
            Harness::Codex
        } else {
            Harness::Claude
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn harness_from_flag() {
        assert_eq!(Harness::from_flag("codex"), Harness::Codex);
        assert_eq!(Harness::from_flag("Codex"), Harness::Codex);
        assert_eq!(Harness::from_flag("claude"), Harness::Claude);
        assert_eq!(Harness::from_flag("anything-else"), Harness::Claude); // fail-open default
    }
}
