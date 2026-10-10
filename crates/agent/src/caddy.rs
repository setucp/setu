//! Caddy admin adapter (spec §4 "Caddy").
//!
//! The admin API listens only on `unix//run/caddy/admin.sock|0660` (group
//! `caddy`, `setu` is a member); TCP 2019 is off and `doctor` checks it.
//! Routes are objects in `apps.http.servers.setu.routes` with
//! `@id = "setu-site-<short>"` or `"setu-panel"`:
//! - upsert: `GET /id/<id>`; 200 ⇒ `PATCH /id/<id>` (replace in place),
//!   else `POST /config/apps/http/servers/setu/routes` (append). Never
//!   `PUT`, which inserts a duplicate.
//! - remove: `DELETE /id/<id>`; 404 is success (idempotent).
//!
//! [`CaddyClient`] holds that logic over an [`AdminTransport`]:
//! [`UnixTransport`] (hyper 1 HTTP/1 over a `tokio::net::UnixStream`) in
//! production ([`CaddySocket`]), or the in-memory [`FakeAdmin`] model in
//! tests ([`FakeCaddy`]), so the fake exercises the same request
//! sequence the socket client sends.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;

/// Route id of the control-plane panel route.
pub const PANEL_ROUTE_ID: &str = "setu-panel";

/// The Caddy server Setu owns.
pub const SERVER_NAME: &str = "setu";

/// Admin path of the routes array of the `setu` server.
pub const ROUTES_PATH: &str = "/config/apps/http/servers/setu/routes";

/// Default admin socket.
pub const DEFAULT_SOCKET: &str = "/run/caddy/admin.sock";

/// Caddy's default TCP admin address, which must stay closed.
pub const TCP_ADMIN_ADDR: &str = "127.0.0.1:2019";

/// Default panel upstream (`setu serve` on its unix socket).
pub const DEFAULT_PANEL_UPSTREAM: &str = "unix//run/setu/http.sock";

/// Per-request deadline on the admin socket (connect + response).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The panel route recorded in `/etc/setu/agent.toml` on a control-plane
/// box: `host` → `upstream` (default `unix//run/setu/http.sock`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PanelRoute {
    pub host: String,
    pub upstream: String,
}

/// `setu-site-<short>` for a validated short id.
pub fn site_route_id(short: &str) -> anyhow::Result<String> {
    if !setu_common::is_short_id(short) {
        anyhow::bail!("invalid site short id");
    }
    Ok(format!("setu-site-{short}"))
}

/// Upstream of a site route: loopback plus the site's port.
pub fn site_upstream(port: u16) -> String {
    format!("127.0.0.1:{port}")
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

/// `setu-panel` or `setu-site-<short>`: the only ids Setu writes, and the
/// only ones that reach an admin URL path.
pub fn valid_route_id(id: &str) -> bool {
    id == PANEL_ROUTE_ID
        || id
            .strip_prefix("setu-site-")
            .is_some_and(setu_common::is_short_id)
}

/// `127.0.0.1:<port>` or `unix/<absolute path>` with a conservative
/// character set (no quotes, spaces or control characters).
fn valid_upstream(upstream: &str) -> bool {
    if let Some(path) = upstream.strip_prefix("unix/") {
        return path.starts_with('/')
            && path.len() > 1
            && path
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c));
    }
    upstream
        .strip_prefix("127.0.0.1:")
        .is_some_and(|p| p.parse::<u16>().is_ok_and(|p| p > 0))
}

/// The route object (golden, see `tests/golden/route-*.json`).
pub fn route_json(id: &str, domain: &str, upstream: &str) -> anyhow::Result<serde_json::Value> {
    if !valid_route_id(id) {
        anyhow::bail!("invalid route id {id:?}");
    }
    if !valid_domain(domain) {
        anyhow::bail!("invalid domain {domain:?}");
    }
    if !valid_upstream(upstream) {
        anyhow::bail!("invalid upstream {upstream:?}");
    }
    Ok(serde_json::json!({
        "@id": id,
        "match": [{ "host": [domain] }],
        "handle": [{
            "handler": "reverse_proxy",
            "upstreams": [{ "dial": upstream }],
        }],
        "terminal": true,
    }))
}

/// Caddy surface the agent and `install_system` need.
#[async_trait::async_trait]
pub trait CaddyAdmin: Send + Sync {
    /// Create or replace the route `id` (`GET /id` → `PATCH` | `POST`).
    async fn upsert_route(&self, id: &str, domain: &str, upstream: &str) -> anyhow::Result<()>;
    /// Delete route `id`; a missing route is success.
    async fn remove_route(&self, id: &str) -> anyhow::Result<()>;
    /// `@id`s of the routes of server `setu`, in order.
    async fn list_route_ids(&self) -> anyhow::Result<Vec<String>> {
        Ok(self
            .routes()
            .await?
            .iter()
            .filter_map(|r| r.get("@id").and_then(|v| v.as_str()).map(str::to_string))
            .collect())
    }
    /// The full routes array of server `setu` (`[]` when null/missing).
    async fn routes(&self) -> anyhow::Result<Vec<serde_json::Value>>;
    /// Make route `id` the first route of server `setu` (Caddy routes
    /// are first-match): when it exists and is not already first, the
    /// array is rebuilt with it in front and written back with one
    /// `PATCH` of the routes path (replace). A missing route is a no-op.
    async fn move_route_first(&self, id: &str) -> anyhow::Result<()>;
    /// `POST /load` a whole config.
    async fn load(&self, config: &serde_json::Value) -> anyhow::Result<()>;
    /// True when something accepts TCP connections on the admin port.
    async fn tcp_admin_open(&self) -> bool;
}

/// One admin API exchange: method, origin-form path, optional JSON body
/// → (status, body).
#[async_trait::async_trait]
pub trait AdminTransport: Send + Sync {
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<(u16, Vec<u8>)>;
    async fn tcp_admin_open(&self) -> bool;
}

/// The admin-API logic over any transport.
#[derive(Debug, Clone)]
pub struct CaddyClient<T> {
    transport: T,
}

/// Production client: HTTP/1 over the admin unix socket.
pub type CaddySocket = CaddyClient<UnixTransport>;

/// Test client over the in-memory [`FakeAdmin`].
pub type FakeCaddy = CaddyClient<FakeAdmin>;

impl<T> CaddyClient<T> {
    pub fn with_transport(transport: T) -> Self {
        Self { transport }
    }

    pub fn transport(&self) -> &T {
        &self.transport
    }
}

impl CaddySocket {
    /// Client for the admin socket at `socket`.
    pub fn new(socket: impl AsRef<Path>) -> Self {
        Self::with_transport(UnixTransport {
            socket: socket.as_ref().to_path_buf(),
            tcp_admin: TCP_ADMIN_ADDR.parse().expect("static addr"),
        })
    }

    /// Probe another address for [`CaddyAdmin::tcp_admin_open`] (tests).
    pub fn with_tcp_admin_addr(mut self, addr: SocketAddr) -> Self {
        self.transport.tcp_admin = addr;
        self
    }
}

impl FakeCaddy {
    /// An up fake holding the base config (`routes: []`).
    pub fn new() -> Self {
        Self::with_transport(FakeAdmin::new())
    }

    pub fn admin(&self) -> &FakeAdmin {
        &self.transport
    }
}

impl Default for FakeCaddy {
    fn default() -> Self {
        Self::new()
    }
}

fn first_line(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    let line = text.lines().next().unwrap_or("").trim();
    line.chars().take(200).collect()
}

impl<T: AdminTransport> CaddyClient<T> {
    async fn expect_ok(
        &self,
        what: &str,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<Vec<u8>> {
        let (status, resp) = self.transport.request(method, path, body).await?;
        if !(200..300).contains(&status) {
            anyhow::bail!("caddy {what}: HTTP {status}: {}", first_line(&resp));
        }
        Ok(resp)
    }
}

#[async_trait::async_trait]
impl<T: AdminTransport> CaddyAdmin for CaddyClient<T> {
    async fn upsert_route(&self, id: &str, domain: &str, upstream: &str) -> anyhow::Result<()> {
        let route = route_json(id, domain, upstream)?;
        let body = serde_json::to_vec(&route)?;
        let path = format!("/id/{id}");
        let (status, resp) = self.transport.request("GET", &path, None).await?;
        match status {
            200 => {
                self.expect_ok("replace route", "PATCH", &path, Some(body))
                    .await?;
            }
            404 => {
                self.expect_ok("add route", "POST", ROUTES_PATH, Some(body))
                    .await?;
            }
            other => anyhow::bail!("caddy get route: HTTP {other}: {}", first_line(&resp)),
        }
        Ok(())
    }

    async fn remove_route(&self, id: &str) -> anyhow::Result<()> {
        if !valid_route_id(id) {
            anyhow::bail!("invalid route id {id:?}");
        }
        let (status, resp) = self
            .transport
            .request("DELETE", &format!("/id/{id}"), None)
            .await?;
        if (200..300).contains(&status) || status == 404 {
            Ok(())
        } else {
            anyhow::bail!("caddy remove route: HTTP {status}: {}", first_line(&resp))
        }
    }

    async fn routes(&self) -> anyhow::Result<Vec<serde_json::Value>> {
        let body = self
            .expect_ok("list routes", "GET", ROUTES_PATH, None)
            .await?;
        let routes: Option<Vec<serde_json::Value>> =
            serde_json::from_slice(&body).context("caddy list routes: bad json")?;
        Ok(routes.unwrap_or_default())
    }

    async fn move_route_first(&self, id: &str) -> anyhow::Result<()> {
        if !valid_route_id(id) {
            anyhow::bail!("invalid route id {id:?}");
        }
        let mut routes = self.routes().await?;
        let is_id = |r: &serde_json::Value| r.get("@id").and_then(|v| v.as_str()) == Some(id);
        let Some(pos) = routes.iter().position(is_id) else {
            return Ok(());
        };
        if pos == 0 {
            return Ok(());
        }
        let route = routes.remove(pos);
        routes.insert(0, route);
        let body = serde_json::to_vec(&serde_json::Value::Array(routes))?;
        self.expect_ok("reorder routes", "PATCH", ROUTES_PATH, Some(body))
            .await?;
        Ok(())
    }

    async fn load(&self, config: &serde_json::Value) -> anyhow::Result<()> {
        self.expect_ok("load", "POST", "/load", Some(serde_json::to_vec(config)?))
            .await?;
        Ok(())
    }

    async fn tcp_admin_open(&self) -> bool {
        self.transport.tcp_admin_open().await
    }
}

/// HTTP/1 over the admin unix socket (hyper 1, one connection per
/// request; admin traffic is a handful of calls).
#[derive(Debug, Clone)]
pub struct UnixTransport {
    socket: PathBuf,
    tcp_admin: SocketAddr,
}

impl UnixTransport {
    async fn exchange(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<(u16, Vec<u8>)> {
        use http_body_util::{BodyExt as _, Full};
        let stream = tokio::net::UnixStream::connect(&self.socket)
            .await
            .with_context(|| format!("caddy admin socket {}", self.socket.display()))?;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(stream))
                .await
                .context("caddy admin handshake")?;
        let driver = tokio::spawn(async move {
            let _ = conn.await;
        });
        let method = hyper::Method::from_bytes(method.as_bytes()).context("method")?;
        // Caddy accepts only "", 127.0.0.1 or ::1 as Host on a unix
        // socket admin listener.
        let mut req = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header(hyper::header::HOST, "127.0.0.1");
        if body.is_some() {
            req = req.header(hyper::header::CONTENT_TYPE, "application/json");
        }
        let req = req
            .body(Full::new(bytes::Bytes::from(body.unwrap_or_default())))
            .context("caddy admin request")?;
        let resp = sender
            .send_request(req)
            .await
            .context("caddy admin request")?;
        let status = resp.status().as_u16();
        let bytes = resp
            .into_body()
            .collect()
            .await
            .context("caddy admin response")?
            .to_bytes();
        driver.abort();
        Ok((status, bytes.to_vec()))
    }
}

#[async_trait::async_trait]
impl AdminTransport for UnixTransport {
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<(u16, Vec<u8>)> {
        tokio::time::timeout(REQUEST_TIMEOUT, self.exchange(method, path, body))
            .await
            .map_err(|_| anyhow::anyhow!("caddy admin: timed out"))?
    }

    async fn tcp_admin_open(&self) -> bool {
        matches!(
            tokio::time::timeout(
                Duration::from_secs(2),
                tokio::net::TcpStream::connect(self.tcp_admin)
            )
            .await,
            Ok(Ok(_))
        )
    }
}

/// One request the fake saw.
#[derive(Debug, Clone, PartialEq)]
pub struct FakeRequest {
    pub method: String,
    pub path: String,
    pub body: Option<serde_json::Value>,
}

#[derive(Debug)]
struct FakeState {
    config: serde_json::Value,
    requests: Vec<FakeRequest>,
    up: bool,
    tcp_admin_open: bool,
}

/// In-memory model of the admin API subset Setu uses, mirroring real
/// Caddy where it matters: `@id`s are unique (a duplicate is refused on
/// `POST` and `/load`), `PUT` on an existing id is refused, `PATCH /id`
/// replaces in place, `DELETE /id` of a missing id is 404. Records every
/// request; `set_up(false)` makes every call fail like a dead socket.
#[derive(Debug, Clone)]
pub struct FakeAdmin {
    inner: Arc<Mutex<FakeState>>,
}

impl Default for FakeAdmin {
    fn default() -> Self {
        Self::new()
    }
}

fn json_body(v: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(v).unwrap_or_default()
}

fn err_body(msg: &str) -> Vec<u8> {
    json_body(&serde_json::json!({ "error": msg }))
}

fn collect_ids(v: &serde_json::Value, out: &mut Vec<String>) {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(id) = map.get("@id").and_then(|v| v.as_str()) {
                out.push(id.to_string());
            }
            map.values().for_each(|v| collect_ids(v, out));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| collect_ids(v, out)),
        _ => {}
    }
}

fn has_duplicate_ids(v: &serde_json::Value) -> bool {
    let mut ids = Vec::new();
    collect_ids(v, &mut ids);
    let n = ids.len();
    ids.sort();
    ids.dedup();
    ids.len() != n
}

impl FakeAdmin {
    /// Up, holding the base config from `assets/caddy-base.json`.
    pub fn new() -> Self {
        let config = serde_json::from_str(crate::install_system::CADDY_BASE_ASSET)
            .expect("caddy-base.json is valid JSON");
        Self {
            inner: Arc::new(Mutex::new(FakeState {
                config,
                requests: Vec::new(),
                up: true,
                tcp_admin_open: false,
            })),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_up(&self, up: bool) {
        self.lock().up = up;
    }

    pub fn is_up(&self) -> bool {
        self.lock().up
    }

    pub fn set_tcp_admin_open(&self, open: bool) {
        self.lock().tcp_admin_open = open;
    }

    pub fn requests(&self) -> Vec<FakeRequest> {
        self.lock().requests.clone()
    }

    pub fn clear_requests(&self) {
        self.lock().requests.clear();
    }

    /// The whole running config.
    pub fn config(&self) -> serde_json::Value {
        self.lock().config.clone()
    }

    /// Routes of server `setu`.
    pub fn routes(&self) -> Vec<serde_json::Value> {
        routes_of(&self.lock().config).cloned().unwrap_or_default()
    }

    /// Serve one request against the model (also used by a test server
    /// that exposes it over a real socket).
    pub fn handle(&self, method: &str, path: &str, body: &[u8]) -> (u16, Vec<u8>) {
        let mut st = self.lock();
        let parsed: Option<serde_json::Value> = if body.is_empty() {
            None
        } else {
            match serde_json::from_slice(body) {
                Ok(v) => Some(v),
                Err(_) => return (400, err_body("invalid JSON")),
            }
        };
        st.requests.push(FakeRequest {
            method: method.to_string(),
            path: path.to_string(),
            body: parsed.clone(),
        });
        let cfg = &mut st.config;
        if let Some(id) = path.strip_prefix("/id/") {
            let id = id.trim_end_matches('/');
            let idx = routes_of(cfg).and_then(|r| {
                r.iter()
                    .position(|r| r.get("@id").and_then(|v| v.as_str()) == Some(id))
            });
            return match (method, idx) {
                ("GET", Some(i)) => (200, json_body(&routes_of(cfg).expect("routes")[i])),
                ("PATCH", Some(i)) => match parsed {
                    Some(v) => {
                        routes_mut(cfg).expect("routes")[i] = v;
                        if has_duplicate_ids(cfg) {
                            return (400, err_body("duplicate ID"));
                        }
                        (200, Vec::new())
                    }
                    None => (400, err_body("missing body")),
                },
                ("DELETE", Some(i)) => {
                    routes_mut(cfg).expect("routes").remove(i);
                    (200, Vec::new())
                }
                // Real Caddy inserts on PUT; Setu must never PUT.
                ("PUT", Some(_)) => (400, err_body("PUT on an existing id")),
                (_, None) => (404, err_body(&format!("unknown object ID '{id}'"))),
                _ => (405, err_body("method not allowed")),
            };
        }
        match (method, path) {
            ("GET", "/config/" | "/config") => (200, json_body(cfg)),
            ("GET", ROUTES_PATH) => match routes_of(cfg) {
                Some(r) => (200, json_body(&serde_json::Value::Array(r.clone()))),
                None => (200, b"null".to_vec()),
            },
            ("POST", ROUTES_PATH) => {
                let Some(route) = parsed else {
                    return (400, err_body("missing body"));
                };
                let Some(routes) = routes_mut(cfg) else {
                    return (404, err_body("no routes array"));
                };
                routes.push(route);
                if has_duplicate_ids(cfg) {
                    routes_mut(cfg).expect("routes").pop();
                    return (400, err_body("duplicate ID"));
                }
                (200, Vec::new())
            }
            // PATCH replaces an existing value (Caddy's semantics).
            ("PATCH", ROUTES_PATH) => {
                let Some(serde_json::Value::Array(new)) = parsed else {
                    return (400, err_body("routes must be an array"));
                };
                let Some(routes) = routes_mut(cfg) else {
                    return (404, err_body("no routes array"));
                };
                let old = std::mem::replace(routes, new);
                if has_duplicate_ids(cfg) {
                    *routes_mut(cfg).expect("routes") = old;
                    return (400, err_body("duplicate ID"));
                }
                (200, Vec::new())
            }
            ("POST", "/load") => match parsed {
                Some(v) if has_duplicate_ids(&v) => (400, err_body("duplicate ID")),
                Some(v) => {
                    *cfg = v;
                    (200, Vec::new())
                }
                None => (400, err_body("missing body")),
            },
            ("PUT", _) => (409, err_body("key already exists")),
            _ => (404, err_body("not found")),
        }
    }
}

fn routes_of(cfg: &serde_json::Value) -> Option<&Vec<serde_json::Value>> {
    cfg.pointer("/apps/http/servers/setu/routes")?.as_array()
}

fn routes_mut(cfg: &mut serde_json::Value) -> Option<&mut Vec<serde_json::Value>> {
    cfg.pointer_mut("/apps/http/servers/setu/routes")?
        .as_array_mut()
}

#[async_trait::async_trait]
impl AdminTransport for FakeAdmin {
    async fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> anyhow::Result<(u16, Vec<u8>)> {
        if !self.is_up() {
            anyhow::bail!("caddy admin socket: connection refused (fake down)");
        }
        Ok(self.handle(method, path, &body.unwrap_or_default()))
    }

    async fn tcp_admin_open(&self) -> bool {
        self.lock().tcp_admin_open
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn upstreams_are_loopback_ports_or_unix_paths() {
        assert!(valid_upstream("127.0.0.1:20001"));
        assert!(valid_upstream("unix//run/setu/http.sock"));
        assert!(!valid_upstream("127.0.0.1:0"));
        assert!(!valid_upstream("10.0.0.1:80"));
        assert!(!valid_upstream("unix/relative.sock"));
        assert!(!valid_upstream("unix//run/a b.sock"));
    }

    #[test]
    fn route_ids_are_panel_or_site_short() {
        assert!(valid_route_id("setu-panel"));
        assert!(valid_route_id("setu-site-abcd1234"));
        assert!(!valid_route_id("setu-site-ABCD1234"));
        assert!(!valid_route_id("setu-site-../x"));
        assert!(!valid_route_id("setu-example.com"));
    }
}
