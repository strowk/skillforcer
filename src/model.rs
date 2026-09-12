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
