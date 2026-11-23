// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Servers integration tests: authz, install command, cordon.

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
async fn post_servers_returns_install_command_and_separate_join_token() {
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
    assert_eq!(
        command,
        format!("setu agent install --control-plane {public_url}"),
        "the command names the panel and never carries the token"
    );
    assert!(!command.contains(join_token));

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
async fn cordon_toggles_and_is_audited() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let viewer = srv.login_as("viewer").await;
    let created: serde_json::Value = owner
        .post(srv.url("/api/v1/servers"))
        .json(&serde_json::json!({"name": "web-2"}))
        .send()
        .await
        .expect("create sends")
        .json()
        .await
        .expect("json");
    let id = created["server"]["id"].as_str().expect("id").to_string();
    assert_eq!(created["server"]["cordoned"], serde_json::json!(false));
    assert_eq!(
        created["server"]["is_control_plane"],
        serde_json::json!(false)
    );
    assert_eq!(created["server"]["load1"], serde_json::Value::Null);
    let url = srv.url(&format!("/api/v1/servers/{id}/cordon"));

    let denied = viewer
        .post(&url)
        .json(&serde_json::json!({"cordoned": true}))
        .send()
        .await
        .expect("viewer sends");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    for want in [true, false, true] {
        let resp = owner
            .post(&url)
            .json(&serde_json::json!({"cordoned": want}))
            .send()
            .await
            .expect("cordon sends");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: serde_json::Value = resp.json().await.expect("json");
        assert_eq!(body["cordoned"], serde_json::json!(want));
    }
    assert!(srv.db.servers().get(&id).await.expect("row").cordoned);
    let actions: Vec<String> = srv
        .db
        .audit()
        .list_recent(20)
        .await
        .expect("audit")
        .into_iter()
        .map(|a| a.action)
        .collect();
    assert_eq!(
        actions.iter().filter(|a| *a == "server.cordon").count(),
        2,
        "{actions:?}"
    );
    assert_eq!(
        actions.iter().filter(|a| *a == "server.uncordon").count(),
        1
    );
    let missing = owner
        .post(srv.url("/api/v1/servers/nope/cordon"))
        .json(&serde_json::json!({"cordoned": true}))
        .send()
        .await
        .expect("sends");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn server_logs_page_by_cursor_and_refuse_other_units() {
    let tdb = TestDb::new().await;
    let agent = FakeAgent::new();
    agent.set_log_lines(vec!["one".into(), "two".into(), "three".into()]);
    let srv = TestServer::spawn(tdb, agent).await;
    let owner = srv.login_as_owner().await;
    let viewer = srv.login_as("viewer").await;
    let created: serde_json::Value = owner
        .post(srv.url("/api/v1/servers"))
        .json(&serde_json::json!({"name": "web-logs"}))
        .send()
        .await
        .expect("create sends")
        .json()
        .await
        .expect("json");
    let id = created["server"]["id"].as_str().expect("id").to_string();
    srv.db
        .servers()
        .heartbeat(
            &id,
            &setu_db::HeartbeatInput {
                agent_version: "0.1.7",
                boot_id: None,
                load1: 0.0,
                mem_free_mb: 0,
                disk_free_mb: 0,
                metrics: &[],
            },
        )
        .await
        .expect("heartbeat");
    let url = srv.url(&format!("/api/v1/servers/{id}/logs"));

    let denied = viewer
        .get(&url)
        .query(&[("unit", "caddy")])
        .send()
        .await
        .expect("viewer sends");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    let first: serde_json::Value = owner
        .get(&url)
        .query(&[("unit", "caddy"), ("lines", "2")])
        .send()
        .await
        .expect("first sends")
        .json()
        .await
        .expect("json");
    assert_eq!(first["lines"], serde_json::json!(["one", "two"]));
    let cursor = first["cursor"].as_str().expect("cursor").to_string();
    let second: serde_json::Value = owner
        .get(&url)
        .query(&[("unit", "agent"), ("cursor", cursor.as_str())])
        .send()
        .await
        .expect("second sends")
        .json()
        .await
        .expect("json");
    assert_eq!(second["lines"], serde_json::json!(["three"]));

    for q in [
        vec![("unit", "sshd")],
        vec![("unit", "caddy"), ("cursor", "a\nb")],
        vec![],
    ] {
        let bad = owner.get(&url).query(&q).send().await.expect("bad sends");
        assert!(bad.status().is_client_error(), "{q:?} -> {}", bad.status());
    }
    let units: Vec<String> = srv
        .agent
        .commands_of("fetch_system_logs")
        .iter()
        .map(|c| c["unit"].as_str().unwrap_or_default().to_string())
        .collect();
    assert_eq!(
        units,
        ["caddy", "agent"],
        "refused requests never reach the agent"
    );
}

#[tokio::test]
async fn an_agent_older_than_0_1_7_is_not_asked_for_logs() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let created: serde_json::Value = owner
        .post(srv.url("/api/v1/servers"))
        .json(&serde_json::json!({"name": "old-logs"}))
        .send()
        .await
        .expect("create sends")
        .json()
        .await
        .expect("json");
    let id = created["server"]["id"].as_str().expect("id").to_string();
    srv.db
        .servers()
        .heartbeat(
            &id,
            &setu_db::HeartbeatInput {
                agent_version: "0.1.6",
                boot_id: None,
                load1: 0.0,
                mem_free_mb: 0,
                disk_free_mb: 0,
                metrics: &[],
            },
        )
        .await
        .expect("heartbeat");
    let res = owner
        .get(srv.url(&format!("/api/v1/servers/{id}/logs")))
        .query(&[("unit", "caddy")])
        .send()
        .await
        .expect("sends");
    assert_eq!(res.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(srv.agent.commands_of("fetch_system_logs").len(), 0);
}
