//! Servers integration tests: authz, install command, CSRF guard.

use setu_api::testing::TestServer;
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

#[tokio::test]
async fn viewer_cannot_create_server() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let viewer = srv.login_as("viewer").await;

    let resp = viewer
        .post(srv.url("/api/v1/servers"))
        .json(&serde_json::json!({"name": "web-1"}))
        .send()
        .await
        .expect("request sends");
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
    let body: serde_json::Value = resp.json().await.expect("body is json");
    assert_eq!(body["error"]["code"], serde_json::json!("forbidden"));
}

#[tokio::test]
async fn post_servers_returns_install_command_with_join_token() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;

    let resp = owner
        .post(srv.url("/api/v1/servers"))
        .json(&serde_json::json!({"name": "web-1"}))
        .send()
        .await
        .expect("request sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: serde_json::Value = resp.json().await.expect("body is json");

    let join_token = body["join_token"].as_str().expect("join_token is a string");
    assert!(
        join_token.starts_with("setu_join_"),
        "join token must carry its kind prefix, got: {join_token}"
    );
    let command = body["install_command"]
        .as_str()
        .expect("install_command is a string");
    let public_url = format!("http://{}", srv.addr);
    assert!(
        command.contains("setu agent install"),
        "install command must use the single binary, got: {command}"
    );
    assert!(
        command.contains(join_token),
        "install command must embed the join token, got: {command}"
    );
    assert!(
        command.contains(&public_url),
        "install command must embed SETU_PUBLIC_URL ({public_url}), got: {command}"
    );

    // The new server reads back as pending.
    let id = body["server"]["id"].as_str().expect("server id");
    let got = owner
        .get(srv.url(&format!("/api/v1/servers/{id}")))
        .send()
        .await
        .expect("get sends");
    assert_eq!(got.status(), reqwest::StatusCode::OK);
    let got_body: serde_json::Value = got.json().await.expect("body is json");
    assert_eq!(got_body["status"], serde_json::json!("pending"));
}

#[tokio::test]
async fn mismatched_origin_is_rejected_with_403() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;

    let resp = owner
        .post(srv.url("/api/v1/servers"))
        .header("Origin", "https://evil.example")
        .json(&serde_json::json!({"name": "web-1"}))
        .send()
        .await
        .expect("request sends");
    assert_eq!(resp.status(), reqwest::StatusCode::FORBIDDEN);
}
