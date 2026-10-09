//! `state.json`: the sites this host knows about.
//!
//! The control plane is the source of truth; this file lets the agent
//! reconcile systemd and Postgres against it after a restart.

use std::collections::BTreeMap;
use std::path::Path;

use crate::config::atomic_write;

/// One site the agent knows about.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteEntry {
    pub port: u16,
    #[serde(default)]
    pub db: Option<String>,
    #[serde(default)]
    pub version: String,
}

/// The whole state file.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StateFile {
    #[serde(default)]
    pub sites: BTreeMap<String, SiteEntry>,
}

/// Errors reading or writing the state file.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error("cannot read state {0}: {1}")]
    Read(String, String),
    #[error("cannot parse state {0}: {1}")]
    Parse(String, String),
    #[error("cannot write state {0}: {1}")]
    Write(String, String),
}

impl StateFile {
    /// Load from `path`; a missing file is empty state (fresh agent).
    pub fn load(path: &Path) -> Result<Self, StateError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| StateError::Parse(path.display().to_string(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StateError::Read(path.display().to_string(), e.to_string())),
        }
    }

    /// Save atomically (temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| StateError::Write(path.display().to_string(), e.to_string()))?;
        atomic_write(path, text.as_bytes())
            .map_err(|e| StateError::Write(path.display().to_string(), e.to_string()))?;
        Ok(())
    }

    /// Record or update one site entry, then save.
    pub fn upsert(
        &mut self,
        site_id: &str,
        entry: SiteEntry,
        path: &Path,
    ) -> Result<(), StateError> {
        self.sites.insert(site_id.to_string(), entry);
        self.save(path)
    }

    /// Drop one site entry, then save.
    pub fn remove(&mut self, site_id: &str, path: &Path) -> Result<(), StateError> {
        self.sites.remove(site_id);
        self.save(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_state() {
        let s = StateFile::load(Path::new("/nonexistent-setu-state.json")).expect("load");
        assert!(s.sites.is_empty());
    }

    #[test]
    fn upsert_remove_round_trip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        let mut s = StateFile::load(&path).expect("empty");
        s.upsert(
            "site1",
            SiteEntry {
                port: 20001,
                db: Some("site_abcd".to_string()),
                version: "0.1.0".to_string(),
            },
            &path,
        )
        .expect("upsert");
        let back = StateFile::load(&path).expect("reload");
        assert_eq!(back.sites["site1"].port, 20001);
        let mut back = back;
        back.remove("site1", &path).expect("remove");
        let empty = StateFile::load(&path).expect("reload");
        assert!(empty.sites.is_empty());
    }
}
