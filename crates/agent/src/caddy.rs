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

    fn routes_url(&self) -> String {
        format!(
            "{}/config/apps/http/servers/srv0/routes",
            self.admin.trim_end_matches('/')
        )
    }

    /// Full routes array (`[]` when the key is missing/`null`).
    async fn list_routes(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let resp = self
            .client
            .get(self.routes_url())
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("caddy list routes: {e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("caddy list routes: HTTP {}", resp.status());
        }
        let routes: Option<Vec<serde_json::Value>> = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("caddy list routes: bad json: {e}"))?;
        Ok(routes.unwrap_or_default())
    }

    /// Numeric index of the route carrying `@id`, or `None` when absent.
    async fn route_index(&self, id: &str) -> anyhow::Result<Option<usize>> {
        Ok(self
            .list_routes()
            .await?
            .iter()
            .position(|r| r.get("@id").and_then(|v| v.as_str()) == Some(id)))
    }
}

#[async_trait::async_trait]
impl Caddy for CaddyAdmin {
    /// Idempotent upsert: the admin API addresses `routes` (a JSON
    /// array) by numeric index only — `@id` in the path is rejected
    /// (`invalid array index`, found by the e2e against real Caddy
    /// 2.11). Whole-array replace is PATCH, not PUT (2.6.2 answers
    /// collection-PUT with `key already exists: routes` — found the
    /// same way). Our route goes FIRST in the array: a foreign
    /// catch-all (distro default page) must never shadow a
    /// host-specific route, since Caddy evaluates routes in order.
    async fn set_route(&self, domain: &str, port: u16) -> anyhow::Result<()> {
        if !valid_domain(domain) {
            anyhow::bail!("caddy set_route: invalid domain {domain:?}");
        }
        let body = route_body(domain, port);
        let id = route_id(domain);
        let mut routes = self.list_routes().await?;
        routes.retain(|r| r.get("@id").and_then(|v| v.as_str()) != Some(id.as_str()));
        routes.insert(0, body);
        let resp = self
            .client
            .patch(self.routes_url())
            .header("Content-Type", "application/json")
            .json(&routes)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("caddy set_route {domain}: {e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("caddy set_route {domain}: HTTP {}", resp.status());
        }
        Ok(())
    }

    /// Idempotent delete by numeric index; a missing route is success.
    async fn remove_route(&self, domain: &str) -> anyhow::Result<()> {
        if !valid_domain(domain) {
            anyhow::bail!("caddy remove_route: invalid domain {domain:?}");
        }
        let Some(i) = self.route_index(&route_id(domain)).await? else {
            return Ok(());
        };
        let resp = self
            .client
            .delete(self.routes_url() + &format!("/{i}"))
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

    /// `CaddyAdmin` against a mock that behaves like real Caddy: routes
    /// are a JSON array addressed by numeric index; `@id` in the path is
    /// a 500 (the e2e failure). Catches regressions to id-in-path.
    async fn mock_admin() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        tokio::task::JoinHandle<()>,
    ) {
        use axum::{http::StatusCode, routing::any, Json, Router};
        let routes: std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let app = {
            let routes = routes.clone();
            let collection = routes.clone();
            Router::new()
                // Collection, mirroring real Caddy (2.6.2 and 2.11):
                // GET lists, PATCH replaces the whole array, PUT is
                // create-only (`key already exists`), POST appends one
                // route object.
                .route(
                    "/config/apps/http/servers/srv0/routes",
                    any(move |method: axum::http::Method, body: String| {
                        let routes = collection.clone();
                        async move {
                            let mut lock = routes.lock().expect("mock lock");
                            if method == axum::http::Method::PATCH {
                                let v: Vec<serde_json::Value> =
                                    serde_json::from_str(&body).unwrap_or_default();
                                *lock = v;
                                return (StatusCode::OK, Json(serde_json::Value::Null));
                            }
                            if method == axum::http::Method::PUT {
                                return (
                                    StatusCode::INTERNAL_SERVER_ERROR,
                                    Json(serde_json::json!({"error": "key already exists"})),
                                );
                            }
                            if body.is_empty() {
                                return (
                                    StatusCode::OK,
                                    Json(serde_json::Value::Array(lock.clone())),
                                );
                            }
                            let v: serde_json::Value =
                                serde_json::from_str(&body).unwrap_or_default();
                            lock.push(v);
                            (StatusCode::OK, Json(serde_json::Value::Null))
                        }
                    }),
                )
                // Element: numeric index only — like real Caddy, any
                // non-numeric segment is a 500, even a live @id.
                .route(
                    "/config/apps/http/servers/srv0/routes/{rest}",
                    any(
                        move |axum::extract::Path(rest): axum::extract::Path<String>,
                              body: String| {
                            let routes = routes.clone();
                            async move {
                                let mut lock = routes.lock().expect("mock lock");
                                let Ok(i) = rest.parse::<usize>() else {
                                    return (
                                        StatusCode::INTERNAL_SERVER_ERROR,
                                        Json(serde_json::json!({"error": "invalid array index"})),
                                    );
                                };
                                if body.is_empty() {
                                    // DELETE.
                                    if i < lock.len() {
                                        lock.remove(i);
                                        (StatusCode::OK, Json(serde_json::Value::Null))
                                    } else {
                                        (StatusCode::NOT_FOUND, Json(serde_json::Value::Null))
                                    }
                                } else if i < lock.len() {
                                    lock[i] = serde_json::from_str(&body)
                                        .unwrap_or(serde_json::Value::Null);
                                    (StatusCode::OK, Json(serde_json::Value::Null))
                                } else {
                                    (StatusCode::NOT_FOUND, Json(serde_json::Value::Null))
                                }
                            }
                        },
                    ),
                )
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind mock admin");
        let addr = listener.local_addr().expect("mock addr");
        let handle = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve mock admin");
        });
        (format!("http://{addr}"), routes, handle)
    }

    #[tokio::test]
    async fn admin_upserts_by_index_and_deletes_idempotently() {
        let (base, routes, _guard) = mock_admin().await;
        let c = CaddyAdmin::new(base);
        c.set_route("a.test", 20001).await.expect("create");
        c.set_route("a.test", 20001).await.expect("repeat is ok");
        c.set_route("a.test", 20002).await.expect("repoint");
        c.set_route("b.test", 20003).await.expect("second domain");
        c.remove_route("a.test").await.expect("remove");
        c.remove_route("a.test").await.expect("remove again is ok");
        c.remove_route("never.test").await.expect("missing is ok");
        // b.test survives with its port: no dupes, no clobber.
        let lock = routes.lock().expect("mock lock");
        assert_eq!(lock.len(), 1, "routes: {lock:?}");
        assert_eq!(lock[0]["@id"], "setu-b.test");
        assert_eq!(
            lock[0]["handle"][0]["upstreams"][0]["dial"],
            "127.0.0.1:20003"
        );
    }

    #[tokio::test]
    async fn admin_prepends_ahead_of_foreign_catchall() {
        let (base, routes, _guard) = mock_admin().await;
        // A foreign catch-all (distro default page): ours must land
        // ahead of it and never duplicate.
        routes.lock().expect("mock lock").push(serde_json::json!({
            "@id": "default-catchall",
            "handle": [{"handler": "static_response", "body": "Caddy works!"}],
        }));
        let c = CaddyAdmin::new(base);
        c.set_route("a.test", 20001).await.expect("create");
        c.set_route("a.test", 20002).await.expect("repoint");
        {
            let lock = routes.lock().expect("mock lock");
            assert_eq!(lock.len(), 2, "routes: {lock:?}");
            assert_eq!(lock[0]["@id"], "setu-a.test", "ours first: {lock:?}");
            assert_eq!(
                lock[0]["handle"][0]["upstreams"][0]["dial"], "127.0.0.1:20002",
                "repoint applied once: {lock:?}"
            );
            assert_eq!(lock[1]["@id"], "default-catchall", "foreign kept: {lock:?}");
        }
    }
}
