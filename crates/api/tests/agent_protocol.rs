//! Agent protocol integration tests (plan Task 8, spec §4): join,
//! long-poll commands, results completing `DbAgentClient::call`, heartbeats
//! and idempotent redelivery.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use setu_api::testing::TestServer;
use setu_jobs::testing::FakeAgent;
use setu_jobs::AgentClient;
use setu_testkit::TestDb;
use sqlx::Row;

/// Agent-side client: no cookie store (agents use bearer tokens, never
/// sessions); timeout above the longest `?wait=` used below.
fn agent_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .expect("agent client builds")
}

/// Register a server and join it; returns `(server_id, server_token)`.
async fn joined(srv: &TestServer, name: &str) -> (String, String) {
    let owner = srv.login_as_owner().await;
    let unique = format!("{name}-{}", setu_common::new_id()[..8].to_lowercase());
    let resp = owner
        .post(srv.url("/api/v1/servers"))
        .json(&json!({"name": unique}))
        .send()
        .await
        .expect("create server sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.expect("create body is json");
    let join_token = body["join_token"]
        .as_str()
        .expect("join_token is a string")
        .to_string();
    let server_id = body["server"]["id"]
        .as_str()
        .expect("server id is a string")
        .to_string();

    let join = agent_client()
        .post(srv.url("/agent/v1/join"))
        .json(&json!({
            "join_token": join_token,
            "hostname": "web-1",
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
    let join_body: Value = join.json().await.expect("join body is json");
    assert_eq!(
        join_body["server_id"].as_str().expect("server_id"),
        server_id
    );
    let server_token = join_body["server_token"]
        .as_str()
        .expect("server_token is a string")
        .to_string();
    assert!(
        server_token.starts_with("setu_srv_"),
        "server token must carry its kind prefix"
    );
    (server_id, server_token)
}

#[tokio::test]
async fn join_with_used_token_is_401() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;

    let resp = owner
        .post(srv.url("/api/v1/servers"))
        .json(&json!({"name": "join-once"}))
        .send()
        .await
        .expect("create server sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.expect("body is json");
    let join_token = body["join_token"].as_str().unwrap().to_string();

    let join_body = || {
        json!({
            "join_token": join_token,
            "hostname": "web-1",
            "arch": "x86_64",
            "os": "linux",
            "cpu_cores": 2,
            "memory_mb": 4096,
            "disk_mb": 51200,
        })
    };
    let agent = agent_client();
    let first = agent
        .post(srv.url("/agent/v1/join"))
        .json(&join_body())
        .send()
        .await
        .expect("first join sends");
    assert_eq!(first.status(), reqwest::StatusCode::OK);

    // The single-use token is spent: the retry is 401 with the envelope.
    let second = agent
        .post(srv.url("/agent/v1/join"))
        .json(&join_body())
        .send()
        .await
        .expect("second join sends");
    assert_eq!(second.status(), reqwest::StatusCode::UNAUTHORIZED);
    let err: Value = second.json().await.expect("error body is json");
    assert_eq!(err["error"]["code"], json!("unauthorized"));

    // A bogus token is 401 too (and never logs the token).
    let bogus = agent
        .post(srv.url("/agent/v1/join"))
        .json(&json!({
            "join_token": "setu_join_bogus",
            "hostname": "web-1",
            "arch": "x86_64",
            "os": "linux",
            "cpu_cores": 2,
            "memory_mb": 4096,
            "disk_mb": 51200,
        }))
        .send()
        .await
        .expect("bogus join sends");
    assert_eq!(bogus.status(), reqwest::StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn commands_long_poll_returns_when_enqueued() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (server_id, server_token) = joined(&srv, "long-poll").await;
    let agent = agent_client();

    // No bearer token at all is 401 with the envelope.
    let anon = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .send()
        .await
        .expect("anonymous poll sends");
    assert_eq!(anon.status(), reqwest::StatusCode::UNAUTHORIZED);
    let err: Value = anon.json().await.expect("error body is json");
    assert_eq!(err["error"]["code"], json!("unauthorized"));

    // Empty queue with wait=0 answers immediately with 204.
    let empty = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("empty poll sends");
    assert_eq!(empty.status(), reqwest::StatusCode::NO_CONTENT);

    // A wait=5 poll blocked in the handler must resolve well under a
    // second once a command is enqueued 200 ms later.
    let url = srv.url("/agent/v1/commands?wait=5");
    let waiting = tokio::spawn({
        let agent = agent_client();
        let server_token = server_token.clone();
        async move {
            agent
                .get(&url)
                .bearer_auth(&server_token)
                .send()
                .await
                .expect("long poll sends")
        }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    let start = Instant::now();
    let enqueued = srv
        .db
        .agent_commands()
        .enqueue(
            &server_id,
            &setu_proto::Command::Status {
                site_id: "site-1".to_string(),
            },
        )
        .await
        .expect("enqueue");
    let resp = waiting.await.expect("poll task joins");
    let elapsed = start.elapsed();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert!(
        elapsed < Duration::from_secs(1),
        "long poll must resolve in <1s after enqueue, took {elapsed:?}"
    );
    let body: Value = resp.json().await.expect("command body is json");
    assert_eq!(body["id"].as_str().expect("command id"), enqueued.id);
    assert_eq!(body["command"]["kind"], json!("status"));
}

#[tokio::test]
async fn result_completes_wait() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (server_id, server_token) = joined(&srv, "result-wait").await;

    // `DbAgentClient::call` (the production jobs path) blocks in `wait`
    // until the agent posts the result over HTTP.
    let caller = setu_jobs::DbAgentClient::new(srv.db.clone(), Duration::from_secs(10));
    let call = tokio::spawn(async move {
        caller
            .call(
                &server_id,
                setu_proto::Command::Status {
                    site_id: "site-9".to_string(),
                },
                Duration::from_secs(10),
            )
            .await
    });
    // Let `call` enqueue, then pick the command up as the agent would.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let agent = agent_client();
    let poll = agent
        .get(srv.url("/agent/v1/commands?wait=5"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("poll sends");
    assert_eq!(poll.status(), reqwest::StatusCode::OK);
    let cmd: Value = poll.json().await.expect("command body is json");
    let cmd_id = cmd["id"].as_str().expect("command id").to_string();

    // A result for an unknown id is 404 with the envelope.
    let missing = agent
        .post(srv.url("/agent/v1/commands/does-not-exist/result"))
        .bearer_auth(&server_token)
        .json(
            &json!({"id": "does-not-exist", "ok": true, "output": {}, "log": "ok", "error": null}),
        )
        .send()
        .await
        .expect("missing result sends");
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);

    // A body id that disagrees with the path is a 400.
    let mismatch = agent
        .post(srv.url(&format!("/agent/v1/commands/{cmd_id}/result")))
        .bearer_auth(&server_token)
        .json(&json!({"id": "some-other-id", "ok": true, "output": {}, "log": "ok", "error": null}))
        .send()
        .await
        .expect("mismatch result sends");
    assert_eq!(mismatch.status(), reqwest::StatusCode::BAD_REQUEST);

    let posted = agent
        .post(srv.url(&format!("/agent/v1/commands/{cmd_id}/result")))
        .bearer_auth(&server_token)
        .json(&json!({"id": cmd_id, "ok": true, "output": {"active": true}, "log": "running", "error": null}))
        .send()
        .await
        .expect("result sends");
    assert_eq!(posted.status(), reqwest::StatusCode::NO_CONTENT);

    let result = call.await.expect("call task joins").expect("call resolves");
    assert!(result.ok);
    assert_eq!(result.output, json!({"active": true}));
}

#[tokio::test]
async fn heartbeat_marks_online_and_stores_metrics() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (server_id, server_token) = joined(&srv, "heartbeat").await;

    // Metrics reference sites (FK), so create the app + site rows first.
    srv.db
        .apps()
        .upsert_version(&json!({"app": {"id": "hello", "name": "Hello", "version": "0.1.0"}}))
        .await
        .expect("upsert app");
    let plan = srv.db.plans().seed_default().await.expect("seed plan");
    let port = srv.db.sites().next_port(&server_id).await.expect("port");
    let site = srv
        .db
        .sites()
        .create(&setu_db::NewSite {
            name: "hello-1".to_string(),
            app_id: "hello".to_string(),
            app_version: "0.1.0".to_string(),
            server_id: server_id.clone(),
            plan_id: plan.id.clone(),
            domain: "hello-1.example.com".to_string(),
            port,
            data_dir: "/srv/setu/sites/hello-1/data".to_string(),
            db_name: "site_hello1".to_string(),
            env: json!({}),
            owner_email: "owner@example.com".to_string(),
        })
        .await
        .expect("create site");

    let agent = agent_client();
    let hb = agent
        .post(srv.url("/agent/v1/heartbeat"))
        .bearer_auth(&server_token)
        .json(&json!({
            "agent_version": "0.1.0",
            "load1": 0.5,
            "mem_free_mb": 1024,
            "disk_free_mb": 2048,
            "units": [],
            "metrics": [{
                "site_id": site.id,
                "at": chrono::Utc::now().to_rfc3339(),
                "cpu_seconds": 1.5,
                "memory_bytes": 1024,
                "disk_bytes": 2048,
            }],
        }))
        .send()
        .await
        .expect("heartbeat sends");
    assert_eq!(hb.status(), reqwest::StatusCode::NO_CONTENT);

    // The server reads back as online (last_seen within 60 s).
    let owner = srv.login_as_owner().await;
    let got = owner
        .get(srv.url(&format!("/api/v1/servers/{server_id}")))
        .send()
        .await
        .expect("get server sends");
    assert_eq!(got.status(), reqwest::StatusCode::OK);
    let got_body: Value = got.json().await.expect("server body is json");
    assert_eq!(got_body["status"], json!("online"));

    // The metrics batch landed in `site_metrics`.
    let row = sqlx::query("SELECT COUNT(*) AS c FROM site_metrics WHERE site_id = $1")
        .bind(&site.id)
        .fetch_one(srv.db.pool())
        .await
        .expect("count metrics");
    let count: i64 = row.try_get("c").expect("count column");
    assert_eq!(count, 1, "heartbeat metrics batch must be stored");
}

#[tokio::test]
async fn redelivered_command_is_idempotent() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (server_id, server_token) = joined(&srv, "redeliver").await;
    let agent = agent_client();

    let enqueued = srv
        .db
        .agent_commands()
        .enqueue(
            &server_id,
            &setu_proto::Command::Status {
                site_id: "site-1".to_string(),
            },
        )
        .await
        .expect("enqueue");

    // First delivery (no result is ever posted: the agent "restarted").
    let first = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("first poll sends");
    assert_eq!(first.status(), reqwest::StatusCode::OK);
    let first_body: Value = first.json().await.expect("body is json");
    assert_eq!(first_body["id"].as_str().expect("id"), enqueued.id);

    // Nothing left until the stale delivery times out.
    let drained = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("drain poll sends");
    assert_eq!(drained.status(), reqwest::StatusCode::NO_CONTENT);

    // Age the delivery past the cutoff and requeue: the same command id
    // comes back with attempts = 2 (first delivery + redelivery).
    sqlx::query(
        "UPDATE agent_commands SET delivered_at = now() - INTERVAL '2 seconds' WHERE id = $1",
    )
    .bind(&enqueued.id)
    .execute(srv.db.pool())
    .await
    .expect("age delivery");
    let requeued = srv
        .db
        .agent_commands()
        .requeue_stale(Duration::from_secs(1))
        .await
        .expect("requeue stale");
    assert_eq!(requeued, 1);

    let second = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("second poll sends");
    assert_eq!(second.status(), reqwest::StatusCode::OK);
    let second_body: Value = second.json().await.expect("body is json");
    assert_eq!(second_body["id"].as_str().expect("id"), enqueued.id);
    assert_eq!(
        second_body["attempts"],
        json!(2),
        "redelivered command must carry attempts = 2"
    );
}

#[tokio::test]
async fn result_is_scoped_to_authed_server() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (_server_a, token_a) = joined(&srv, "scope-a").await;
    let (server_b, token_b) = joined(&srv, "scope-b").await;
    let agent = agent_client();

    let enqueued = srv
        .db
        .agent_commands()
        .enqueue(
            &server_b,
            &setu_proto::Command::Status {
                site_id: "site-1".to_string(),
            },
        )
        .await
        .expect("enqueue for B");

    // Server A cannot complete server B's command, even with the right id.
    let cross = agent
        .post(srv.url(&format!("/agent/v1/commands/{}/result", enqueued.id)))
        .bearer_auth(&token_a)
        .json(&json!({"id": enqueued.id, "ok": true, "output": {}, "log": "ok", "error": null}))
        .send()
        .await
        .expect("cross-server result sends");
    assert_eq!(cross.status(), reqwest::StatusCode::NOT_FOUND);
    let err: Value = cross.json().await.expect("error body is json");
    assert_eq!(err["error"]["code"], json!("not_found"));

    // B's command is still pending: B polls it back via `next_for`.
    let poll = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&token_b)
        .send()
        .await
        .expect("owner poll sends");
    assert_eq!(poll.status(), reqwest::StatusCode::OK);
    let body: Value = poll.json().await.expect("command body is json");
    assert_eq!(body["id"].as_str().expect("id"), enqueued.id);

    // And B itself can complete it.
    let own = agent
        .post(srv.url(&format!("/agent/v1/commands/{}/result", enqueued.id)))
        .bearer_auth(&token_b)
        .json(&json!({"id": enqueued.id, "ok": true, "output": {}, "log": "ok", "error": null}))
        .send()
        .await
        .expect("owner result sends");
    assert_eq!(own.status(), reqwest::StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn result_repeat_post_is_idempotent_no_overwrite() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (server_id, server_token) = joined(&srv, "idem-complete").await;
    let agent = agent_client();

    let enqueued = srv
        .db
        .agent_commands()
        .enqueue(
            &server_id,
            &setu_proto::Command::Status {
                site_id: "site-1".to_string(),
            },
        )
        .await
        .expect("enqueue");
    // Deliver so the command exists in-flight (not strictly required for
    // `complete_for`, but mirrors the real agent flow).
    let poll = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("poll sends");
    assert_eq!(poll.status(), reqwest::StatusCode::OK);

    let first = agent
        .post(srv.url(&format!("/agent/v1/commands/{}/result", enqueued.id)))
        .bearer_auth(&server_token)
        .json(&json!({"id": enqueued.id, "ok": true, "output": {"n": 1}, "log": "first", "error": null}))
        .send()
        .await
        .expect("first result sends");
    assert_eq!(first.status(), reqwest::StatusCode::NO_CONTENT);

    // Repeat post with a different payload: still 204, first writer wins.
    let second = agent
        .post(srv.url(&format!("/agent/v1/commands/{}/result", enqueued.id)))
        .bearer_auth(&server_token)
        .json(&json!({"id": enqueued.id, "ok": false, "output": {"n": 2}, "log": "second", "error": "x"}))
        .send()
        .await
        .expect("second result sends");
    assert_eq!(second.status(), reqwest::StatusCode::NO_CONTENT);

    let row = sqlx::query("SELECT result FROM agent_commands WHERE id = $1")
        .bind(&enqueued.id)
        .fetch_one(srv.db.pool())
        .await
        .expect("fetch result");
    let stored: Value = row.try_get("result").expect("result column");
    assert_eq!(stored["ok"], json!(true), "repeat must not overwrite");
    assert_eq!(stored["log"], json!("first"));
}

#[tokio::test]
async fn heartbeat_ignores_foreign_site_metrics() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (server_a, token_a) = joined(&srv, "hb-own").await;
    let (server_b, _token_b) = joined(&srv, "hb-foreign").await;

    srv.db
        .apps()
        .upsert_version(&json!({"app": {"id": "hello", "name": "Hello", "version": "0.1.0"}}))
        .await
        .expect("upsert app");
    let plan = srv.db.plans().seed_default().await.expect("seed plan");
    let mk_site = async |server_id: &str, name: &str, domain: &str, db_name: &str| {
        let port = srv.db.sites().next_port(server_id).await.expect("port");
        srv.db
            .sites()
            .create(&setu_db::NewSite {
                name: name.to_string(),
                app_id: "hello".to_string(),
                app_version: "0.1.0".to_string(),
                server_id: server_id.to_string(),
                plan_id: plan.id.clone(),
                domain: domain.to_string(),
                port,
                data_dir: format!("/srv/setu/sites/{name}/data"),
                db_name: db_name.to_string(),
                env: json!({}),
                owner_email: "owner@example.com".to_string(),
            })
            .await
            .expect("create site")
    };
    let unique = setu_common::new_id()[..8].to_lowercase();
    let site_own = mk_site(
        &server_a,
        &format!("own-{unique}"),
        &format!("own-{unique}.example.com"),
        &format!("site_own_{unique}"),
    )
    .await;
    let site_foreign = mk_site(
        &server_b,
        &format!("foreign-{unique}"),
        &format!("foreign-{unique}.example.com"),
        &format!("site_foreign_{unique}"),
    )
    .await;

    let at = chrono::Utc::now().to_rfc3339();
    let agent = agent_client();
    let hb = agent
        .post(srv.url("/agent/v1/heartbeat"))
        .bearer_auth(&token_a)
        .json(&json!({
            "agent_version": "0.1.0",
            "load1": 0.5,
            "mem_free_mb": 1024,
            "disk_free_mb": 2048,
            "units": [],
            "metrics": [
                {"site_id": site_own.id, "at": at, "cpu_seconds": 1.0, "memory_bytes": 1, "disk_bytes": 1},
                {"site_id": site_foreign.id, "at": at, "cpu_seconds": 9.0, "memory_bytes": 9, "disk_bytes": 9},
                {"site_id": "does-not-exist", "at": at, "cpu_seconds": 5.0, "memory_bytes": 5, "disk_bytes": 5},
            ],
        }))
        .send()
        .await
        .expect("heartbeat sends");
    assert_eq!(hb.status(), reqwest::StatusCode::NO_CONTENT);

    let count = async |site_id: &str| -> i64 {
        let row = sqlx::query("SELECT COUNT(*) AS c FROM site_metrics WHERE site_id = $1")
            .bind(site_id)
            .fetch_one(srv.db.pool())
            .await
            .expect("count metrics");
        row.try_get("c").expect("count column")
    };
    assert_eq!(count(&site_own.id).await, 1, "own-site metric is stored");
    assert_eq!(
        count(&site_foreign.id).await,
        0,
        "foreign-site metric must be ignored"
    );
    assert_eq!(
        count("does-not-exist").await,
        0,
        "unknown site_id must be ignored, not a 400"
    );
}

#[tokio::test]
async fn wait_param_validation() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let (_server_id, server_token) = joined(&srv, "wait-parse").await;
    let agent = agent_client();

    for bad in ["?wait=abc", "?wait=30ss", "?wait=-1", "?wait=s"] {
        let resp = agent
            .get(srv.url(&format!("/agent/v1/commands{bad}")))
            .bearer_auth(&server_token)
            .send()
            .await
            .expect("bad wait sends");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{bad} must be 400"
        );
        let err: Value = resp.json().await.expect("error body is json");
        assert_eq!(err["error"]["code"], json!("validation"));
    }

    // `0` and `0s` poll once and return 204 immediately on an empty queue.
    for ok in ["?wait=0", "?wait=0s"] {
        let start = Instant::now();
        let resp = agent
            .get(srv.url(&format!("/agent/v1/commands{ok}")))
            .bearer_auth(&server_token)
            .send()
            .await
            .expect("ok wait sends");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::NO_CONTENT,
            "{ok} must be 204"
        );
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{ok} must return immediately"
        );
    }
}

#[tokio::test]
async fn agent_poll_rate_limit_429() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn_with_agent_poll_limit(tdb, FakeAgent::new(), 3).await;
    let (_server_id, server_token) = joined(&srv, "rate-limit").await;
    let agent = agent_client();

    for _ in 0..3 {
        let resp = agent
            .get(srv.url("/agent/v1/commands?wait=0"))
            .bearer_auth(&server_token)
            .send()
            .await
            .expect("poll sends");
        assert_eq!(resp.status(), reqwest::StatusCode::NO_CONTENT);
    }
    let limited = agent
        .get(srv.url("/agent/v1/commands?wait=0"))
        .bearer_auth(&server_token)
        .send()
        .await
        .expect("limited poll sends");
    assert_eq!(limited.status(), reqwest::StatusCode::TOO_MANY_REQUESTS);
    let err: Value = limited.json().await.expect("error body is json");
    assert_eq!(err["error"]["code"], json!("rate_limited"));
}
