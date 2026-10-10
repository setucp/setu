// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Caddy admin adapter (spec §4 "Caddy", plan Task 10c): route ids
//! `setu-site-<short>` / `setu-panel`, `GET /id` then `PATCH /id` or
//! `POST …/servers/setu/routes`, never `PUT`; the socket client speaks
//! HTTP/1 over a unix socket.

use std::path::PathBuf;

use setu_agent::caddy::{
    route_json, site_route_id, site_upstream, CaddyAdmin, CaddySocket, FakeAdmin, FakeCaddy,
    PANEL_ROUTE_ID, ROUTES_PATH,
};

const SHORT: &str = "abcd1234";

fn methods_and_paths(admin: &FakeAdmin) -> Vec<String> {
    admin
        .requests()
        .into_iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect()
}

#[tokio::test]
async fn caddy_upsert_uses_patch_for_existing_and_post_for_new() {
    let caddy = FakeCaddy::new();
    let id = site_route_id(SHORT).expect("id");
    assert_eq!(id, "setu-site-abcd1234");

    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20001))
        .await
        .expect("create");
    assert_eq!(
        methods_and_paths(caddy.admin()),
        vec![
            "GET /id/setu-site-abcd1234".to_string(),
            format!("POST {ROUTES_PATH}"),
        ]
    );

    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20002))
        .await
        .expect("replace");
    let calls = methods_and_paths(caddy.admin());
    assert_eq!(
        calls[2..],
        [
            "GET /id/setu-site-abcd1234".to_string(),
            "PATCH /id/setu-site-abcd1234".to_string(),
        ]
    );
    assert!(!calls.iter().any(|c| c.starts_with("PUT ")), "{calls:?}");

    let routes = caddy.admin().routes();
    assert_eq!(routes.len(), 1, "no duplicate route: {routes:?}");
    assert_eq!(
        routes[0]["handle"][0]["upstreams"][0]["dial"],
        "127.0.0.1:20002"
    );
    assert_eq!(caddy.list_route_ids().await.expect("ids"), vec![id.clone()]);

    // The fake behaves like Caddy where it matters: PUT on an existing
    // id and a second route with the same @id are refused.
    let body = serde_json::to_vec(&route_json(&id, "a.example.test", "127.0.0.1:1").expect("json"))
        .expect("bytes");
    assert_eq!(
        caddy.admin().handle("PUT", &format!("/id/{id}"), &body).0,
        400
    );
    assert_eq!(caddy.admin().handle("POST", ROUTES_PATH, &body).0, 400);
    assert_eq!(caddy.admin().routes().len(), 1);
}

#[test]
fn caddy_route_json_is_golden() {
    let site = route_json(
        &site_route_id(SHORT).expect("id"),
        "a.example.test",
        &site_upstream(20001),
    )
    .expect("site route");
    assert_eq!(
        serde_json::to_string_pretty(&site).expect("json") + "\n",
        include_str!("golden/route-setu-site-abcd1234.json")
    );
    let panel = route_json(
        PANEL_ROUTE_ID,
        "panel.example.test",
        "unix//run/setu/http.sock",
    )
    .expect("panel route");
    assert_eq!(
        serde_json::to_string_pretty(&panel).expect("json") + "\n",
        include_str!("golden/route-setu-panel.json")
    );

    // Nothing hostile reaches the admin URL path or the config.
    assert!(site_route_id("ABCD1234").is_err());
    assert!(site_route_id("../x/y12").is_err());
    assert!(route_json("setu-site-abcd1234", "a b", "127.0.0.1:1").is_err());
    assert!(route_json("setu-other", "a.test", "127.0.0.1:1").is_err());
    assert!(route_json("setu-panel", "a.test", "127.0.0.1:1\"").is_err());
}

#[tokio::test]
async fn caddy_remove_missing_is_ok() {
    let caddy = FakeCaddy::new();
    let id = site_route_id(SHORT).expect("id");
    caddy.remove_route(&id).await.expect("missing is ok");
    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20001))
        .await
        .expect("create");
    caddy.remove_route(&id).await.expect("remove");
    caddy.remove_route(&id).await.expect("remove again");
    assert_eq!(caddy.admin().routes(), [] as [serde_json::Value; 0]);
    let calls = methods_and_paths(caddy.admin());
    assert_eq!(
        calls
            .iter()
            .filter(|c| c.starts_with("DELETE /id/"))
            .count(),
        3,
        "{calls:?}"
    );
}

/// Serves `FakeAdmin` over a unix socket in a tempdir with axum (HTTP/1),
/// rejecting a Host other than Caddy's unix-socket allowlist.
struct FakeCaddyServer {
    _dir: tempfile::TempDir,
    socket: PathBuf,
    admin: FakeAdmin,
    task: tokio::task::JoinHandle<()>,
}

impl FakeCaddyServer {
    #[allow(clippy::unused_async, clippy::unused_async_trait_impl)] // reviewed: kept as-is (R150)
    async fn start() -> Self {
        use axum::body::Bytes;
        use axum::http::{HeaderMap, Method, StatusCode, Uri};
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("admin.sock");
        let admin = FakeAdmin::new();
        let served = admin.clone();
        let app = axum::Router::new().fallback(
            move |method: Method, uri: Uri, headers: HeaderMap, body: Bytes| {
                let admin = served.clone();
                async move {
                    let host = headers
                        .get(axum::http::header::HOST)
                        .and_then(|h| h.to_str().ok())
                        .unwrap_or("");
                    // Caddy accepts only these Host values on a unix socket.
                    if !["", "127.0.0.1", "::1"].contains(&host) {
                        return (StatusCode::FORBIDDEN, Vec::new());
                    }
                    let (status, body) = admin.handle(method.as_str(), uri.path(), &body);
                    (StatusCode::from_u16(status).expect("status"), body)
                }
            },
        );
        let listener = tokio::net::UnixListener::bind(&socket).expect("bind unix");
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve");
        });
        Self {
            _dir: dir,
            socket,
            admin,
            task,
        }
    }
}

impl Drop for FakeCaddyServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[tokio::test]
async fn caddy_socket_client_talks_http1_over_unix() {
    let server = FakeCaddyServer::start().await;
    let caddy = CaddySocket::new(&server.socket);
    let id = site_route_id(SHORT).expect("id");

    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20001))
        .await
        .expect("create over the socket");
    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20002))
        .await
        .expect("replace over the socket");
    caddy
        .upsert_route(
            PANEL_ROUTE_ID,
            "panel.example.test",
            "unix//run/setu/http.sock",
        )
        .await
        .expect("panel route");
    assert_eq!(
        caddy.list_route_ids().await.expect("ids"),
        vec![id.clone(), PANEL_ROUTE_ID.to_string()]
    );
    let routes = caddy.routes().await.expect("routes");
    assert_eq!(
        routes[0]["handle"][0]["upstreams"][0]["dial"],
        "127.0.0.1:20002"
    );

    caddy.remove_route(&id).await.expect("remove");
    caddy.remove_route(&id).await.expect("remove missing");
    assert_eq!(
        caddy.list_route_ids().await.expect("ids"),
        vec![PANEL_ROUTE_ID.to_string()]
    );

    let mut config = setu_agent::install_system::caddy_base_config(true, None).expect("base");
    config["apps"]["http"]["servers"]["setu"]["routes"] = serde_json::json!([]);
    caddy.load(&config).await.expect("load");
    assert_eq!(
        caddy.list_route_ids().await.expect("ids"),
        [] as [std::string::String; 0]
    );
    assert_eq!(server.admin.config()["apps"]["tls"], config["apps"]["tls"]);

    let calls = methods_and_paths(&server.admin);
    assert!(
        calls.contains(&"PATCH /id/setu-site-abcd1234".to_string()),
        "{calls:?}"
    );
    assert!(calls.contains(&"POST /load".to_string()), "{calls:?}");
    assert!(!calls.iter().any(|c| c.starts_with("PUT ")), "{calls:?}");

    // A missing socket is an error, not a hang.
    let gone = CaddySocket::new(server.socket.with_file_name("missing.sock"));
    assert!(gone.list_route_ids().await.is_err());
}

#[tokio::test]
async fn tcp_admin_open_detects_listener() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    let caddy = CaddySocket::new("/nonexistent/admin.sock").with_tcp_admin_addr(addr);
    assert!(caddy.tcp_admin_open().await, "listener must be detected");
    drop(listener);
    assert!(
        !caddy.tcp_admin_open().await,
        "closed port must read closed"
    );

    let fake = FakeCaddy::new();
    assert!(!fake.tcp_admin_open().await);
    fake.admin().set_tcp_admin_open(true);
    assert!(fake.tcp_admin_open().await);
}

/// Optional check against a real Caddy: set `SETU_TEST_CADDY_SOCKET` to
/// an admin socket this user can open (e.g. a `caddy:2-alpine`
/// container started with the base config and the socket in a bind
/// mount). Skipped otherwise.
#[tokio::test]
async fn caddy_socket_against_real_caddy_when_available() {
    let Ok(socket) = std::env::var("SETU_TEST_CADDY_SOCKET") else {
        eprintln!("skipped: SETU_TEST_CADDY_SOCKET not set");
        return;
    };
    let caddy = CaddySocket::new(&socket);
    let base = setu_agent::install_system::caddy_base_config(true, None).expect("base");
    // Keep the listener the test harness chose.
    let mut config = base.clone();
    config["admin"]["listen"] = serde_json::json!(format!("unix/{socket}|0666"));
    config["apps"]["http"]["servers"]["setu"]["listen"] = serde_json::json!([":8443"]);
    caddy.load(&config).await.expect("load base");
    let id = site_route_id(SHORT).expect("id");
    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20001))
        .await
        .expect("create");
    caddy
        .upsert_route(&id, "a.example.test", &site_upstream(20002))
        .await
        .expect("replace");
    caddy
        .upsert_route(
            PANEL_ROUTE_ID,
            "panel.example.test",
            "unix//run/setu/http.sock",
        )
        .await
        .expect("panel");
    assert_eq!(
        caddy.list_route_ids().await.expect("ids"),
        vec![id.clone(), PANEL_ROUTE_ID.to_string()]
    );
    let routes = caddy.routes().await.expect("routes");
    assert_eq!(
        routes[0]["handle"][0]["upstreams"][0]["dial"],
        "127.0.0.1:20002"
    );
    caddy.remove_route(&id).await.expect("remove");
    caddy.remove_route(&id).await.expect("remove missing");
    assert_eq!(
        caddy.list_route_ids().await.expect("ids"),
        vec![PANEL_ROUTE_ID.to_string()]
    );
}
