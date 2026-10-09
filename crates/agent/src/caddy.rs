//! Caddy admin adapter: a trait, the reqwest impl, and an in-memory fake.
//!
//! Routes carry `@id = "setu-<domain>"`, so set/remove is idempotent: PUT
//! upserts, DELETE of a missing route succeeds.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// Caddy surface the agent needs.
#[async_trait::async_trait]
pub trait Caddy: Send + Sync {
    async fn set_route(&self, domain: &str, port: u16) -> anyhow::Result<()>;
    async fn remove_route(&self, domain: &str) -> anyhow::Result<()>;
}

/// Route id pinned in Caddy's config for a domain.
pub fn route_id(domain: &str) -> String {
    format!("setu-{domain}")
}

/// Domains are bare hostnames (letters, digits, hyphen, dot); anything
/// else is rejected before it reaches the admin URL path or config.
pub fn valid_domain(domain: &str) -> bool {
    !domain.is_empty()
        && domain.len() <= 253
        && domain
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

/// Route object PUT to the Caddy admin API.
pub fn route_body(domain: &str, port: u16) -> serde_json::Value {
    serde_json::json!({
        "@id": route_id(domain),
        "match": [{ "host": [domain] }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": format!("127.0.0.1:{port}") }],
        }],
        "terminal": true,
    })
}

/// Production impl over the Caddy admin API
/// (`/config/apps/http/servers/srv0/routes`).
#[derive(Debug, Clone)]
pub struct CaddyAdmin {
    admin: String,
    client: reqwest::Client,
}

impl CaddyAdmin {
    pub fn new(admin: String) -> Self {
        Self {
            admin,
            client: reqwest::Client::new(),
        }
    }

    fn route_url(&self, domain: &str) -> String {
        format!(
            "{}/config/apps/http/servers/srv0/routes/{}",
            self.admin.trim_end_matches('/'),
            route_id(domain)
        )
    }
}

#[async_trait::async_trait]
impl Caddy for CaddyAdmin {
    async fn set_route(&self, domain: &str, port: u16) -> anyhow::Result<()> {
        if !valid_domain(domain) {
            anyhow::bail!("caddy set_route: invalid domain {domain:?}");
        }
        let resp = self
            .client
            .put(self.route_url(domain))
            .header("Content-Type", "application/json")
            .json(&route_body(domain, port))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("caddy set_route {domain}: {e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("caddy set_route {domain}: HTTP {}", resp.status());
        }
        Ok(())
    }

    async fn remove_route(&self, domain: &str) -> anyhow::Result<()> {
        if !valid_domain(domain) {
            anyhow::bail!("caddy remove_route: invalid domain {domain:?}");
        }
        let resp = self
            .client
            .delete(self.route_url(domain))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("caddy remove_route {domain}: {e}"))?;
        // 404 means already gone: idempotent success.
        if !(resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND) {
            anyhow::bail!("caddy remove_route {domain}: HTTP {}", resp.status());
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct FakeInner {
    routes: HashMap<String, u16>,
    calls: Vec<String>,
}

/// In-memory `Caddy` for tests.
#[derive(Debug, Clone, Default)]
pub struct CaddyFake {
    inner: Arc<Mutex<FakeInner>>,
}

impl CaddyFake {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake caddy lock")
    }

    pub fn route(&self, domain: &str) -> Option<u16> {
        self.lock().routes.get(domain).copied()
    }

    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }
}

#[async_trait::async_trait]
impl Caddy for CaddyFake {
    async fn set_route(&self, domain: &str, port: u16) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("set {domain} {port}"));
        inner.routes.insert(domain.to_string(), port);
        Ok(())
    }

    async fn remove_route(&self, domain: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("remove {domain}"));
        inner.routes.remove(domain);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_body_pins_id_and_dial() {
        let body = route_body("example.com", 20001);
        assert_eq!(body["@id"], "setu-example.com");
        assert_eq!(body["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:20001");
    }

    #[test]
    fn valid_domain_accepts_hostnames_rejects_paths() {
        assert!(valid_domain("example.com"));
        assert!(valid_domain("a-b.test-1.example"));
        assert!(!valid_domain(""));
        assert!(!valid_domain("a/b"));
        assert!(!valid_domain("a b"));
        assert!(!valid_domain("a/b@test"));
        assert!(!valid_domain("../etc"));
    }

    #[tokio::test]
    async fn fake_set_then_remove() {
        let c = CaddyFake::new();
        c.set_route("a.test", 20001).await.expect("set");
        assert_eq!(c.route("a.test"), Some(20001));
        c.remove_route("a.test").await.expect("remove");
        assert_eq!(c.route("a.test"), None);
        // Removing again is fine (idempotent).
        c.remove_route("a.test").await.expect("remove again");
    }
}
