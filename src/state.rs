use crate::model::{Cursor, SkillLoad};
use anyhow::Result;
use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionState {
    pub session_id: String,
    #[serde(default)]
    pub loads: Vec<SkillLoad>,
    #[serde(default)]
    pub cursor: Cursor,
}

pub struct Store {
    base: PathBuf,
}

impl Store {
    pub fn with_base(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn discover() -> Result<Self> {
        let dirs = directories::ProjectDirs::from("", "", "skillforcer")
            .ok_or_else(|| anyhow::anyhow!("cannot determine state directory"))?;
        let base = dirs
            .state_dir()
            .unwrap_or_else(|| dirs.data_dir())
            .join("sessions");
        Ok(Self { base })
    }

    fn path_for(&self, session_id: &str) -> PathBuf {
        let safe: String = session_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.base.join(format!("{safe}.json"))
    }

    pub fn load(&self, session_id: &str) -> SessionState {
        let path = self.path_for(session_id);
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<SessionState>(&s).ok())
            .unwrap_or(SessionState {
                session_id: session_id.to_string(),
                loads: Vec::new(),
                cursor: Cursor::default(),
            })
    }

    pub fn save(&self, st: &SessionState) -> Result<()> {
        std::fs::create_dir_all(&self.base)?;
        let path = self.path_for(&st.session_id);
        let json = serde_json::to_string_pretty(st)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn prune(&self, max_age: std::time::Duration) {
        let Ok(rd) = std::fs::read_dir(&self.base) else {
            return;
        };
        let now = std::time::SystemTime::now();
        for entry in rd.flatten() {
            if let Ok(meta) = entry.metadata()
                && let Ok(modified) = meta.modified()
                && now
                    .duration_since(modified)
                    .map(|d| d > max_age)
                    .unwrap_or(false)
            {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Cursor, SkillLoad};

    #[test]
    fn round_trips_state() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::with_base(dir.path().to_path_buf());
        let mut st = store.load("sess-1");
        assert!(st.loads.is_empty());
        st.loads.push(SkillLoad {
            skill: "x".into(),
            at: "2026-09-12T10:00:00Z".parse().unwrap(),
            turn: 2,
            tokens: 50,
        });
        st.cursor = Cursor {
            offset: 123,
            turn: 4,
            tokens: 80,
        };
        store.save(&st).unwrap();

        let again = store.load("sess-1");
        assert_eq!(again.loads.len(), 1);
        assert_eq!(again.cursor.offset, 123);
    }

    #[test]
    fn missing_session_is_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::with_base(dir.path().to_path_buf());
        let st = store.load("nope");
        assert_eq!(st.session_id, "nope");
        assert_eq!(st.cursor, Cursor::default());
    }
}
