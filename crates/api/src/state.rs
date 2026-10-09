//! Shared handler state: database, sealer, config, agent client and the
//! in-memory login rate-limit table.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use setu_common::{AppError, Config, Sealer};
use setu_db::Db;
use setu_jobs::AgentClient;

/// Failed logins per source IP within [`RATE_LIMIT_WINDOW`]; at
/// [`MAX_LOGIN_FAILURES`] the next attempt is rejected with 429.
const MAX_LOGIN_FAILURES: usize = 10;
const RATE_LIMIT_WINDOW: Duration = Duration::from_secs(15 * 60);
/// Hard cap on tracked IPs so a distributed scan cannot grow the map
/// without bound; the oldest entry is evicted first.
const MAX_LOGIN_IPS: usize = 10_000;

/// State shared by every handler. `Clone` is cheap (all `Arc` inside bar
/// `Config`, which is small); `Debug` redacts the sealer and secret key
/// (via their own `Debug` impls) and never prints tokens.
#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub sealer: Sealer,
    pub config: Config,
    pub agent: Arc<dyn AgentClient>,
    login_failures: Arc<tokio::sync::Mutex<HashMap<IpAddr, Vec<Instant>>>>,
}

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
    pub fn new(db: Db, sealer: Sealer, config: Config, agent: Arc<dyn AgentClient>) -> Self {
        Self {
            db,
            sealer,
            config,
            agent,
            login_failures: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        }
    }

    /// Reject the login when this IP already failed [`MAX_LOGIN_FAILURES`]
    /// times inside the window (pruning stale entries first).
    pub async fn check_login_rate(&self, ip: IpAddr) -> Result<(), AppError> {
        let mut table = self.login_failures.lock().await;
        let now = Instant::now();
        let entry = table.entry(ip).or_default();
        entry.retain(|t| now.duration_since(*t) < RATE_LIMIT_WINDOW);
        if entry.len() >= MAX_LOGIN_FAILURES {
            return Err(AppError::rate_limited(
                "too many failed login attempts; try again later",
            ));
        }
        Ok(())
    }

    /// Record one failed login for the IP, pruning expired entries and
    /// evicting the oldest IP when the table exceeds [`MAX_LOGIN_IPS`].
    pub async fn record_login_failure(&self, ip: IpAddr) {
        let mut table = self.login_failures.lock().await;
        let now = Instant::now();
        prune_login_table(&mut table, now);
        table.entry(ip).or_default().push(now);
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
    }

    /// Forget failures after a successful login.
    pub async fn clear_login_failures(&self, ip: IpAddr) {
        let mut table = self.login_failures.lock().await;
        table.remove(&ip);
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
            .unwrap_or_else(|| now - Duration::from_secs(1));
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
