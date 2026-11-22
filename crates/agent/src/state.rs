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

/// One site the agent knows about. `Debug` redacts the bcrypt hash.
#[derive(Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SiteRecord {
    /// Recorded by `SetRoute`, cleared by `RemoveRoute`: reconcile
    /// rebuilds `setu-site-<short>` from it.
    #[serde(default)]
    pub domain: Option<String>,
    #[serde(default)]
    pub port: u16,
    /// `(user, bcrypt hash)` recorded by `SetRoute`; reconcile puts it back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basic_auth: Option<(String, String)>,
    /// The site's `www.` → apex redirect is on. Off when absent (a state
    /// file from before 0.1.7, a command from an older control plane): an
    /// upgrade changes no site until its setting is sent.
    #[serde(default)]
    pub www_redirect: bool,
    /// Extra hostnames recorded by `SetRoute` (none for a state file from
    /// before 0.1.7).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<setu_proto::Alias>,
    /// Search engines are asked to stay away (off for a state file from
    /// before 0.1.7).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub noindex: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub db: Option<String>,
}

impl std::fmt::Debug for SiteRecord {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteRecord")
            .field("domain", &self.domain)
            .field("port", &self.port)
            .field(
                "basic_auth",
                &self
                    .basic_auth
                    .as_ref()
                    .map(|(user, _)| (user, "[redacted]")),
            )
            .field("www_redirect", &self.www_redirect)
            .field("aliases", &self.aliases)
            .field("noindex", &self.noindex)
            .field("version", &self.version)
            .field("db", &self.db)
            .finish()
    }
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
    /// The `www.` name `short`'s route redirects to its apex domain: none
    /// when the site opted out, its domain is not an apex, or another site
    /// on this server serves that very name (first-match routing would let
    /// the redirect shadow the other site).
    pub fn www_host(&self, short: &str) -> Option<String> {
        let site = self.sites.get(short)?;
        if !site.www_redirect {
            return None;
        }
        let www = crate::caddy::www_name(site.domain.as_deref()?)?;
        // An explicit alias of that name replaces the automatic redirect.
        if site.aliases.iter().any(|a| a.host == www) {
            return None;
        }
        let taken = self
            .sites
            .iter()
            .any(|(other, r)| other != short && r.domain.as_deref() == Some(www.as_str()));
        (!taken).then_some(www)
    }

    /// The aliases of `short` that may be routed: not the site's own
    /// domain, not a name another site serves or aliases (first-match
    /// routing would let one site shadow the other), each host once.
    pub fn live_aliases(&self, short: &str) -> Vec<setu_proto::Alias> {
        let Some(site) = self.sites.get(short) else {
            return Vec::new();
        };
        let mut seen = std::collections::BTreeSet::new();
        site.aliases
            .iter()
            .filter(|a| crate::caddy::valid_domain(&a.host))
            .filter(|a| site.domain.as_deref() != Some(a.host.as_str()))
            .filter(|a| {
                !self.sites.iter().any(|(other, r)| {
                    other != short
                        && (r.domain.as_deref() == Some(a.host.as_str())
                            || r.aliases.iter().any(|o| o.host == a.host))
                })
            })
            .filter(|a| seen.insert(a.host.clone()))
            .cloned()
            .collect()
    }

    /// The other sites whose `www.` name is `domain`: their redirect
    /// appears or disappears when a site starts or stops serving `domain`.
    pub fn www_neighbours(&self, short: &str, domain: &str) -> Vec<String> {
        self.sites
            .iter()
            .filter(|(other, r)| {
                other.as_str() != short
                    && r.domain
                        .as_deref()
                        .and_then(crate::caddy::www_name)
                        .is_some_and(|w| w == domain)
            })
            .map(|(other, _)| other.clone())
            .collect()
    }

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
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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

/// The hostnames of `short`'s live `serve` aliases (the site route answers
/// on them too).
pub fn serve_hosts(state: &State, short: &str) -> Vec<String> {
    state
        .live_aliases(short)
        .into_iter()
        .filter(|a| a.mode == setu_proto::AliasMode::Serve)
        .map(|a| a.host)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_prints_the_basic_auth_hash() {
        let hash = format!("$2b$12${}", "a".repeat(53));
        let mut state = State::default();
        state.sites.insert(
            "abc".into(),
            SiteRecord {
                basic_auth: Some(("alice".into(), hash.clone())),
                ..SiteRecord::default()
            },
        );
        let shown = format!("{state:?}");
        assert!(
            shown.contains("alice") && shown.contains("[redacted]"),
            "{shown}"
        );
        assert!(!shown.contains(&hash) && !shown.contains("$2b$"), "{shown}");
    }

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
