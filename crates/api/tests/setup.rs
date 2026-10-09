//! Setup wizard integration tests (plan Task 14, spec §8): the public
//! first-owner flow, the single-use guarantee, and the local-server step
//! whose install command joins a real agent.

use std::time::Duration;

use serde_json::{json, Value};
use setu_api::testing::{new_client, TestServer};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

#[tokio::test]
async fn setup_status_reflects_operator_count() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let anon = new_client();

    let resp = anon
        .get(srv.url("/api/v1/setup"))
        .send()
        .await
        .expect("status sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: Value = resp.json().await.expect("status body is json");
    assert_eq!(body["needed"], json!(true));
    assert!(
        body["public_url"].as_str().is_some_and(|u| !u.is_empty()),
        "status must expose the public URL for the wizard, got: {body}"
    );

    // After the owner exists, setup is no longer needed.
    let created = anon
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"email": "owner@example.com", "password": "correct-horse-1"}))
        .send()
        .await
        .expect("setup sends");
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);

    let again = anon
        .get(srv.url("/api/v1/setup"))
        .send()
        .await
        .expect("status sends");
    let body: Value = again.json().await.expect("status body is json");
    assert_eq!(body["needed"], json!(false));
}

#[tokio::test]
async fn setup_creates_owner_once() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let anon = new_client();

    let resp = anon
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"email": "owner@example.com", "password": "correct-horse-1"}))
        .send()
        .await
        .expect("setup sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.expect("setup body is json");
    assert_eq!(body["email"], json!("owner@example.com"));
    assert_eq!(body["role"], json!("owner"));

    // The new owner can log in with the same credentials.
    let login = anon
        .post(srv.url("/api/v1/auth/login"))
        .json(&json!({"email": "owner@example.com", "password": "correct-horse-1"}))
        .send()
        .await
        .expect("login sends");
    assert_eq!(login.status(), reqwest::StatusCode::OK);

    // A second setup attempt is 409 — for anonymous callers too (the
    // endpoint is public-once, not authenticated).
    for client in [&anon, &srv.login_as_owner().await] {
        let second = client
            .post(srv.url("/api/v1/setup"))
            .json(&json!({"email": "other@example.com", "password": "correct-horse-2"}))
            .send()
            .await
            .expect("second setup sends");
        assert_eq!(second.status(), reqwest::StatusCode::CONFLICT);
        let err: Value = second.json().await.expect("error body is json");
        assert_eq!(err["error"]["code"], json!("conflict"));
    }
}

#[tokio::test]
async fn setup_validates_owner_input() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let anon = new_client();

    for (email, password) in [
        ("not-an-email", "correct-horse-1"),
        ("owner@example.com", "short"),
    ] {
        let resp = anon
            .post(srv.url("/api/v1/setup"))
            .json(&json!({"email": email, "password": password}))
            .send()
            .await
            .expect("setup sends");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "setup({email:?}) must be 400"
        );
    }
    // No operator was created by the invalid attempts: setup still needed.
    assert_eq!(srv.db.operators().count().await.expect("count"), 0);
}

#[tokio::test]
async fn setup_local_server_returns_install_command() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let anon = new_client();

    // Anonymous callers cannot register the local server.
    let denied = anon
        .post(srv.url("/api/v1/setup/local-server"))
        .json(&json!({}))
        .send()
        .await
        .expect("anon local-server sends");
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Full wizard flow: create owner, log in, register local server.
    let created = anon
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"email": "owner@example.com", "password": "correct-horse-1"}))
        .send()
        .await
        .expect("setup sends");
    assert_eq!(created.status(), reqwest::StatusCode::CREATED);
    let login = srv
        .login(&anon, "owner@example.com", "correct-horse-1")
        .await;
    assert_eq!(login.status(), reqwest::StatusCode::OK);

    let resp = anon
        .post(srv.url("/api/v1/setup/local-server"))
        .json(&json!({}))
        .send()
        .await
        .expect("local-server sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.expect("local-server body is json");

    // Default name is the machine hostname, never empty.
    let name = body["server"]["name"].as_str().expect("server name");
    assert!(!name.is_empty(), "local server must be named, got: {body}");
    assert_eq!(body["server"]["status"], json!("pending"));

    let join_token = body["join_token"].as_str().expect("join_token is a string");
    assert!(join_token.starts_with("setu_join_"));
    let command = body["install_command"]
        .as_str()
        .expect("install_command is a string");
    assert!(
        command.contains("setu agent install"),
        "install command must reuse the single binary, got: {command}"
    );
    assert!(
        command.contains(join_token),
        "install command must embed the join token, got: {command}"
    );

    // The join token really joins (agent side, no session).
    let agent = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("agent client builds");
    let join = agent
        .post(srv.url("/agent/v1/join"))
        .json(&json!({
            "join_token": join_token,
            "nonce": "setup-nonce",
            "hostname": name,
            "arch": "x86_64",
            "os": "linux",
            "cpu_cores": 4,
            "memory_mb": 8192,
            "disk_mb": 102400,
        }))
        .send()
        .await
        .expect("join sends");
    assert_eq!(join.status(), reqwest::StatusCode::OK);

    // An explicit name overrides the hostname default.
    let named = anon
        .post(srv.url("/api/v1/setup/local-server"))
        .json(&json!({"name": "db-1"}))
        .send()
        .await
        .expect("named local-server sends");
    assert_eq!(named.status(), reqwest::StatusCode::CREATED);
    let named_body: Value = named.json().await.expect("named body is json");
    assert_eq!(named_body["server"]["name"], json!("db-1"));
}

/// Two concurrent `POST /setup` with *different* emails: exactly one
/// wins with 201, the other gets 409 (the advisory-locked insert, not
/// just `UNIQUE(email)`).
#[tokio::test]
async fn setup_concurrent_posts_create_one_owner() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let url = srv.url("/api/v1/setup");

    let post = |email: &str| {
        let client = new_client();
        let url = url.clone();
        let body = json!({"email": email, "password": "correct-horse-1"});
        async move {
            client
                .post(url)
                .json(&body)
                .send()
                .await
                .expect("setup sends")
                .status()
        }
    };
    let (a, b) = tokio::join!(post("first@example.com"), post("second@example.com"),);
    // Exactly one 201 and one 409, in either order (`StatusCode` has
    // no numeric `Ord`, so compare raw codes).
    let mut codes = [a.as_u16(), b.as_u16()];
    codes.sort_unstable();
    assert_eq!(
        codes,
        [201, 409],
        "exactly one owner may be created, got {codes:?}"
    );
    // A slow `count()` is fine here: no more writers remain.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(srv.db.operators().count().await.expect("count"), 1);
}
