//! `state.json`: the sites this host knows about and the last command
//! results (spec §5).
//!
//! The control plane is the source of truth; this file lets the agent
//! rebuild Caddy routes ([`crate::reconcile`]) after a restart and
//! dedupe a command delivered twice: every result is persisted here
//! **before** it is posted, so a redelivered id returns the cached
//! result without executing again.
//!
//! [`StateStore`] is the one in-process owner: commands run
//! concurrently (one task each), so every change is a short critical
//! section that mutates and saves atomically (temp + rename, 0600).

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use setu_proto::CommandResult;

use crate::config::atomic_write;

/// How many results `recent` keeps (spec §5: "last 1000").
pub const RECENT_CAP: usize = 1000;

/// One site the agent knows about.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteRecord {
    /// Recorded by `SetRoute`, cleared by `RemoveRoute`: reconcile
    /// rebuilds `setu-site-<short>` from it.
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub db: Option<String>,
}

/// One persisted command result; `posted` once the control plane
/// acknowledged it. Results of read-only commands (`status`,
/// `fetch_logs`, `inventory`: re-running them is harmless, and a log
/// page can be megabytes) are dropped once posted.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RecentResult {
    pub id: String,
    pub result: CommandResult,
    #[serde(default)]
    pub posted: bool,
    #[serde(default)]
    pub read_only: bool,
}

/// The whole state file.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct State {
    #[serde(default)]
    pub sites: BTreeMap<String, SiteRecord>,
    #[serde(default)]
    pub recent: VecDeque<RecentResult>,
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

impl State {
    /// Load from `path`; a missing file is empty state (fresh agent).
    pub fn load(path: &Path) -> Result<Self, StateError> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| StateError::Parse(path.display().to_string(), e.to_string())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(StateError::Read(path.display().to_string(), e.to_string())),
        }
    }

    /// Save atomically (temp file created 0600 + rename).
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let text = serde_json::to_string_pretty(self)
            .map_err(|e| StateError::Write(path.display().to_string(), e.to_string()))?;
        atomic_write(path, text.as_bytes())
            .map_err(|e| StateError::Write(path.display().to_string(), e.to_string()))
    }

    /// The cached result for `id`, if any.
    pub fn cached(&self, id: &str) -> Option<&CommandResult> {
        self.recent.iter().find(|r| r.id == id).map(|r| &r.result)
    }

    /// Record `result` (replacing an older entry for the same id) and
    /// drop the oldest beyond [`RECENT_CAP`].
    pub fn push_result(&mut self, result: CommandResult, read_only: bool) {
        self.recent.retain(|r| r.id != result.id);
        self.recent.push_back(RecentResult {
            id: result.id.clone(),
            result,
            posted: false,
            read_only,
        });
        while self.recent.len() > RECENT_CAP {
            self.recent.pop_front();
        }
    }
}

/// The in-process owner of `state.json`.
#[derive(Debug)]
pub struct StateStore {
    path: PathBuf,
    inner: Mutex<State>,
}

impl StateStore {
    /// Load `path` (missing = empty). An unreadable or corrupt file is
    /// logged and replaced by empty state on the next save: the control
    /// plane replays what it needs, refusing to start would not help.
    pub fn open(path: &Path) -> Self {
        let state = State::load(path).unwrap_or_else(|e| {
            tracing::error!(error = %e, "state.json unreadable; starting empty");
            State::default()
        });
        Self {
            path: path.to_path_buf(),
            inner: Mutex::new(state),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A copy of the current state.
    pub fn snapshot(&self) -> State {
        self.lock().clone()
    }

    /// Read without saving.
    pub fn read<T>(&self, f: impl FnOnce(&State) -> T) -> T {
        f(&self.lock())
    }

    /// Mutate and save atomically while holding the lock.
    pub fn update<T>(&self, f: impl FnOnce(&mut State) -> T) -> Result<T, StateError> {
        let mut st = self.lock();
        let out = f(&mut st);
        st.save(&self.path)?;
        Ok(out)
    }

    /// Update one site's record (created when absent).
    pub fn update_site(
        &self,
        short: &str,
        f: impl FnOnce(&mut SiteRecord),
    ) -> Result<(), StateError> {
        self.update(|st| f(st.sites.entry(short.to_string()).or_default()))
    }

    /// Forget one site.
    pub fn remove_site(&self, short: &str) -> Result<(), StateError> {
        self.update(|st| {
            st.sites.remove(short);
        })
    }

    pub fn cached(&self, id: &str) -> Option<CommandResult> {
        self.read(|st| st.cached(id).cloned())
    }

    /// Persist a result (before it is posted).
    pub fn record_result(&self, result: &CommandResult, read_only: bool) -> Result<(), StateError> {
        self.update(|st| st.push_result(result.clone(), read_only))
    }

    /// Mark `id` acknowledged by the control plane (a read-only result
    /// is dropped instead).
    pub fn mark_posted(&self, id: &str) -> Result<(), StateError> {
        self.update(|st| {
            if let Some(i) = st.recent.iter().position(|r| r.id == id) {
                if st.recent[i].read_only {
                    st.recent.remove(i);
                } else {
                    st.recent[i].posted = true;
                }
            }
        })
    }

    /// Results never acknowledged, oldest first.
    pub fn unposted(&self) -> Vec<CommandResult> {
        self.read(|st| {
            st.recent
                .iter()
                .filter(|r| !r.posted)
                .map(|r| r.result.clone())
                .collect()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_state() {
        let s = State::load(Path::new("/nonexistent-setu-state.json")).expect("load");
        assert!(s.sites.is_empty() && s.recent.is_empty());
    }

    #[test]
    fn pre_ws7_state_without_recent_or_domain_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("state.json");
        std::fs::write(
            &path,
            r#"{"sites":{"abcd1234":{"port":20001,"db":null,"version":"1"}}}"#,
        )
        .expect("write");
        let s = State::load(&path).expect("load");
        assert_eq!(s.sites["abcd1234"].port, 20001);
        assert_eq!(s.sites["abcd1234"].domain, None);
    }
}
