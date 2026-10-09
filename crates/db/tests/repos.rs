//! Repository tests over throwaway databases (`setu-testkit::TestDb`).
//!
//! Each test gets its own database, so tests run in parallel safely.
//! The concurrent-port test shares one pool across 20 tasks on purpose.

use std::time::Duration;

use setu_common::ErrorCode;
use setu_db::{Db, NewSite};
use setu_proto::{Command, CommandResult, Heartbeat, JoinRequest};
use setu_testkit::TestDb;

async fn test_db() -> (TestDb, Db) {
    let test_db = TestDb::new().await;
    let db = Db::new(test_db.pool.clone());
    (test_db, db)
}

fn manifest_json(app: &str, version: &str) -> serde_json::Value {
    serde_json::json!({
        "app": {
            "id": app,
            "name": app,
            "version": version,
            "homepage": "https://example.com",
        }
    })
}

fn join_request(join_token: &str) -> JoinRequest {
    JoinRequest {
        join_token: join_token.to_string(),
        hostname: "web1".to_string(),
        arch: "x86_64".to_string(),
        os: "linux".to_string(),
        cpu_cores: 4,
        memory_mb: 8192,
        disk_mb: 102_400,
    }
}

fn test_heartbeat() -> Heartbeat {
    Heartbeat {
        agent_version: "0.1.0".to_string(),
        load1: 0.5,
        mem_free_mb: 1024,
        disk_free_mb: 2048,
        units: vec![],
        metrics: vec![],
    }
}

/// App + plan rows every site test needs.
async fn catalogue(db: &Db, app: &str, version: &str) -> String {
    db.apps()
        .upsert_version(&manifest_json(app, version))
        .await
        .expect("upsert app version");
    db.plans().seed_default().await.expect("seed plan").id
}

#[allow(clippy::too_many_arguments)]
async fn new_site(
    db: &Db,
    name: &str,
    server_id: &str,
    plan_id: &str,
    domain: &str,
    port: i32,
) -> setu_db::Site {
    db.sites()
        .create(&NewSite {
            name: name.to_string(),
            app_id: "hello".to_string(),
            app_version: "0.1.0".to_string(),
            server_id: server_id.to_string(),
            plan_id: plan_id.to_string(),
            domain: domain.to_string(),
            port,
            data_dir: format!("/srv/setu/sites/{name}"),
            db_name: format!("site_{name}"),
            env: serde_json::json!({}),
            owner_email: "owner@example.com".to_string(),
        })
        .await
        .expect("create site")
}

#[tokio::test]
async fn operators_create_count_and_by_email() {
    let _test = test_db().await.0;
    let db = Db::new(_test.pool.clone());
    assert_eq!(db.operators().count().await.unwrap(), 0);
    let op = db
        .operators()
        .create("owner@example.com", "argon2:hash", "owner")
        .await
        .unwrap();
    assert_eq!(op.email, "owner@example.com");
    assert_eq!(db.operators().count().await.unwrap(), 1);
    let found = db
        .operators()
        .by_email("owner@example.com")
        .await
        .unwrap()
        .expect("operator by email");
    assert_eq!(found.id, op.id);
    // Duplicate email is a conflict, not an internal error.
    let err = db
        .operators()
        .create("owner@example.com", "argon2:other", "viewer")
        .await
        .expect_err("duplicate email must fail");
    assert_eq!(err.code, ErrorCode::Conflict);
}

#[tokio::test]
async fn join_token_is_single_use() {
    let (_test, db) = test_db().await;
    let (pending, join_token) = db.servers().create_pending("web1").await.unwrap();
    assert_eq!(pending.status, "pending");

    let (server, server_token) = db
        .servers()
        .join(&join_token, &join_request(&join_token))
        .await
        .expect("first join succeeds");
    assert!(!server_token.is_empty());

    // The server token authenticates from here on.
    let authed = db
        .servers()
        .by_token(&server_token)
        .await
        .expect("by_token")
        .expect("server found by token");
    assert_eq!(authed.id, server.id);
    assert!(db.servers().by_token("bogus").await.unwrap().is_none());

    // Redeeming the same join token again is 401.
    let err = db
        .servers()
        .join(&join_token, &join_request(&join_token))
        .await
        .expect_err("second join must fail");
    assert_eq!(err.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn join_token_expires() {
    let (_test, db) = test_db().await;
    let (pending, join_token) = db.servers().create_pending("web1").await.unwrap();
    sqlx::query(
        "UPDATE servers SET join_token_expires_at = now() - INTERVAL '1 hour' WHERE id = $1",
    )
    .bind(&pending.id)
    .execute(&_test.pool)
    .await
    .unwrap();
    let err = db
        .servers()
        .join(&join_token, &join_request(&join_token))
        .await
        .expect_err("expired join must fail");
    assert_eq!(err.code, ErrorCode::Unauthorized);
}

#[tokio::test]
async fn heartbeat_marks_online_and_stores_metrics() {
    let (_test, db) = test_db().await;
    let (pending, join_token) = db.servers().create_pending("web1").await.unwrap();
    let (server, _) = db
        .servers()
        .join(&join_token, &join_request(&join_token))
        .await
        .unwrap();
    // Join proves liveness: the server counts as online immediately.
    assert_eq!(db.servers().online().await.unwrap().len(), 1);

    let updated = db
        .servers()
        .heartbeat(&server.id, &test_heartbeat())
        .await
        .unwrap();
    assert_eq!(updated.status, "online");
    assert!(updated.last_seen_at.is_some());

    // A stale last_seen drops out of online() but the row stays listed.
    sqlx::query("UPDATE servers SET last_seen_at = now() - INTERVAL '5 minutes' WHERE id = $1")
        .bind(&pending.id)
        .execute(&_test.pool)
        .await
        .unwrap();
    assert!(db.servers().online().await.unwrap().is_empty());
    assert_eq!(db.servers().list().await.unwrap().len(), 1);
}

#[tokio::test]
async fn next_port_skips_taken_ports() {
    let (_test, db) = test_db().await;
    let server = db.servers().create_pending("web1").await.unwrap().0;
    let plan_id = catalogue(&db, "hello", "0.1.0").await;

    let first = db.sites().next_port(&server.id).await.unwrap();
    assert_eq!(first, 20000);
    new_site(&db, "a", &server.id, &plan_id, "a.example.com", first).await;

    let second = db.sites().next_port(&server.id).await.unwrap();
    assert_eq!(second, 20001);
    new_site(&db, "b", &server.id, &plan_id, "b.example.com", second).await;

    // A gap is reused before the high-water mark advances.
    sqlx::query("DELETE FROM sites WHERE domain = $1")
        .bind("a.example.com")
        .execute(&_test.pool)
        .await
        .unwrap();
    assert_eq!(db.sites().next_port(&server.id).await.unwrap(), 20000);
}

#[tokio::test]
async fn ports_are_unique_under_concurrent_provisioning() {
    let (_test, db) = test_db().await;
    let server = db.servers().create_pending("web1").await.unwrap().0;
    let plan_id = catalogue(&db, "hello", "0.1.0").await;
    let db = std::sync::Arc::new(db);

    let mut handles = Vec::new();
    for i in 0..20 {
        let db = db.clone();
        let server_id = server.id.clone();
        let plan_id = plan_id.clone();
        handles.push(tokio::spawn(async move {
            let name = format!("s{i:02}");
            // next_port can race with a concurrent insert; the
            // UNIQUE(server_id, port) constraint rejects the loser,
            // which retries until it holds a free port.
            loop {
                let port = db.sites().next_port(&server_id).await.unwrap();
                match db
                    .sites()
                    .create(&NewSite {
                        name: name.clone(),
                        app_id: "hello".to_string(),
                        app_version: "0.1.0".to_string(),
                        server_id: server_id.clone(),
                        plan_id: plan_id.clone(),
                        domain: format!("{name}.example.com"),
                        port,
                        data_dir: format!("/srv/setu/sites/{name}"),
                        db_name: format!("site_{name}"),
                        env: serde_json::json!({}),
                        owner_email: "owner@example.com".to_string(),
                    })
                    .await
                {
                    Ok(_) => return port,
                    Err(e) if e.code == ErrorCode::Conflict => continue,
                    Err(e) => panic!("unexpected site create error: {e}"),
                }
            }
        }));
    }
    let mut ports = Vec::new();
    for h in handles {
        ports.push(h.await.expect("task joins"));
    }
    ports.sort_unstable();
    let mut deduped = ports.clone();
    deduped.dedup();
    assert_eq!(ports.len(), 20, "all tasks provisioned");
    assert_eq!(deduped.len(), 20, "ports are all distinct: {ports:?}");
}

#[tokio::test]
async fn claim_skips_locked_jobs() {
    let (_test, db) = test_db().await;
    let j1 = db
        .jobs()
        .enqueue("provision_site", None, None, None)
        .await
        .unwrap();
    let j2 = db
        .jobs()
        .enqueue("provision_site", None, None, None)
        .await
        .unwrap();

    let jobs = db.jobs();
    let (c1, c2) = tokio::join!(jobs.claim("w1"), jobs.claim("w2"));
    let c1 = c1.unwrap().expect("first claim hits");
    let c2 = c2.unwrap().expect("second claim hits");
    assert_ne!(c1.id, c2.id, "concurrent claims hold different jobs");
    assert!(c1.id == j1.id || c1.id == j2.id);
    assert!(c2.id == j1.id || c2.id == j2.id);
    assert_eq!(c1.status, "running");
    assert!(db.jobs().claim("w3").await.unwrap().is_none());

    db.jobs().append_log(&c1.id, "step one\n").await.unwrap();
    db.jobs().finish(&c1.id, true, None).await.expect("finish");
    let done = db.jobs().get(&c1.id).await.unwrap();
    assert_eq!(done.status, "completed");
    assert!(done.log.contains("step one"));
}

#[tokio::test]
async fn requeue_stale_requeues_only_delivered_uncompleted() {
    let (_test, db) = test_db().await;
    let server = db.servers().create_pending("web1").await.unwrap().0;
    let cmd = || Command::Status {
        site_id: "site-1".to_string(),
    };

    // Never delivered: untouched by requeue.
    let _fresh = db
        .agent_commands()
        .enqueue(&server.id, &cmd())
        .await
        .unwrap();
    // Delivered long ago, never completed: requeued.
    let stale = db
        .agent_commands()
        .enqueue(&server.id, &cmd())
        .await
        .unwrap();
    let first = db
        .agent_commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .expect("delivers the oldest undelivered");
    assert_eq!(first.id, _fresh.id);
    let second = db
        .agent_commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .expect("delivers the next");
    assert_eq!(second.id, stale.id);
    // Delivered and completed: untouched by requeue.
    let done = db
        .agent_commands()
        .enqueue(&server.id, &cmd())
        .await
        .unwrap();
    db.agent_commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .expect("delivers the third");
    db.agent_commands()
        .complete(
            &done.id,
            &CommandResult {
                id: done.id.clone(),
                ok: true,
                output: serde_json::json!({}),
                log: "ok".to_string(),
                error: None,
            },
        )
        .await
        .unwrap();

    // Age only the stale delivery past the cutoff.
    sqlx::query(
        "UPDATE agent_commands SET delivered_at = now() - INTERVAL '2 hours' WHERE id = $1",
    )
    .bind(&stale.id)
    .execute(&_test.pool)
    .await
    .unwrap();

    let requeued = db
        .agent_commands()
        .requeue_stale(Duration::from_secs(3600))
        .await
        .unwrap();
    assert_eq!(requeued, 1);

    // The stale command is deliverable again; the fresh one (never
    // delivered) and the completed one are not duplicated.
    let redelivered = db
        .agent_commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .expect("stale command redelivered");
    assert_eq!(redelivered.id, stale.id);
    assert!(db
        .agent_commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .is_none());

    // wait() resolves once a result lands.
    let pending = db
        .agent_commands()
        .enqueue(&server.id, &cmd())
        .await
        .unwrap();
    let waiter = {
        let db = Db::new(_test.pool.clone());
        let id = pending.id.clone();
        tokio::spawn(async move { db.agent_commands().wait(&id, Duration::from_secs(10)).await })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    db.agent_commands()
        .complete(
            &pending.id,
            &CommandResult {
                id: pending.id.clone(),
                ok: false,
                output: serde_json::json!({}),
                log: "boom".to_string(),
                error: Some("boom".to_string()),
            },
        )
        .await
        .unwrap();
    let result = waiter.await.expect("waiter joins").expect("wait resolves");
    assert!(!result.ok);
    assert_eq!(result.error.as_deref(), Some("boom"));

    // wait() on an id nobody completes times out as agent_unreachable.
    let orphan = db
        .agent_commands()
        .enqueue(&server.id, &cmd())
        .await
        .unwrap();
    let err = db
        .agent_commands()
        .wait(&orphan.id, Duration::from_millis(600))
        .await
        .expect_err("wait must time out");
    assert_eq!(err.code, ErrorCode::AgentUnreachable);
}

#[tokio::test]
async fn site_domain_is_unique() {
    let (_test, db) = test_db().await;
    let server = db.servers().create_pending("web1").await.unwrap().0;
    let plan_id = catalogue(&db, "hello", "0.1.0").await;
    new_site(&db, "a", &server.id, &plan_id, "dup.example.com", 20000).await;

    let err = db
        .sites()
        .create(&NewSite {
            name: "b".to_string(),
            app_id: "hello".to_string(),
            app_version: "0.1.0".to_string(),
            server_id: server.id.clone(),
            plan_id: plan_id.clone(),
            domain: "dup.example.com".to_string(),
            port: 20001,
            data_dir: "/srv/setu/sites/b".to_string(),
            db_name: "site_b".to_string(),
            env: serde_json::json!({}),
            owner_email: "owner@example.com".to_string(),
        })
        .await
        .expect_err("duplicate domain must fail");
    assert_eq!(err.code, ErrorCode::Conflict);

    let site = db
        .sites()
        .by_domain("dup.example.com")
        .await
        .unwrap()
        .expect("by_domain finds the first site");
    assert_eq!(site.name, "a");
}
