// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Apps integration tests: validator endpoint and URL-publish SSRF guards.

use setu_api::testing::TestServer;
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

const HELLO_TOML: &str = r#"schema = 1

[app]
id = "hello"
name = "Hello"
version = "0.1.0"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"

[run]
kind = "binary"
args = ["serve"]

[run.binary]
"x86_64-linux" = { url = "https://example.com/hello-0.1.0-x86_64.tar.gz", sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", exec = "hello" }

[needs]
dirs = []

[env]
HELLO_PORT = { from = "port" }
HELLO_NAME = { prompt = "Display name", secret = false }

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = false

[update]
strategy = "replace"
"#;

const BAD_TOML: &str = r#"schema = 1

[app]
id = "BAD ID WITH SPACES"
name = "Bad"
colour = "not a field"
"#;

#[tokio::test]
async fn post_apps_validate_reports_errors() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;

    let bad = owner
        .post(srv.url("/api/v1/apps/validate"))
        .json(&serde_json::json!({"toml": BAD_TOML}))
        .send()
        .await
        .expect("request sends");
    assert_eq!(bad.status(), reqwest::StatusCode::OK);
    let bad_body: serde_json::Value = bad.json().await.expect("body is json");
    assert_eq!(bad_body["ok"], serde_json::json!(false));
    assert!(
        bad_body["errors"].as_array().is_some_and(|e| !e.is_empty()),
        "bad toml must produce errors, got: {bad_body}"
    );

    let good = owner
        .post(srv.url("/api/v1/apps/validate"))
        .json(&serde_json::json!({"toml": HELLO_TOML}))
        .send()
        .await
        .expect("request sends");
    assert_eq!(good.status(), reqwest::StatusCode::OK);
    let good_body: serde_json::Value = good.json().await.expect("body is json");
    assert_eq!(good_body["ok"], serde_json::json!(true));
}

#[tokio::test]
async fn post_apps_from_url_refuses_private_hosts() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;

    for url in [
        "http://127.0.0.1:9/manifest.toml",
        "http://localhost:9/manifest.toml",
        "http://10.0.0.5/manifest.toml",
    ] {
        let resp = owner
            .post(srv.url("/api/v1/apps"))
            .json(&serde_json::json!({"url": url}))
            .send()
            .await
            .expect("request sends");
        assert!(
            resp.status() == reqwest::StatusCode::BAD_REQUEST
                || resp.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY,
            "private-host url {url} must be refused, got {}",
            resp.status()
        );
    }

    // A pasted manifest still publishes fine.
    let resp = owner
        .post(srv.url("/api/v1/apps"))
        .json(&serde_json::json!({"toml": HELLO_TOML}))
        .send()
        .await
        .expect("request sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = resp.json().await.expect("body is json");
    assert_eq!(body["app"]["id"], serde_json::json!("hello"));
    assert_eq!(body["version"]["version"], serde_json::json!("0.1.0"));
}

#[tokio::test]
async fn malformed_json_uses_the_validation_envelope() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;

    let resp = owner
        .post(srv.url("/api/v1/apps"))
        .header("Content-Type", "application/json")
        .body("{not valid json")
        .send()
        .await
        .expect("request sends");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: serde_json::Value = resp.json().await.expect("body is json");
    assert_eq!(body["error"]["code"], serde_json::json!("validation"));
}

#[tokio::test]
async fn unknown_route_uses_the_not_found_envelope() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;

    // Unknown API paths always keep the envelope — including when the UI
    // is embedded (plain `/nope` serves the SPA shell then; see the
    // `fallback` unit tests in `embed.rs` for both build modes).
    for path in ["/api/v1/nope", "/agent/v1/nope"] {
        let resp = srv
            .client
            .get(srv.url(path))
            .send()
            .await
            .expect("request sends");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NOT_FOUND,
            "GET {path} must be 404"
        );
        let body: serde_json::Value = resp.json().await.expect("body is json");
        assert_eq!(body["error"]["code"], serde_json::json!("not_found"));
    }
}

/// Serves manifests from memory by URL (the real fetcher refuses local
/// addresses), so followed URLs can change between checks.
#[derive(Debug, Default)]
struct MemFetcher(std::sync::Mutex<std::collections::HashMap<String, String>>);

impl MemFetcher {
    fn put(&self, url: &str, toml: &str) {
        self.0.lock().unwrap().insert(url.into(), toml.into());
    }
}

#[async_trait::async_trait]
impl setu_api::follow::ManifestFetcher for MemFetcher {
    async fn fetch(&self, url: &str) -> Result<String, setu_common::AppError> {
        self.0.lock().unwrap().get(url).cloned().ok_or_else(|| {
            setu_common::AppError::validation("manifest fetch failed with status 404")
        })
    }
}

fn hello(version: &str, sha: &str) -> String {
    HELLO_TOML
        .replace("version = \"0.1.0\"", &format!("version = \"{version}\""))
        .replace(
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            sha,
        )
}

const URL: &str = "https://manifests.example/hello.toml";
const SHA_A: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

async fn follow_server() -> (TestServer, std::sync::Arc<MemFetcher>) {
    let fetcher = std::sync::Arc::new(MemFetcher::default());
    let srv = TestServer::spawn_with(
        TestDb::new().await,
        FakeAgent::new(),
        setu_api::testing::TestOptions {
            fetcher: Some(fetcher.clone()),
            ..Default::default()
        },
    )
    .await;
    (srv, fetcher)
}

/// 0.1.5 §1: publishing from a URL follows it; a check publishes a new
/// version, never replaces a published one, and records its outcome.
#[tokio::test]
async fn published_url_is_followed_and_checked() {
    let (srv, fetcher) = follow_server().await;
    let owner = srv.login_as_owner().await;
    fetcher.put(URL, &hello("0.1.0", SHA_A));

    let resp = owner
        .post(srv.url("/api/v1/apps"))
        .json(&serde_json::json!({ "url": URL }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["app"]["source"]["url"], URL);

    // Nothing new: no version published, the check is recorded.
    let check = |o: &reqwest::Client| o.post(srv.url("/api/v1/apps/hello/check")).send();
    let out: serde_json::Value = check(&owner).await.unwrap().json().await.unwrap();
    assert_eq!(out["published"], serde_json::Value::Null);
    assert!(out["source"]["checked_at"].is_string());
    assert_eq!(out["source"]["last_error"], serde_json::Value::Null);

    // The same version with other binaries is not re-published.
    fetcher.put(URL, &hello("0.1.0", SHA_B));
    let out: serde_json::Value = check(&owner).await.unwrap().json().await.unwrap();
    assert_eq!(out["published"], serde_json::Value::Null);
    let v = srv.db.apps().version("hello", "0.1.0").await.unwrap();
    assert!(
        v.manifest.to_string().contains(SHA_A),
        "published manifest kept"
    );

    // A new version is published and audited without an operator.
    fetcher.put(URL, &hello("0.2.0", SHA_B));
    let out: serde_json::Value = check(&owner).await.unwrap().json().await.unwrap();
    assert_eq!(out["published"], "0.2.0");
    srv.db.apps().version("hello", "0.2.0").await.unwrap();
    let audit = srv.db.audit().list_recent(5).await.unwrap();
    let entry = audit
        .iter()
        .find(|e| e.target_id.as_deref() == Some("hello/0.2.0 (followed)"));
    assert!(entry.is_some_and(|e| e.action == "app.publish" && e.operator_id.is_none()));

    // The app list carries the source.
    let list: serde_json::Value = owner
        .get(srv.url("/api/v1/apps"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["apps"][0]["source"]["url"], URL);

    // A failing fetch is recorded and reported.
    fetcher.0.lock().unwrap().clear();
    let resp = check(&owner).await.unwrap();
    assert!(resp.status().is_client_error());
    let src = srv.db.app_sources().get("hello").await.unwrap().unwrap();
    assert!(src.last_error.unwrap().contains("404"));

    // Stop following: the source is gone, a second clear is 404.
    let del = |o: &reqwest::Client| o.delete(srv.url("/api/v1/apps/hello/source")).send();
    assert_eq!(del(&owner).await.unwrap().status(), 204);
    assert_eq!(del(&owner).await.unwrap().status(), 404);
    assert_eq!(check(&owner).await.unwrap().status(), 404);
}

/// A followed URL that starts serving another app is refused.
#[tokio::test]
async fn followed_url_must_keep_its_app_id() {
    let (srv, fetcher) = follow_server().await;
    let owner = srv.login_as_owner().await;
    fetcher.put(URL, &hello("0.1.0", SHA_A));
    let resp = owner
        .post(srv.url("/api/v1/apps"))
        .json(&serde_json::json!({ "url": URL }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);

    fetcher.put(
        URL,
        &hello("0.2.0", SHA_A).replace("id = \"hello\"", "id = \"other\""),
    );
    let resp = owner
        .post(srv.url("/api/v1/apps/hello/check"))
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_client_error());
    let src = srv.db.app_sources().get("hello").await.unwrap().unwrap();
    assert!(src.last_error.unwrap().contains("not hello"));
    assert!(srv.db.apps().version("other", "0.2.0").await.is_err());
}

/// The scheduled round checks every followed app.
#[tokio::test]
async fn check_all_publishes_new_versions() {
    let (srv, fetcher) = follow_server().await;
    let owner = srv.login_as_owner().await;
    fetcher.put(URL, &hello("0.1.0", SHA_A));
    owner
        .post(srv.url("/api/v1/apps"))
        .json(&serde_json::json!({ "url": URL }))
        .send()
        .await
        .unwrap();
    fetcher.put(URL, &hello("0.3.0", SHA_A));
    setu_api::follow::check_all(&srv.db, fetcher.as_ref(), &setu_jobs::SystemClock).await;
    srv.db.apps().version("hello", "0.3.0").await.unwrap();
}

/// Viewers cannot check or stop following.
#[tokio::test]
async fn follow_endpoints_need_an_operator() {
    let (srv, _fetcher) = follow_server().await;
    let viewer = srv.login_as("viewer").await;
    let resp = viewer
        .post(srv.url("/api/v1/apps/hello/check"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
    let resp = viewer
        .delete(srv.url("/api/v1/apps/hello/source"))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 403);
}
