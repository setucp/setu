//! Shared fixtures for the jobs integration tests: throwaway DB,
//! online server, catalogue version, plan, and a `provisioning` site.
//!
//! Each integration target compiles this module separately, so not
//! every helper is used everywhere.
#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use serde_json::json;
use setu_common::{Sealer, SecretKey};
use setu_db::{Db, NewSite};
use setu_jobs::{testing::FakeAgent, JobCtx};
use setu_manifest::Manifest;

pub const SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

/// Minimal binary manifest with a `once` secret, `replace` strategy.
pub fn manifest_toml(version: &str) -> String {
    format!(
        r#"
schema = 1

[app]
id = "hello"
name = "Hello"
version = "{version}"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"

[run]
kind = "binary"
args = ["serve"]

[run.binary]
"x86_64-linux" = {{ url = "https://example.com/hello-{version}.tar.gz", sha256 = "{SHA256}", exec = "hello" }}

[needs]
dirs = ["media"]

[env]
HELLO_SECRET = {{ generate = "hex32", secret = true }}
HELLO_PASSWORD = {{ generate = "password20", secret = true, once = true }}
HELLO_DATABASE_URL = {{ from = "postgres_url" }}
HELLO_MEDIA_DIR = {{ from = "dir", dir = "media" }}
HELLO_PORT = {{ from = "port" }}

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = true
dirs = ["media"]

[update]
strategy = "replace"
"#
    )
}

/// Same app as a container image (phase 2): parses, but provisioning
/// and update must reject it before issuing anything.
pub fn manifest_toml_image(version: &str) -> String {
    format!(
        r#"
schema = 1

[app]
id = "hello"
name = "Hello"
version = "{version}"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"

[run]
kind = "image"

[run.image]
image = "ghcr.io/example/hello:{version}"

[needs]
dirs = ["media"]

[env]
HELLO_SECRET = {{ generate = "hex32", secret = true }}

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = true
dirs = ["media"]

[update]
strategy = "replace"
"#
    )
}

pub struct Fx {
    pub db: Db,
    pub server_id: String,
    pub site_id: String,
    pub domain: String,
}

/// Throwaway DB with an online server, one catalogue version, the
/// default plan, and a `provisioning` site for that version.
pub async fn setup(manifest_src: &str) -> (setu_testkit::TestDb, Fx) {
    let test_db = setu_testkit::TestDb::new().await;
    let db = Db::new(test_db.pool.clone());

    let (pending, join_token) = db
        .servers()
        .create_pending("test-server", false)
        .await
        .unwrap();
    let join = setu_db::JoinInput {
        nonce: "test-nonce".to_string(),
        hostname: "test-host".to_string(),
        public_ip: None,
        arch: "x86_64".to_string(),
        os: "linux".to_string(),
        cpu_cores: 4,
        memory_mb: 8192,
        disk_mb: 102400,
    };
    let key = setu_common::SecretKey::from_hex(&"cd".repeat(32)).unwrap();
    let sealer = setu_common::Sealer::new(&key);
    let (server, _server_token) = db
        .servers()
        .join(&join_token, &join, &sealer)
        .await
        .unwrap();

    let manifest = Manifest::parse(manifest_src)
        .unwrap_or_else(|errors| panic!("test manifest must parse: {errors:?}"));
    let manifest_json = serde_json::to_value(&manifest).unwrap();
    let app_version = db.apps().upsert_version(&manifest_json).await.unwrap();

    let plan = db.plans().seed_default().await.unwrap();
    let site = create_site(
        &db,
        &NewSite {
            name: "hello-site".to_string(),
            app_id: app_version.app_id.clone(),
            app_version: app_version.version.clone(),
            server_id: Some(server.id.clone()),
            plan_id: plan.id.clone(),
            domain: "hello.example.com".to_string(),
            owner_email: "owner@example.com".to_string(),
            env_sealed: json!([]),
            plan_memory_mb: i64::from(plan.memory_mb),
        },
    )
    .await;

    assert_eq!(
        pending.status(chrono::Utc::now()),
        setu_db::ServerStatus::Pending
    );
    (
        test_db,
        Fx {
            db,
            server_id: server.id,
            site_id: site.id,
            domain: site.domain,
        },
    )
}

/// Publish another version of the `hello` catalogue app.
pub async fn publish_version(db: &Db, manifest_src: &str) {
    let manifest = Manifest::parse(manifest_src)
        .unwrap_or_else(|errors| panic!("test manifest must parse: {errors:?}"));
    let manifest_json = serde_json::to_value(&manifest).unwrap();
    db.apps().upsert_version(&manifest_json).await.unwrap();
}

pub fn test_sealer() -> Sealer {
    Sealer::new(&SecretKey::from_hex(&"ab".repeat(32)).unwrap())
}

/// A [`JobCtx`] wiring the [`FakeAgent`] plus captured log lines.
pub fn ctx_with_logs(db: Db, agent: &FakeAgent) -> (JobCtx, Arc<Mutex<Vec<String>>>) {
    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let capture = lines.clone();
    let ctx = JobCtx::new(
        db,
        Arc::new(agent.clone()),
        test_sealer(),
        move |line: &str| {
            capture.lock().expect("log lock").push(line.to_string());
        },
    )
    .with_verify_delay(std::time::Duration::ZERO);
    (ctx, lines)
}

/// Another `provisioning` site of the fixture's app and plan on the
/// same server.
pub async fn add_site(db: &Db, like: &str, domain: &str) -> String {
    let base = db.sites().get(like).await.unwrap();
    let plan = db.plans().get(&base.plan_id).await.unwrap();
    create_site(
        db,
        &NewSite {
            name: domain.to_string(),
            app_id: base.app_id.clone(),
            app_version: base.app_version.clone(),
            server_id: Some(base.server_id.clone()),
            plan_id: base.plan_id.clone(),
            domain: domain.to_string(),
            owner_email: "owner@example.com".to_string(),
            env_sealed: json!([]),
            plan_memory_mb: i64::from(plan.memory_mb),
        },
    )
    .await
    .id
}

/// `sites.create_with` generating the env once from the allocation,
/// exactly as `POST /sites` does.
pub async fn create_site(db: &Db, new: &NewSite) -> setu_db::Site {
    let version = db
        .apps()
        .version(&new.app_id, &new.app_version)
        .await
        .unwrap();
    let manifest: Manifest = serde_json::from_value(version.manifest).unwrap();
    let sealer = test_sealer();
    db.sites()
        .create_with(new, |alloc| {
            setu_jobs::resolve_site_env(
                &manifest,
                alloc,
                &new.domain,
                &new.owner_email,
                &Default::default(),
                &sealer,
            )
        })
        .await
        .unwrap()
}

/// A job row of `kind` for the fixture site (so the run has a cursor,
/// a payload and `jobs.log`) and a context bound to it.
pub async fn job_ctx(
    db: &Db,
    agent: &FakeAgent,
    kind: &str,
    site_id: &str,
    payload: serde_json::Value,
) -> (JobCtx, String, Arc<Mutex<Vec<String>>>) {
    let site = db.sites().get(site_id).await.unwrap();
    let job = db
        .jobs()
        .enqueue(kind, Some(site_id), Some(&site.server_id), Some(&payload))
        .await
        .unwrap();
    let (ctx, logs) = ctx_with_logs(db.clone(), agent);
    (ctx.with_job(&job.id), job.id, logs)
}

/// Finish a job driven by hand, so the site may take its next job.
pub async fn finish_job(db: &Db, job_id: &str, ok: bool) {
    db.jobs().finish(job_id, ok, None).await.unwrap();
}

/// Plaintext of the stored env: name → value.
pub fn stored_env(site: &setu_db::Site) -> std::collections::BTreeMap<String, String> {
    let sealer = test_sealer();
    site.env
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["name"].as_str().unwrap().to_string(),
                sealer.open(v["value"].as_str().unwrap()).unwrap(),
            )
        })
        .collect()
}

/// The password inside a `postgres://role:pw@host/db` URL.
pub fn url_password(url: &str) -> String {
    url.split(':')
        .nth(2)
        .and_then(|rest| rest.split('@').next())
        .expect("password in url")
        .to_string()
}

/// Poll until the job leaves `queued`/`running` (or `within` passes).
pub async fn wait_job(db: &Db, job_id: &str, within: std::time::Duration) -> setu_db::Job {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        let job = db.jobs().get(job_id).await.unwrap();
        if job.status == "done" || job.status == "failed" {
            return job;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "job {job_id} still {} after {within:?}",
            job.status
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Run one SQL statement with a single text parameter.
pub async fn exec(test_db: &setu_testkit::TestDb, sql: &str, arg: &str) {
    sqlx::query(sqlx::AssertSqlSafe(sql.to_string()))
        .bind(arg)
        .execute(&test_db.pool)
        .await
        .unwrap();
}
