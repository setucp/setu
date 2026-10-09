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
port_env = "PORT"
admin_path = "/"

[run]
kind = "binary"

[run.binary]
args = ["serve"]
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

    for path in ["/nope", "/api/v1/nope"] {
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
