// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Repository tests over throwaway databases (`setu-testkit::TestDb`),
//! plan Task 5. Each test gets its own database, so tests run in
//! parallel safely.

use std::collections::HashSet;
use std::time::Duration;

use setu_common::{ErrorCode, Sealer};
use setu_db::{CreateError, Db, JoinError, JoinInput, NewPlan, NewSite, WaitError};
use setu_proto::{Command, CommandResult, EnvVar};
use setu_testkit::TestDb;

async fn test_db() -> (TestDb, Db) {
    let test_db = TestDb::new().await;
    let db = Db::new(test_db.pool.clone());
    (test_db, db)
}

fn sealer() -> Sealer {
    let key = setu_common::config::SecretKey::from_hex(&"ab".repeat(32)).expect("key");
    Sealer::new(&key)
}

fn join_input(nonce: &str, memory_mb: i64) -> JoinInput {
    JoinInput {
        nonce: nonce.to_string(),
        hostname: "web1".to_string(),
        public_ip: None,
        arch: "x86_64".to_string(),
        os: "linux".to_string(),
        cpu_cores: 4,
        memory_mb,
        disk_mb: 102_400,
    }
}

async fn exec(test: &TestDb, sql: &str, id: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(id)
        .execute(&test.pool)
        .await
        .expect(sql);
}

/// A joined (hence online) server with `memory_mb`.
async fn online_server(db: &Db, memory_mb: i64, is_control_plane: bool) -> setu_db::Server {
    let (_, join_token) = db
        .servers()
        .create_pending("web", is_control_plane)
        .await
        .expect("create pending");
    db.servers()
        .join(&join_token, &join_input("n", memory_mb), &sealer())
        .await
        .expect("join")
        .0
}

/// App "hello" 0.1.0 plus a plan of `memory_mb`; returns the plan id.
async fn catalogue(db: &Db, memory_mb: i32) -> String {
    db.apps()
        .upsert_version(&serde_json::json!({
            "app": {"id": "hello", "name": "hello", "version": "0.1.0"}
        }))
        .await
        .expect("upsert app version");
    db.plans()
        .create(&NewPlan {
            name: format!("p{memory_mb}"),
            memory_mb,
            cpu_percent: 100,
            disk_mb: 1024,
            allowed_app_ids: None,
        })
        .await
        .expect("plan")
        .id
}

fn new_site(server_id: Option<&str>, plan_id: &str, domain: &str, mem: i64) -> NewSite {
    NewSite {
        name: domain.to_string(),
        app_id: "hello".to_string(),
        app_version: "0.1.0".to_string(),
        server_id: server_id.map(str::to_string),
        plan_id: plan_id.to_string(),
        domain: domain.to_string(),
        owner_email: "owner@example.com".to_string(),
        env_sealed: serde_json::json!([]),
        plan_memory_mb: mem,
    }
}

async fn site(db: &Db) -> setu_db::Site {
    let server = online_server(db, 100_000, false).await;
    let plan = catalogue(db, 512).await;
    db.sites()
        .create(&new_site(Some(&server.id), &plan, "a.example.com", 512))
        .await
        .expect("create site")
}

const PW: &str = "pw-SECRET-1";
const ENV_SECRET: &str = "env-SECRET-2";

fn secret_commands() -> [Command; 2] {
    [
        Command::CreateDatabase {
            db: "site_abcd1234".to_string(),
            role: "site_abcd1234".to_string(),
            password: PW.to_string(),
        },
        Command::WriteEnv {
            site: "abcd1234".to_string(),
            env: vec![EnvVar {
                name: "TOKEN".to_string(),
                value: ENV_SECRET.to_string(),
                secret: true,
                once: false,
            }],
        },
    ]
}

/// The stored command JSON holds neither secret, and the secret fields
/// are present but null.
async fn assert_redacted(db: &Db, id: &str) {
    let row = db.commands().get(id).await.expect("row");
    let text = row.command.to_string();
    assert!(
        !text.contains(PW) && !text.contains(ENV_SECRET),
        "secret survived: {}",
        row.status
    );
    match row.command["kind"].as_str() {
        Some("create_database") => assert!(row.command["password"].is_null()),
        Some("write_env") => assert!(row.command["env"][0]["value"].is_null()),
        other => panic!("unexpected kind {other:?}"),
    }
    // The SQL function and proto's agent-side `redact()` agree exactly.
    let original = secret_commands()
        .into_iter()
        .find(|c| Some(c.kind()) == row.command["kind"].as_str())
        .expect("known command");
    assert_eq!(
        row.command,
        original.redact(),
        "SQL and proto redaction differ"
    );
}

fn result(id: &str, ok: bool) -> CommandResult {
    CommandResult {
        id: id.to_string(),
        ok,
        output: serde_json::json!({}),
        log: String::new(),
        error: None,
    }
}

const TTL: Duration = Duration::from_secs(120);

// ---------------------------------------------------------------- sessions

#[tokio::test]
async fn session_round_trip_and_sliding_expiry() {
    let (test, db) = test_db().await;
    let op = db
        .operators()
        .create("Owner@Example.com", "argon2:hash", "owner")
        .await
        .unwrap();
    assert_eq!(op.email, "owner@example.com");
    let cookie = db.sessions().create(&op.id).await.unwrap();
    assert_eq!(cookie.len(), 43, "32 bytes base64url without padding");

    let (session, who) = db.sessions().load(&cookie).await.unwrap().expect("loads");
    assert_eq!(who.id, op.id);
    assert_ne!(session.id_hash, cookie, "only the hash is stored");
    let week = chrono::Duration::days(7);
    assert!(session.expires_at > chrono::Utc::now() + week - chrono::Duration::minutes(1));

    // Last refreshed 30 minutes ago: not refreshed yet.
    exec(
        &test,
        "UPDATE sessions SET expires_at = now() + interval '7 days' - interval '30 minutes' WHERE operator_id = $1",
        &op.id,
    )
    .await;
    let (s, _) = db.sessions().load(&cookie).await.unwrap().unwrap();
    assert!(s.expires_at < chrono::Utc::now() + week - chrono::Duration::minutes(29));

    // Last refreshed 2 hours ago: the expiry slides to 7 days again.
    exec(
        &test,
        "UPDATE sessions SET expires_at = now() + interval '7 days' - interval '2 hours' WHERE operator_id = $1",
        &op.id,
    )
    .await;
    let (s, _) = db.sessions().load(&cookie).await.unwrap().unwrap();
    assert!(s.expires_at > chrono::Utc::now() + week - chrono::Duration::minutes(1));

    // Garbage and unknown cookies are None, not errors.
    assert!(db.sessions().load("not base64!").await.unwrap().is_none());
    let other = db.sessions().create(&op.id).await.unwrap();
    db.sessions().destroy(&other).await.unwrap();
    assert!(db.sessions().load(&other).await.unwrap().is_none());

    // Expired: gone, and purged.
    exec(
        &test,
        "UPDATE sessions SET expires_at = now() - interval '1 second' WHERE operator_id = $1",
        &op.id,
    )
    .await;
    assert!(db.sessions().load(&cookie).await.unwrap().is_none());
    assert_eq!(db.sessions().purge_expired().await.unwrap(), 1);
}

#[tokio::test]
async fn password_change_destroys_sessions() {
    let (_test, db) = test_db().await;
    let a = db
        .operators()
        .create("a@x.io", "h1", "owner")
        .await
        .unwrap();
    let b = db
        .operators()
        .create("b@x.io", "h1", "viewer")
        .await
        .unwrap();
    let a1 = db.sessions().create(&a.id).await.unwrap();
    let a2 = db.sessions().create(&a.id).await.unwrap();
    let b1 = db.sessions().create(&b.id).await.unwrap();

    db.operators().set_password_hash(&a.id, "h2").await.unwrap();

    assert!(db.sessions().load(&a1).await.unwrap().is_none());
    assert!(db.sessions().load(&a2).await.unwrap().is_none());
    assert!(db.sessions().load(&b1).await.unwrap().is_some());
    let a = db.operators().by_id(&a.id).await.unwrap().unwrap();
    assert_eq!(a.password_hash, "h2");
}

// ---------------------------------------------------------------- servers

#[tokio::test]
async fn join_retry_same_nonce_returns_same_token() {
    let (_test, db) = test_db().await;
    let (pending, join_token) = db.servers().create_pending("web1", false).await.unwrap();
    assert_eq!(
        pending.status(chrono::Utc::now()),
        setu_db::ServerStatus::Pending
    );
    let (server, t1) = db
        .servers()
        .join(&join_token, &join_input("nonce-1", 4096), &sealer())
        .await
        .expect("first join");
    assert!(t1.starts_with("setu_srv_"));
    assert_eq!(
        server.status(chrono::Utc::now()),
        setu_db::ServerStatus::Online
    );
    assert!(server
        .join_token_sealed
        .as_deref()
        .is_some_and(|s| !s.contains(&t1)));

    let (again, t2) = db
        .servers()
        .join(&join_token, &join_input("nonce-1", 4096), &sealer())
        .await
        .expect("retry with the same nonce");
    assert_eq!(t1, t2);
    assert_eq!(again.id, server.id);
    let authed = db.servers().by_token(&t1).await.unwrap().expect("by token");
    assert_eq!(authed.id, server.id);
}

#[tokio::test]
async fn join_retry_different_nonce_is_refused() {
    let (_test, db) = test_db().await;
    let (_, join_token) = db.servers().create_pending("web1", false).await.unwrap();
    db.servers()
        .join(&join_token, &join_input("nonce-1", 4096), &sealer())
        .await
        .expect("first join");
    let err = db
        .servers()
        .join(&join_token, &join_input("nonce-2", 4096), &sealer())
        .await
        .expect_err("different nonce");
    assert!(matches!(err, JoinError::NonceMismatch), "{err:?}");
    assert!(matches!(
        db.servers()
            .join("setu_join_bogus", &join_input("nonce-1", 1), &sealer())
            .await,
        Err(JoinError::Unknown)
    ));
}

#[tokio::test]
async fn join_token_expires() {
    let (test, db) = test_db().await;
    // Never used, past its hour.
    let (pending, join_token) = db.servers().create_pending("web1", false).await.unwrap();
    exec(
        &test,
        "UPDATE servers SET join_expires_at = now() - interval '1 second' WHERE id = $1",
        &pending.id,
    )
    .await;
    let err = db
        .servers()
        .join(&join_token, &join_input("n", 4096), &sealer())
        .await
        .expect_err("expired");
    assert!(matches!(err, JoinError::Expired), "{err:?}");

    // Used, and the 60 s retry window has passed: refused, sealed token nulled.
    let (pending, join_token) = db.servers().create_pending("web2", false).await.unwrap();
    db.servers()
        .join(&join_token, &join_input("n", 4096), &sealer())
        .await
        .unwrap();
    exec(
        &test,
        "UPDATE servers SET joined_at = now() - interval '61 seconds' WHERE id = $1",
        &pending.id,
    )
    .await;
    let err = db
        .servers()
        .join(&join_token, &join_input("n", 4096), &sealer())
        .await
        .expect_err("window passed");
    assert!(matches!(err, JoinError::Expired), "{err:?}");
    let row = db.servers().get(&pending.id).await.unwrap();
    assert!(row.join_token_sealed.is_none());
}

#[tokio::test]
async fn heartbeat_reports_boot_change_and_keeps_own_metrics() {
    let (_test, db) = test_db().await;
    let site = site(&db).await;
    let metric = |short_id: &str| setu_proto::SiteMetric {
        short_id: short_id.to_string(),
        at: chrono::Utc::now(),
        cpu_seconds: 1.5,
        memory_bytes: 10,
        disk_bytes: 20,
    };
    let hb = |boot: &'static str, m: &'static [setu_proto::SiteMetric]| setu_db::HeartbeatInput {
        agent_version: "0.1.0",
        boot_id: Some(boot),
        load1: 0.25,
        mem_free_mb: 512,
        disk_free_mb: 4096,
        metrics: m,
    };
    let metrics: &'static [setu_proto::SiteMetric] =
        Box::leak(vec![metric(&site.short_id), metric("zzzz9999")].into_boxed_slice());
    let first = db
        .servers()
        .heartbeat(&site.server_id, &hb("b1", metrics))
        .await
        .unwrap();
    assert!(first.boot_changed, "no boot id stored yet");
    let stored = db.servers().get(&site.server_id).await.unwrap();
    assert_eq!(
        (stored.load1, stored.mem_free_mb, stored.disk_free_mb),
        (Some(0.25), Some(512), Some(4096)),
        "heartbeat stores the load figures"
    );
    let same = db
        .servers()
        .heartbeat(&site.server_id, &hb("b1", &[]))
        .await
        .unwrap();
    assert!(!same.boot_changed);
    let changed = db
        .servers()
        .heartbeat(&site.server_id, &hb("b2", &[]))
        .await
        .unwrap();
    assert!(changed.boot_changed);
    let latest = db
        .metrics()
        .latest_for_site(&site.id)
        .await
        .unwrap()
        .expect("metric");
    assert_eq!(latest.memory_bytes, 10);
    assert!(db
        .metrics()
        .latest_for_site("foreign")
        .await
        .unwrap()
        .is_none());
}

// ---------------------------------------------------------------- sites

#[tokio::test]
async fn create_site_allocates_distinct_ports_under_concurrency() {
    let (_test, db) = test_db().await;
    let server = online_server(&db, 1_000_000, false).await;
    let plan = catalogue(&db, 64).await;
    let mut tasks = Vec::new();
    for i in 0..20 {
        let db = db.clone();
        let plan = plan.clone();
        // Half name the server, half let `create` pick it.
        let server_id = (i % 2 == 0).then(|| server.id.clone());
        tasks.push(tokio::spawn(async move {
            db.sites()
                .create(&new_site(
                    server_id.as_deref(),
                    &plan,
                    &format!("s{i}.example.com"),
                    64,
                ))
                .await
        }));
    }
    let mut ports = HashSet::new();
    let mut shorts = HashSet::new();
    for t in tasks {
        let site = t.await.unwrap().expect("create under concurrency");
        assert_eq!(site.server_id, server.id);
        assert_eq!(site.db_name, format!("site_{}", site.short_id));
        assert_eq!(site.db_role, site.db_name);
        assert!(setu_common::is_short_id(&site.short_id));
        ports.insert(site.port);
        shorts.insert(site.short_id);
    }
    assert_eq!(ports.len(), 20);
    assert_eq!(shorts.len(), 20);
    assert_eq!(
        ports,
        (20000..20020).collect::<HashSet<i32>>(),
        "lowest free ports"
    );
}

#[tokio::test]
async fn create_site_refuses_over_85_percent() {
    let (_test, db) = test_db().await;
    // 85 % of 2000 = 1700. Each site costs 512 + 256.
    let server = online_server(&db, 2000, false).await;
    let plan = catalogue(&db, 512).await;
    let s = |d: &str| new_site(Some(&server.id), &plan, d, 512);
    db.sites()
        .create(&s("a.example.com"))
        .await
        .expect("768 ≤ 1700");
    db.sites()
        .create(&s("b.example.com"))
        .await
        .expect("1280 ≤ 1700");
    let err = db
        .sites()
        .create(&s("c.example.com"))
        .await
        .expect_err("1792 > 1700");
    assert!(matches!(err, CreateError::NoCapacity), "{err:?}");
    // Auto-pick sees the same rule.
    let err = db
        .sites()
        .create(&new_site(None, &plan, "d.example.com", 512))
        .await
        .expect_err("no server fits");
    assert!(matches!(err, CreateError::NoCapacity), "{err:?}");
}

#[tokio::test]
#[allow(clippy::similar_names)] // reviewed: kept as-is (R150)
async fn create_site_counts_control_plane_overhead() {
    let (_test, db) = test_db().await;
    let cp = online_server(&db, 2000, true).await;
    let plain = online_server(&db, 2000, false).await;
    let plan = catalogue(&db, 512).await;
    for (server, domain) in [(&cp, "a.example.com"), (&plain, "b.example.com")] {
        db.sites()
            .create(&new_site(Some(&server.id), &plan, domain, 512))
            .await
            .expect("first site fits both");
    }
    // Second site: 512 + 512 + 256 = 1280 ≤ 1700 on the plain server,
    // + 512 control-plane overhead = 1792 > 1700 on the control plane's.
    let err = db
        .sites()
        .create(&new_site(Some(&cp.id), &plan, "c.example.com", 512))
        .await
        .expect_err("control plane full");
    assert!(matches!(err, CreateError::NoCapacity), "{err:?}");
    db.sites()
        .create(&new_site(Some(&plain.id), &plan, "d.example.com", 512))
        .await
        .expect("plain server has room");
    // Auto-pick skips the control plane's server.
    let picked = db
        .sites()
        .create(&new_site(None, &plan, "e.example.com", 64))
        .await
        .expect("auto");
    assert_eq!(picked.server_id, plain.id);
}

#[tokio::test]
async fn create_site_rejects_duplicate_domain() {
    let (test, db) = test_db().await;
    let site = site(&db).await;
    let err = db
        .sites()
        .create(&new_site(
            Some(&site.server_id),
            &site.plan_id,
            "a.example.com",
            512,
        ))
        .await
        .expect_err("duplicate");
    assert!(matches!(err, CreateError::DomainTaken), "{err:?}");
    assert!(db
        .sites()
        .by_domain_excluding("a.example.com", &site.id)
        .await
        .unwrap()
        .is_none());

    // Cordoned and offline servers refuse too.
    db.servers()
        .set_cordoned(&site.server_id, true)
        .await
        .unwrap();
    let err = db
        .sites()
        .create(&new_site(
            Some(&site.server_id),
            &site.plan_id,
            "b.example.com",
            512,
        ))
        .await
        .expect_err("cordoned");
    assert!(matches!(err, CreateError::Cordoned), "{err:?}");
    db.servers()
        .set_cordoned(&site.server_id, false)
        .await
        .unwrap();
    exec(
        &test,
        "UPDATE servers SET last_seen_at = now() - interval '5 minutes' WHERE id = $1",
        &site.server_id,
    )
    .await;
    let err = db
        .sites()
        .create(&new_site(
            Some(&site.server_id),
            &site.plan_id,
            "b.example.com",
            512,
        ))
        .await
        .expect_err("offline");
    assert!(matches!(err, CreateError::ServerOffline), "{err:?}");
    let err = db
        .sites()
        .create(&new_site(None, &site.plan_id, "b.example.com", 512))
        .await
        .expect_err("nothing online");
    assert!(matches!(err, CreateError::NoServer), "{err:?}");
    let err = db
        .servers()
        .delete(&site.server_id)
        .await
        .expect_err("has sites");
    assert_eq!(err.code, ErrorCode::Conflict);
}

// ---------------------------------------------------------------- jobs

#[tokio::test]
async fn claim_skips_locked_and_reclaims_expired_lease() {
    let (test, db) = test_db().await;
    let lease = Duration::from_secs(60);
    let j1 = db
        .jobs()
        .enqueue("backup_site", None, None, None)
        .await
        .unwrap();
    let j2 = db
        .jobs()
        .enqueue("backup_site", None, None, None)
        .await
        .unwrap();

    // Another transaction holds j1's row lock: claim skips it.
    let mut tx = test.pool.begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE id = $1 FOR UPDATE")
        .bind(&j1.id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let got = db.jobs().claim("w1", lease).await.unwrap().expect("j2");
    assert_eq!(got.id, j2.id);
    tx.rollback().await.unwrap();

    let got = db.jobs().claim("w1", lease).await.unwrap().expect("j1");
    assert_eq!(got.id, j1.id);
    assert_eq!(got.locked_by.as_deref(), Some("w1"));
    assert!(
        db.jobs().claim("w2", lease).await.unwrap().is_none(),
        "leases held"
    );

    // w1 died: its lease runs out and w2 reclaims the job.
    exec(
        &test,
        "UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1",
        &j1.id,
    )
    .await;
    let got = db
        .jobs()
        .claim("w2", lease)
        .await
        .unwrap()
        .expect("reclaimed");
    assert_eq!(got.id, j1.id);
    assert_eq!(got.attempts, 2);
    assert!(!db.jobs().renew(&j1.id, "w1", lease).await.unwrap());
    assert!(db.jobs().renew(&j1.id, "w2", lease).await.unwrap());

    db.jobs().set_step(&j1.id, "w2", "install").await.unwrap();
    assert_eq!(
        db.jobs().get_step(&j1.id).await.unwrap().as_deref(),
        Some("install")
    );
    let done = db.jobs().finish(&j1.id, "w2", true, None).await.unwrap();
    assert_eq!(done.status, "done");
    assert!(done.locked_by.is_none());
}

/// Only the lease holder may write a job's log, step cursor, payload or
/// final status: a runner that lost its lease (another runner reclaimed
/// the job) must not overwrite the new owner's work (M2).
#[tokio::test]
async fn finish_requires_lease_holder() {
    let (test, db) = test_db().await;
    let lease = Duration::from_secs(60);
    let job = db
        .jobs()
        .enqueue("delete_backup", None, None, None)
        .await
        .unwrap();
    db.jobs()
        .claim("w1", lease)
        .await
        .unwrap()
        .expect("claimed");
    exec(
        &test,
        "UPDATE jobs SET locked_until = now() - interval '1 second' WHERE id = $1",
        &job.id,
    )
    .await;
    db.jobs()
        .claim("w2", lease)
        .await
        .unwrap()
        .expect("reclaimed");

    // w1 lost the lease: every write is refused and changes nothing.
    assert!(!db
        .jobs()
        .append_log(&job.id, "w1", "stale\n")
        .await
        .unwrap());
    let err = db.jobs().set_step(&job.id, "w1", "late").await.unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = db
        .jobs()
        .set_payload(&job.id, "w1", &serde_json::json!({"x": 1}))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let err = db
        .jobs()
        .finish(&job.id, "w1", false, Some("stale"))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::Conflict);
    let row = db.jobs().get(&job.id).await.unwrap();
    assert_eq!(row.status, "running");
    assert_eq!(row.locked_by.as_deref(), Some("w2"));
    assert_eq!(row.log, "");
    assert!(row.payload.get("step").is_none());

    // w2 holds it: its writes land.
    assert!(db.jobs().append_log(&job.id, "w2", "mine\n").await.unwrap());
    db.jobs().set_step(&job.id, "w2", "go").await.unwrap();
    let done = db.jobs().finish(&job.id, "w2", true, None).await.unwrap();
    assert_eq!(done.status, "done");
    assert_eq!(done.log, "mine\n");
    // A finished job takes no further writes, even from its last holder.
    assert!(!db
        .jobs()
        .append_log(&job.id, "w2", "after\n")
        .await
        .unwrap());
    // A missing job is still not_found.
    let err = db
        .jobs()
        .finish("nope", "w2", true, None)
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn enqueue_second_active_job_for_site_is_conflict() {
    let (_test, db) = test_db().await;
    let site = site(&db).await;
    let first = db
        .jobs()
        .enqueue(
            "provision_site",
            Some(&site.id),
            Some(&site.server_id),
            None,
        )
        .await
        .unwrap();
    let err = db
        .jobs()
        .enqueue("backup_site", Some(&site.id), Some(&site.server_id), None)
        .await
        .expect_err("second active job");
    assert_eq!(err.code, ErrorCode::Conflict);
    assert_eq!(
        db.jobs()
            .active_for_site(&site.id)
            .await
            .unwrap()
            .map(|j| j.id),
        Some(first.id.clone())
    );
    db.jobs()
        .claim("w1", Duration::from_secs(60))
        .await
        .unwrap()
        .expect("claimed");
    db.jobs()
        .finish(&first.id, "w1", false, Some("boom"))
        .await
        .unwrap();
    db.jobs()
        .enqueue("backup_site", Some(&site.id), Some(&site.server_id), None)
        .await
        .expect("allowed once the first is finished");
}

// ---------------------------------------------------------------- commands

#[tokio::test]
async fn complete_nulls_secret_fields() {
    let (_test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    for cmd in secret_commands() {
        let env = db
            .commands()
            .enqueue(&server.id, None, &cmd, TTL)
            .await
            .unwrap();
        let delivered = db
            .commands()
            .next_for(&server.id)
            .await
            .unwrap()
            .expect("delivered");
        assert_eq!(delivered.command, cmd, "the agent receives the secret");
        db.commands()
            .complete(&env.id, &result(&env.id, true))
            .await
            .unwrap();
        assert_eq!(db.commands().get(&env.id).await.unwrap().status, "done");
        assert_redacted(&db, &env.id).await;
    }
}

#[tokio::test]
async fn cancel_nulls_secret_fields() {
    let (_test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    for cmd in secret_commands() {
        let env = db
            .commands()
            .enqueue(&server.id, None, &cmd, TTL)
            .await
            .unwrap();
        assert!(db.commands().cancel(&env.id).await.unwrap());
        assert_eq!(
            db.commands().get(&env.id).await.unwrap().status,
            "cancelled"
        );
        assert_redacted(&db, &env.id).await;
        // A late result never overwrites a cancelled row.
        db.commands()
            .complete(&env.id, &result(&env.id, true))
            .await
            .unwrap();
        assert_eq!(
            db.commands().get(&env.id).await.unwrap().status,
            "cancelled"
        );
    }
    assert!(db.commands().next_for(&server.id).await.unwrap().is_none());
}

#[tokio::test]
async fn expired_command_nulls_secret_fields() {
    let (_test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    for cmd in secret_commands() {
        let env = db
            .commands()
            .enqueue(&server.id, None, &cmd, Duration::ZERO)
            .await
            .unwrap();
        assert!(db.commands().next_for(&server.id).await.unwrap().is_none());
        assert_eq!(db.commands().get(&env.id).await.unwrap().status, "expired");
        assert_redacted(&db, &env.id).await;
    }
    // The prune safety net redacts a stale non-terminal row too.
    let [cmd, _] = secret_commands();
    let env = db
        .commands()
        .enqueue(&server.id, None, &cmd, TTL)
        .await
        .unwrap();
    sqlx::query("UPDATE agent_commands SET created_at = now() - interval '25 hours' WHERE id = $1")
        .bind(&env.id)
        .execute(db.pool())
        .await
        .unwrap();
    db.commands().prune(Duration::from_hours(48)).await.unwrap();
    assert_eq!(db.commands().get(&env.id).await.unwrap().status, "expired");
    assert_redacted(&db, &env.id).await;
}

#[tokio::test]
async fn next_for_marks_expired() {
    let (test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    let start = |id: &str| Command::Start {
        site: id.to_string(),
    };
    let old = db
        .commands()
        .enqueue(&server.id, Some("abcd1234"), &start("a"), TTL)
        .await
        .unwrap();
    let live = db
        .commands()
        .enqueue(&server.id, None, &start("b"), TTL)
        .await
        .unwrap();
    exec(
        &test,
        "UPDATE agent_commands SET expires_at = now() - interval '1 second' WHERE id = $1",
        &old.id,
    )
    .await;
    let got = db
        .commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .expect("live one");
    assert_eq!(got.id, live.id);
    assert_eq!(got.site_short_id, None);
    assert_eq!(db.commands().get(&live.id).await.unwrap().attempts, 1);
    assert_eq!(db.commands().get(&old.id).await.unwrap().status, "expired");
    assert_eq!(
        db.commands().get(&live.id).await.unwrap().status,
        "delivered"
    );
    assert!(matches!(
        db.commands().wait(&old.id, Duration::from_secs(1)).await,
        Err(WaitError::Expired)
    ));
}

#[tokio::test]
async fn wait_returns_cancelled() {
    let (_test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    let cmd = Command::Inventory;
    let env = db
        .commands()
        .enqueue(&server.id, None, &cmd, TTL)
        .await
        .unwrap();
    let waiter = {
        let db = db.clone();
        let id = env.id.clone();
        tokio::spawn(async move { db.commands().wait(&id, Duration::from_secs(10)).await })
    };
    tokio::time::sleep(Duration::from_millis(400)).await;
    db.commands().cancel(&env.id).await.unwrap();
    let res = tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .expect("wait returns promptly")
        .unwrap();
    assert!(matches!(res, Err(WaitError::Cancelled)), "{res:?}");

    // And a short wait on a pending command times out; a completed one
    // returns its result.
    let env = db
        .commands()
        .enqueue(&server.id, None, &cmd, TTL)
        .await
        .unwrap();
    assert!(matches!(
        db.commands()
            .wait(&env.id, Duration::from_millis(350))
            .await,
        Err(WaitError::Timeout)
    ));
    db.commands()
        .complete(&env.id, &result(&env.id, false))
        .await
        .unwrap();
    let r = db
        .commands()
        .wait(&env.id, Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!r.ok);
}

#[tokio::test]
async fn requeue_delivered_requeues_only_delivered() {
    let (_test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    let other = online_server(&db, 4096, false).await;
    let cmd = Command::Inventory;
    let done = db
        .commands()
        .enqueue(&server.id, None, &cmd, TTL)
        .await
        .unwrap();
    db.commands().next_for(&server.id).await.unwrap();
    db.commands()
        .complete(&done.id, &result(&done.id, true))
        .await
        .unwrap();
    let delivered = db
        .commands()
        .enqueue(&server.id, None, &cmd, TTL)
        .await
        .unwrap();
    db.commands().next_for(&server.id).await.unwrap();
    let queued = db
        .commands()
        .enqueue(&server.id, None, &cmd, TTL)
        .await
        .unwrap();
    let elsewhere = db
        .commands()
        .enqueue(&other.id, None, &cmd, TTL)
        .await
        .unwrap();
    db.commands().next_for(&other.id).await.unwrap();

    assert_eq!(
        db.commands().requeue_delivered(&server.id).await.unwrap(),
        1
    );
    let status = |id: String| {
        let db = db.clone();
        async move { db.commands().get(&id).await.unwrap().status }
    };
    assert_eq!(status(done.id).await, "done");
    assert_eq!(status(delivered.id.clone()).await, "queued");
    assert_eq!(status(queued.id).await, "queued");
    assert_eq!(status(elsewhere.id).await, "delivered");
    let again = db
        .commands()
        .next_for(&server.id)
        .await
        .unwrap()
        .expect("redelivered");
    assert_eq!(again.id, delivered.id);
    assert_eq!(db.commands().get(&again.id).await.unwrap().attempts, 2);
}

#[tokio::test]
async fn prune_deletes_terminal_by_created_at() {
    let (test, db) = test_db().await;
    let server = online_server(&db, 4096, false).await;
    let cmd = Command::Inventory;
    let mut ids = Vec::new();
    for _ in 0..3 {
        let env = db
            .commands()
            .enqueue(&server.id, None, &cmd, TTL)
            .await
            .unwrap();
        ids.push(env.id);
    }
    // [0]: created 2 days ago, completed just now -> pruned (by created_at).
    // [1]: created now, done -> kept.  [2]: created 2 days ago but
    // delivered and live -> becomes expired (safety net) and is pruned.
    for id in &ids[..2] {
        db.commands().complete(id, &result(id, true)).await.unwrap();
    }
    for id in [&ids[0], &ids[2]] {
        exec(
            &test,
            "UPDATE agent_commands SET created_at = now() - interval '2 days' WHERE id = $1",
            id,
        )
        .await;
    }
    let n = db.commands().prune(Duration::from_hours(24)).await.unwrap();
    assert_eq!(n, 2);
    assert!(db.commands().get(&ids[0]).await.is_err());
    assert_eq!(db.commands().get(&ids[1]).await.unwrap().status, "done");
    assert!(db.commands().get(&ids[2]).await.is_err());
}

// ---------------------------------------------------------------- misc

#[tokio::test]
async fn settings_and_backups_round_trip() {
    let (_test, db) = test_db().await;
    assert!(db.settings().get("k").await.unwrap().is_none());
    db.settings()
        .set("k", &serde_json::json!({"a": 1}))
        .await
        .unwrap();
    db.settings()
        .set("k", &serde_json::json!({"a": 2}))
        .await
        .unwrap();
    assert_eq!(
        db.settings().get("k").await.unwrap(),
        Some(serde_json::json!({"a": 2}))
    );
    db.settings().delete("k").await.unwrap();
    assert!(db.settings().get("k").await.unwrap().is_none());
    db.settings().delete("k").await.unwrap();

    let site = site(&db).await;
    let mut scheduled = Vec::new();
    for _ in 0..3 {
        scheduled.push(db.backups().create(&site, "scheduled").await.unwrap());
    }
    let pre = db.backups().create(&site, "pre_update").await.unwrap();
    assert_eq!(pre.site_short_id, site.short_id);
    let beyond = db.backups().scheduled_beyond(&site.id, 2).await.unwrap();
    assert_eq!(beyond.len(), 1);
    sqlx::query("UPDATE backups SET taken_at = now() - interval '31 days' WHERE id = $1")
        .bind(&pre.id)
        .execute(db.pool())
        .await
        .unwrap();
    let old = db.backups().pre_older_than(30).await.unwrap();
    assert_eq!(old.iter().map(|b| &b.id).collect::<Vec<_>>(), vec![&pre.id]);
    db.backups().mark_deleted(&pre.id).await.unwrap();
    assert!(db.backups().pre_older_than(30).await.unwrap().is_empty());
    assert_eq!(db.backups().list_for_site(&site.id).await.unwrap().len(), 3);

    // Backups outlive their site.
    db.sites().delete(&site.id).await.unwrap();
    let b = db.backups().get(&scheduled[0].id).await.unwrap();
    assert!(b.site_id.is_none());
    assert_eq!(b.domain, "a.example.com");
}

#[tokio::test]
async fn delete_operator_removes_sessions_and_unknown_is_not_found() {
    let (_test, db) = test_db().await;
    let a = db
        .operators()
        .create("a@x.io", "h1", "owner")
        .await
        .unwrap();
    let b = db
        .operators()
        .create("b@x.io", "h1", "viewer")
        .await
        .unwrap();
    let b1 = db.sessions().create(&b.id).await.unwrap();

    let gone = db.operators().delete(&b.id).await.expect("delete viewer");
    assert_eq!(gone.email, "b@x.io");
    assert!(
        db.sessions().load(&b1).await.unwrap().is_none(),
        "session ended"
    );
    assert!(db.operators().by_id(&b.id).await.unwrap().is_none());
    let err = db
        .operators()
        .delete(&b.id)
        .await
        .expect_err("already gone");
    assert_eq!(err.code, ErrorCode::NotFound);
    let err = db.operators().delete(&a.id).await.expect_err("last owner");
    assert_eq!(err.code, ErrorCode::Conflict);
}

/// Review Focus 1: two owners delete each other at once; exactly one wins.
#[tokio::test]
async fn delete_operator_never_removes_the_last_owner_under_race() {
    for round in 0..5 {
        let (_test, db) = test_db().await;
        let a = db.operators().create("a@x.io", "h", "owner").await.unwrap();
        let b = db.operators().create("b@x.io", "h", "owner").await.unwrap();
        let ops = db.operators();
        let (ra, rb) = tokio::join!(ops.delete(&b.id), ops.delete(&a.id));
        assert_eq!(
            usize::from(ra.is_ok()) + usize::from(rb.is_ok()),
            1,
            "round {round}: exactly one delete succeeds"
        );
        let err = ra.err().or(rb.err()).expect("one refusal");
        assert_eq!(err.code, ErrorCode::Conflict, "round {round}");
        let owners = db.operators().list().await.unwrap();
        assert_eq!(owners.iter().filter(|o| o.role == "owner").count(), 1);
    }
}

#[tokio::test]
async fn latest_for_sites_returns_the_newest_sample_per_site() {
    let (_test, db) = test_db().await;
    let s = site(&db).await;
    let now = chrono::Utc::now();
    let sample = |at, mem: u64| setu_proto::SiteMetric {
        short_id: s.short_id.clone(),
        at,
        cpu_seconds: 1.0,
        memory_bytes: mem,
        disk_bytes: 7 << 20,
    };
    db.metrics()
        .insert_batch(
            &s.server_id,
            &[
                sample(now - chrono::Duration::minutes(2), 1 << 20),
                sample(now, 3 << 20),
            ],
        )
        .await
        .unwrap();

    let map = db
        .metrics()
        .latest_for_sites(&[s.id.clone(), "no-such-site".to_string()])
        .await
        .unwrap();
    assert_eq!(map.len(), 1, "sites without samples are absent");
    assert_eq!(map[&s.id].memory_bytes, 3 << 20, "the newest sample");
    assert!(db.metrics().latest_for_sites(&[]).await.unwrap().is_empty());
}
