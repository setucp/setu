//! Shared handler state: database, sealer, config, clock, agent client,
//! the command notifier and the in-memory rate-limit tables.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use setu_common::{AppError, Config, Sealer};
use setu_db::Db;
use setu_jobs::{AgentClient, Clock};

use crate::notifier::Notifier;

/// Failed logins per source IP within [`RATE_LIMIT_WINDOW`]; at
/// [`MAX_LOGIN_FAILURES`] the next attempt is rejected with 429.
const MAX_LOGIN_FAILURES: usize = 10;
const RATE_LIMIT_WINDOW: Duration = Duration::from_mins(15);
/// Hard cap on tracked IPs so a distributed scan cannot grow the map
/// without bound; the oldest entry is evicted first.
const MAX_LOGIN_IPS: usize = 10_000;
/// Long-poll ceiling per server on `GET /agent/v1/commands` (spec §7).
/// A well-behaved agent holds one `?wait=30` request open at a time
/// (~2 polls/min); 600/min only bites a spinning client, which gets 429.
const MAX_AGENT_POLLS: usize = 600;
const AGENT_RATE_WINDOW: Duration = Duration::from_secs(60);
/// Hard cap on tracked servers, same eviction policy as the login table.
const MAX_AGENT_SERVERS: usize = 10_000;

/// State shared by every handler. `Clone` is cheap (all `Arc` inside bar
/// `Config`, which is small); `Debug` redacts the sealer and secret key
/// (via their own `Debug` impls) and never prints tokens.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub sealer: Sealer,
    pub config: Config,
    /// The same clock the runner and scheduler use (server status).
    pub clock: Arc<dyn Clock>,
    pub agent: Arc<dyn AgentClient>,
    /// Wakes `GET /agent/v1/commands` long polls on enqueue.
    pub notifier: Notifier,
    /// Fetches followed manifest URLs (tests swap in a fake).
    pub fetcher: Arc<dyn crate::follow::ManifestFetcher>,
    login_failures: Arc<tokio::sync::Mutex<HashMap<IpAddr, Vec<Instant>>>>,
    agent_polls: Arc<tokio::sync::Mutex<HashMap<String, Vec<Instant>>>>,
    agent_poll_limit: usize,
}

#[allow(clippy::missing_fields_in_debug)] // reviewed: kept as-is (R150)
impl std::fmt::Debug for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AppState")
            .field("db", &self.db)
            .field("sealer", &"[redacted]")
            .field("config", &self.config)
            .field("agent", &"AgentClient")
            .finish()
    }
}

impl AppState {
    /// Build the state and start the command notifier's listener (on its
    /// own connection from `db`'s pool options), so this must run inside
    /// a tokio runtime.
    pub fn new(
        db: Db,
        sealer: Sealer,
        config: Config,
        clock: Arc<dyn Clock>,
        agent: Arc<dyn AgentClient>,
    ) -> Self {
        let notifier = Notifier::spawn(db.pool().clone());
        Self {
            db,
            sealer,
            config,
            clock,
            agent,
            notifier,
            fetcher: Arc::new(crate::follow::HttpFetcher),
            login_failures: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            agent_polls: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            agent_poll_limit: MAX_AGENT_POLLS,
        }
    }

    /// Override the per-server poll ceiling (tests set a tiny limit to
    /// exercise the 429 path without issuing 600 requests).
    #[allow(clippy::return_self_not_must_use)] // reviewed: kept as-is (R150)
    pub fn with_agent_poll_limit(mut self, limit: usize) -> Self {
        self.agent_poll_limit = limit;
        self
    }

    /// Replace the manifest fetcher (tests serve manifests from memory).
    #[must_use]
    pub fn with_fetcher(mut self, fetcher: Arc<dyn crate::follow::ManifestFetcher>) -> Self {
        self.fetcher = fetcher;
        self
    }

    /// Count one password attempt for the IP *before* the password is
    /// verified, in the same critical section as the check: at
    /// [`MAX_LOGIN_FAILURES`] attempts inside the window the attempt is
    /// rejected with 429 (and not counted). Concurrent guesses therefore
    /// cannot all pass the check before any of them is recorded (I4). A
    /// success then calls [`Self::clear_login_failures`] (login) or
    /// [`Self::forgive_login_attempt`] (re-auth). Prunes expired entries
    /// and evicts the oldest IP past [`MAX_LOGIN_IPS`].
    pub async fn begin_login_attempt(&self, ip: IpAddr) -> Result<(), AppError> {
        let mut table = self.login_failures.lock().await;
        let now = Instant::now();
        prune_login_table(&mut table, now);
        let entry = table.entry(ip).or_default();
        if entry.len() >= MAX_LOGIN_FAILURES {
            return Err(AppError::rate_limited(
                "too many failed login attempts; try again later",
            ));
        }
        entry.push(now);
        while table.len() > MAX_LOGIN_IPS {
            let oldest = table
                .iter()
                .min_by_key(|(_, hits)| hits.iter().max().copied())
                .map(|(addr, _)| *addr);
            match oldest {
                Some(addr) => {
                    table.remove(&addr);
                }
                None => break,
            }
        }
        Ok(())
    }

    /// Forget failures after a successful login.
    pub async fn clear_login_failures(&self, ip: IpAddr) {
        let mut table = self.login_failures.lock().await;
        table.remove(&ip);
    }

    /// Take back the newest counted attempt after a successful re-auth
    /// (env reveal, own password change): the right password is not a
    /// failure, but unlike a login it does not wipe earlier failures.
    pub async fn forgive_login_attempt(&self, ip: IpAddr) {
        let mut table = self.login_failures.lock().await;
        if let Some(hits) = table.get_mut(&ip) {
            hits.pop();
            if hits.is_empty() {
                table.remove(&ip);
            }
        }
    }

    /// Attempts currently counted for the IP (tests, diagnostics).
    pub async fn login_attempts(&self, ip: IpAddr) -> usize {
        let mut table = self.login_failures.lock().await;
        prune_login_table(&mut table, Instant::now());
        table.get(&ip).map_or(0, Vec::len)
    }

    /// Count one `GET /agent/v1/commands` poll for the server, rejecting
    /// with 429 past the per-state poll limit (default [`MAX_AGENT_POLLS`])
    /// per [`AGENT_RATE_WINDOW`]. Check and record are one atomic step
    /// (every poll counts, not just failures); stale entries prune first
    /// and the oldest server evicts past [`MAX_AGENT_SERVERS`].
    pub async fn check_agent_rate(&self, server_id: &str) -> Result<(), AppError> {
        let mut table = self.agent_polls.lock().await;
        let now = Instant::now();
        prune_agent_table(&mut table, now);
        let entry = table.entry(server_id.to_string()).or_default();
        if entry.len() >= self.agent_poll_limit {
            return Err(AppError::rate_limited(
                "too many agent polls; back off and retry",
            ));
        }
        entry.push(now);
        while table.len() > MAX_AGENT_SERVERS {
            let oldest = table
                .iter()
                .min_by_key(|(_, hits)| hits.iter().max().copied())
                .map(|(id, _)| id.clone());
            match oldest {
                Some(id) => {
                    table.remove(&id);
                }
                None => break,
            }
        }
        Ok(())
    }
}

/// Drop timestamps outside the rate-limit window, then drop IPs with no
/// live timestamps, so the table cannot accumulate dead entries.
fn prune_login_table(table: &mut HashMap<IpAddr, Vec<Instant>>, now: Instant) {
    table.retain(|_, hits| {
        hits.retain(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW);
        !hits.is_empty()
    });
}

/// Same pruning for the per-server agent poll table (1-minute window).
fn prune_agent_table(table: &mut HashMap<String, Vec<Instant>>, now: Instant) {
    table.retain(|_, hits| {
        hits.retain(|t| now.duration_since(*t) < AGENT_RATE_WINDOW);
        !hits.is_empty()
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};

    fn ip(n: u8, m: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(10, 0, n, m))
    }

    #[test]
    fn prune_drops_expired_hits_and_empty_ips() {
        let mut table: HashMap<IpAddr, Vec<Instant>> = HashMap::new();
        let now = Instant::now();
        let old = now
            .checked_sub(RATE_LIMIT_WINDOW + Duration::from_secs(1))
            .unwrap_or_else(|| now.checked_sub(Duration::from_secs(1)).unwrap());
        table.insert(ip(0, 1), vec![old]);
        table.insert(ip(0, 2), vec![old, now]);
        prune_login_table(&mut table, now);
        assert!(!table.contains_key(&ip(0, 1)), "expired-only IP is dropped");
        assert_eq!(
            table.get(&ip(0, 2)).map(Vec::len),
            Some(1),
            "live hit survives, expired hit pruned"
        );
    }

    #[test]
    #[allow(clippy::cast_possible_truncation)] // reviewed: kept as-is (R150)
    fn oldest_ip_is_the_eviction_candidate() {
        let now = Instant::now();
        let mut table: HashMap<IpAddr, Vec<Instant>> = HashMap::new();
        for i in 0..MAX_LOGIN_IPS + 5 {
            let a = (i / 256) as u8;
            let b = (i % 256) as u8;
            // Stagger hits so `ip(0,0)` is oldest, higher octets newer
            // (microsecond span: total ~10 ms, well within monotonic range).
            let age = (MAX_LOGIN_IPS + 5 - i) as u64;
            let at = now.checked_sub(Duration::from_micros(age)).unwrap_or(now);
            table.insert(ip(a, b), vec![at]);
        }
        while table.len() > MAX_LOGIN_IPS {
            let oldest = table
                .iter()
                .min_by_key(|(_, hits)| hits.iter().max().copied())
                .map(|(addr, _)| *addr)
                .expect("non-empty table has an oldest entry");
            table.remove(&oldest);
        }
        assert_eq!(table.len(), MAX_LOGIN_IPS);
        assert!(!table.contains_key(&ip(0, 0)), "oldest IP is evicted first");
    }
}
