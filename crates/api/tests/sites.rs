//! Sites/jobs/backups/logs integration tests (plan Task 9, spec §5/§6).
//!
//! Every test boots [`TestServer`] with a scripted [`FakeAgent`]; the
//! server runs the real `Runner` (concurrency 4), so `POST /sites`
//! provisions to `running` on its own and tests poll `GET /jobs/{id}`
//! until done instead of driving jobs by hand.

use std::time::Duration;

use serde_json::{json, Value};
use setu_api::testing::TestServer;
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

const SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

fn hello_toml(version: &str) -> String {
    format!(
        r#"schema = 1

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
HELLO_PORT = {{ from = "port" }}
HELLO_LABEL = {{ value = "hello-world" }}
HELLO_NAME = {{ prompt = "Display name", secret = true }}

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

fn image_toml() -> String {
    r#"schema = 1

[app]
id = "imgapp"
name = "Imgapp"
version = "0.3.0"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"

[run]
kind = "image"

[run.image]
image = "ghcr.io/example/imgapp:0.3.0"

[needs]
dirs = []

[env]
IMGAPP_SECRET = { generate = "hex32", secret = true }

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = false

[update]
strategy = "replace"
"#
    .to_string()
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", setu_common::new_id()[..8].to_lowercase())
}

/// Publish a manifest through the API (also covers `app.publish` audit).
async fn publish(srv: &TestServer, owner: &reqwest::Client, toml: &str) {
    let resp = owner
        .post(srv.url("/api/v1/apps"))
        .json(&json!({"toml": toml}))
        .send()
        .await
        .expect("publish sends");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::CREATED,
        "publish {toml}"
    );
}

/// Register + join + heartbeat a server straight through the repos
/// (no HTTP agent dance); returns the server id, online from now.
async fn online_server(srv: &TestServer, name: &str, memory_mb: u64) -> String {
    let (_pending, join_token) = srv
        .db
        .servers()
        .create_pending(name, false)
        .await
        .expect("create pending");
    let input = setu_db::JoinInput {
        nonce: String::new(),
        hostname: name.to_string(),
        public_ip: None,
        arch: "x86_64".to_string(),
        os: "linux".to_string(),
        cpu_cores: 4,
        memory_mb: i64::try_from(memory_mb).expect("memory fits"),
        disk_mb: 102400,
    };
    let key = setu_common::SecretKey::from_hex(&"ab".repeat(32)).expect("key");
    let (server, _token) = srv
        .db
        .servers()
        .join(&join_token, &input, &setu_common::Sealer::new(&key))
        .await
        .expect("join");
    srv.db
        .servers()
        .heartbeat(
            &server.id,
            &setu_db::HeartbeatInput {
                agent_version: "0.1.0",
                boot_id: None,
                load1: 0.0,
                mem_free_mb: 0,
                disk_free_mb: 0,
                metrics: &[],
            },
        )
        .await
        .expect("heartbeat");
    server.id
}

async fn default_plan_id(srv: &TestServer) -> String {
    srv.db
        .plans()
        .seed_default()
        .await
        .expect("seed default plan")
        .id
}

/// `POST /sites`; asserts 202 and returns `(site_id, job_id)`.
async fn post_site(
    srv: &TestServer,
    owner: &reqwest::Client,
    server_id: Option<&str>,
    domain: &str,
) -> (String, String) {
    let plan_id = default_plan_id(srv).await;
    let mut body = json!({
        "name": unique("site"),
        "app_id": "hello",
        "version": "0.1.0",
        "plan_id": plan_id,
        "domain": domain,
        "owner_email": "owner@example.com",
        "prompts": {"HELLO_NAME": "test-display-name"},
    });
    if let Some(sid) = server_id {
        body["server_id"] = json!(sid);
    }
    let resp = owner
        .post(srv.url("/api/v1/sites"))
        .json(&body)
        .send()
        .await
        .expect("post site sends");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::ACCEPTED,
        "post site {domain}"
    );
    let out: Value = resp.json().await.expect("create body is json");
    (
        out["site"]["id"].as_str().expect("site id").to_string(),
        out["job"]["id"].as_str().expect("job id").to_string(),
    )
}

/// Poll `GET /jobs/{id}` until `done`/`failed` (or the row is gone,
/// which is how a finished delete reads); returns the last body, or
/// `None` when the row vanished.
async fn poll_job(srv: &TestServer, client: &reqwest::Client, job_id: &str) -> Option<Value> {
    for _ in 0..40 {
        let resp = client
            .get(srv.url(&format!("/api/v1/jobs/{job_id}")))
            .send()
            .await
            .expect("poll job sends");
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return None;
        }
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let body: Value = resp.json().await.expect("job body is json");
        match body["status"].as_str().unwrap_or("") {
            "done" | "failed" => return Some(body),
            _ => tokio::time::sleep(Duration::from_millis(500)).await,
        }
    }
    panic!("job {job_id} did not finish in 20 s");
}

async fn get_site(srv: &TestServer, client: &reqwest::Client, site_id: &str) -> Value {
    let resp = client
        .get(srv.url(&format!("/api/v1/sites/{site_id}")))
        .send()
        .await
        .expect("get site sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    resp.json().await.expect("site body is json")
}

#[tokio::test]
async fn create_site_returns_job_and_provisions_to_running() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    // No `server_id`: the control plane auto-picks the online server.
    let server_id = online_server(&srv, "auto-pick-1", 8192).await;

    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        None,
        &format!("{}.example.com", unique("web")),
    )
    .await;

    let job = poll_job(&srv, &owner, &job_id)
        .await
        .expect("provision job row survives");
    assert_eq!(job["status"], json!("done"));
    assert_eq!(job["kind"], json!("provision_site"));

    let site = get_site(&srv, &owner, &site_id).await;
    assert_eq!(site["status"], json!("running"));
    assert_eq!(site["server_id"], json!(server_id));
    assert!(site["db_name"].as_str().unwrap().starts_with("site_"));

    // Spec §6 order: `inventory` is `validate`'s port check, `status`
    // is `verify`, the second WriteEnv (`finish`) drops the `once` secret.
    assert_eq!(
        srv.agent.transcript(),
        vec![
            "inventory",
            "site_create",
            "create_database",
            "install",
            "switch_version",
            "write_unit",
            "write_env",
            "start",
            "health",
            "set_route",
            "status",
            "write_env",
        ]
    );
    // The env was generated at creation; the job carries no prompts.
    let row = srv.db.jobs().get(&job_id).await.expect("job row");
    assert!(
        row.payload.get("prompts").is_none(),
        "{:?}",
        row.payload
            .as_object()
            .map(|o| o.keys().collect::<Vec<_>>())
    );
    let writes = srv.agent.write_env_calls();
    assert_eq!(writes.len(), 2);
    assert!(writes[0].iter().any(|v| v.once));
    assert!(writes[1].iter().all(|v| !v.once));
}

#[tokio::test]
async fn create_site_rejects_image_manifest() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &image_toml()).await;
    let server_id = online_server(&srv, "img-1", 8192).await;
    let plan_id = default_plan_id(&srv).await;

    let resp = owner
        .post(srv.url("/api/v1/sites"))
        .json(&json!({
            "name": unique("site"),
            "app_id": "imgapp",
            "version": "0.3.0",
            "plan_id": plan_id,
            "server_id": server_id,
            "domain": format!("{}.example.com", unique("img")),
            "owner_email": "owner@example.com",
            "prompts": {},
        }))
        .send()
        .await
        .expect("post site sends");
    assert_eq!(resp.status(), reqwest::StatusCode::BAD_REQUEST);
    let body: Value = resp.json().await.expect("error body is json");
    assert_eq!(body["error"]["code"], json!("validation"));
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("image runtime not available"),
        "unexpected message: {body}"
    );
    assert!(srv.agent.transcript().is_empty(), "no agent call may fire");
}

#[tokio::test]
async fn create_site_rejects_duplicate_domain_409() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "dup-1", 8192).await;
    let domain = format!("{}.example.com", unique("dup"));

    post_site(&srv, &owner, Some(&server_id), &domain).await;

    let plan_id = default_plan_id(&srv).await;
    let resp = owner
        .post(srv.url("/api/v1/sites"))
        .json(&json!({
            "name": unique("site"),
            "app_id": "hello",
            "version": "0.1.0",
            "plan_id": plan_id,
            "server_id": server_id,
            "domain": domain,
            "owner_email": "other@example.com",
            "prompts": {"HELLO_NAME": "x"},
        }))
        .send()
        .await
        .expect("duplicate post sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("error body is json");
    assert_eq!(body["error"]["code"], json!("conflict"));
}

#[tokio::test]
async fn create_site_rejects_when_capacity_exceeded() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    // 256 MB server vs the 512 MB Default plan: 512 > 256 × 0.9.
    let tiny = online_server(&srv, "tiny-1", 256).await;
    let plan_id = default_plan_id(&srv).await;

    let resp = owner
        .post(srv.url("/api/v1/sites"))
        .json(&json!({
            "name": unique("site"),
            "app_id": "hello",
            "version": "0.1.0",
            "plan_id": plan_id,
            "server_id": tiny,
            "domain": format!("{}.example.com", unique("big")),
            "owner_email": "owner@example.com",
            "prompts": {"HELLO_NAME": "x"},
        }))
        .send()
        .await
        .expect("post site sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("error body is json");
    assert_eq!(body["error"]["code"], json!("conflict"));
    assert!(srv.agent.transcript().is_empty(), "no agent call may fire");
}

#[tokio::test]
async fn update_site_409_while_job_running() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    publish(&srv, &owner, &hello_toml("0.1.1")).await;
    let server_id = online_server(&srv, "upd-409", 8192).await;
    let plan_id = default_plan_id(&srv).await;

    // A `running` site with a live job (enqueued straight in the repo,
    // so no race with the Runner).
    let site = srv
        .db
        .sites()
        .create(&setu_db::NewSite {
            name: unique("site"),
            app_id: "hello".to_string(),
            app_version: "0.1.0".to_string(),
            server_id: Some(server_id.clone()),
            plan_id,
            domain: format!("{}.example.com", unique("busy")),
            owner_email: "owner@example.com".to_string(),
            env_sealed: json!([]),
            plan_memory_mb: 512,
        })
        .await
        .expect("create site");
    srv.db
        .sites()
        .set_status(&site.id, "running", None)
        .await
        .expect("mark running");
    srv.db
        .jobs()
        .enqueue("backup_site", Some(&site.id), Some(&server_id), None)
        .await
        .expect("enqueue stuck job");

    let resp = owner
        .post(srv.url(&format!("/api/v1/sites/{}/update", site.id)))
        .json(&json!({"version": "0.1.1"}))
        .send()
        .await
        .expect("update sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CONFLICT);
    let body: Value = resp.json().await.expect("error body is json");
    assert_eq!(body["error"]["code"], json!("conflict"));
}

#[tokio::test]
async fn env_get_masks_secrets() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "env-1", 8192).await;
    let plan_id = default_plan_id(&srv).await;

    let resp = owner
        .post(srv.url("/api/v1/sites"))
        .json(&json!({
            "name": unique("site"),
            "app_id": "hello",
            "version": "0.1.0",
            "plan_id": plan_id,
            "server_id": server_id,
            "domain": format!("{}.example.com", unique("env")),
            "owner_email": "owner@example.com",
            "prompts": {"HELLO_NAME": "supersecret-value-1"},
        }))
        .send()
        .await
        .expect("post site sends");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let out: Value = resp.json().await.expect("create body is json");
    let site_id = out["site"]["id"].as_str().unwrap().to_string();
    let job_id = out["job"]["id"].as_str().unwrap().to_string();
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    let env_resp = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/env")))
        .send()
        .await
        .expect("get env sends");
    assert_eq!(env_resp.status(), reqwest::StatusCode::OK);
    let env: Value = env_resp.json().await.expect("env body is json");
    let vars = env["env"].as_array().expect("env array");

    // Prompt secret: masked, flagged, never plaintext.
    let name_var = vars
        .iter()
        .find(|v| v["name"] == json!("HELLO_NAME"))
        .expect("HELLO_NAME present");
    assert_eq!(name_var["value"], json!("********"));
    assert_eq!(name_var["masked"], json!(true));
    assert_eq!(name_var["secret"], json!(true));
    assert_eq!(name_var["has_value"], json!(true));
    let raw = serde_json::to_string(&env).expect("env serialises");
    assert!(
        !raw.contains("supersecret-value-1"),
        "prompt plaintext leaked in GET env"
    );

    // Generated secret: sealed at rest, masked over the API.
    let secret_var = vars
        .iter()
        .find(|v| v["name"] == json!("HELLO_SECRET"))
        .expect("HELLO_SECRET present");
    assert_eq!(secret_var["value"], json!("********"));

    // Plain value var: returned as-is.
    let label_var = vars
        .iter()
        .find(|v| v["name"] == json!("HELLO_LABEL"))
        .expect("HELLO_LABEL present");
    assert_eq!(label_var["value"], json!("hello-world"));
    assert_eq!(label_var["masked"], json!(false));

    // PUT: value + prompt vars change; generated vars are rejected.
    let put = owner
        .put(srv.url(&format!("/api/v1/sites/{site_id}/env")))
        .json(&json!({"env": {"HELLO_LABEL": "new-label", "HELLO_NAME": "second-secret"}}))
        .send()
        .await
        .expect("put env sends");
    assert_eq!(put.status(), reqwest::StatusCode::ACCEPTED);
    let put_body: Value = put.json().await.expect("put body is json");
    assert_eq!(put_body["job"]["kind"], json!("env_update"));
    let put_job = put_body["job"]["id"].as_str().unwrap().to_string();
    // The changes sit sealed in the payload, never plaintext.
    let row = srv.db.jobs().get(&put_job).await.expect("job row");
    let raw_payload = row.payload.to_string();
    assert!(!raw_payload.contains("second-secret") && !raw_payload.contains("new-label"));
    assert!(row.payload["changes"]["HELLO_NAME"]
        .as_str()
        .unwrap()
        .starts_with("v1:"));
    let done = poll_job(&srv, &owner, &put_job)
        .await
        .expect("env update done");
    assert_eq!(done["status"], json!("done"), "{done}");

    let env2: Value = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/env")))
        .send()
        .await
        .expect("get env sends")
        .json()
        .await
        .expect("env body is json");
    let vars2 = env2["env"].as_array().expect("env array");
    assert_eq!(
        vars2
            .iter()
            .find(|v| v["name"] == json!("HELLO_LABEL"))
            .expect("label")["value"],
        json!("new-label")
    );
    let raw2 = serde_json::to_string(&env2).expect("env serialises");
    assert!(!raw2.contains("second-secret"), "updated secret leaked");

    // The env_update job wrote the file, then restarted the site.
    let kinds = srv.agent.transcript();
    assert_eq!(
        kinds[kinds.len() - 2..],
        ["write_env", "restart"],
        "{kinds:?}"
    );

    for (key, expect) in [
        ("HELLO_SECRET", "generated"),
        ("HELLO_PORT", "generated"),
        ("NO_SUCH_VAR", "unknown env var"),
    ] {
        let bad = owner
            .put(srv.url(&format!("/api/v1/sites/{site_id}/env")))
            .json(&json!({"env": {key: "x"}}))
            .send()
            .await
            .expect("bad put sends");
        assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST, "PUT {key}");
        let err: Value = bad.json().await.expect("error body is json");
        assert_eq!(err["error"]["code"], json!("validation"));
        assert!(
            err["error"]["message"]
                .as_str()
                .unwrap_or("")
                .contains(expect),
            "PUT {key}: unexpected message {err}"
        );
    }
}

#[tokio::test]
async fn delete_site_runs_delete_job_and_removes_row() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "del-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("gone")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    let del = owner
        .delete(srv.url(&format!("/api/v1/sites/{site_id}")))
        .send()
        .await
        .expect("delete sends");
    assert_eq!(del.status(), reqwest::StatusCode::ACCEPTED);
    let del_body: Value = del.json().await.expect("delete body is json");
    let del_job = del_body["job"]["id"].as_str().expect("job id").to_string();

    // The delete job removes the site row (cascading its own job row);
    // poll until the job row is gone, then the site must 404 too.
    poll_job(&srv, &owner, &del_job).await;
    for _ in 0..40 {
        let got = owner
            .get(srv.url(&format!("/api/v1/sites/{site_id}")))
            .send()
            .await
            .expect("get sends");
        if got.status() == reqwest::StatusCode::NOT_FOUND {
            let err: Value = got.json().await.expect("error body is json");
            assert_eq!(err["error"]["code"], json!("not_found"));
            return;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("site {site_id} still present 20 s after delete finished");
}

#[tokio::test]
async fn stop_start_roundtrip() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "ss-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("ss")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    let stop_resp = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/stop")))
        .send()
        .await
        .expect("stop sends");
    assert_eq!(stop_resp.status(), reqwest::StatusCode::ACCEPTED);
    let stop: Value = stop_resp.json().await.expect("stop body is json");
    poll_job(&srv, &owner, stop["job"]["id"].as_str().unwrap())
        .await
        .expect("stop done");
    assert_eq!(
        get_site(&srv, &owner, &site_id).await["status"],
        json!("stopped")
    );

    // Stopping a stopped site is a 409 at the API (runner treats it as
    // idempotent, but the API guards the transition).
    let stop2 = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/stop")))
        .send()
        .await
        .expect("second stop sends");
    assert_eq!(stop2.status(), reqwest::StatusCode::CONFLICT);

    let start_resp = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/start")))
        .send()
        .await
        .expect("start sends");
    assert_eq!(start_resp.status(), reqwest::StatusCode::ACCEPTED);
    let start: Value = start_resp.json().await.expect("start body is json");
    poll_job(&srv, &owner, start["job"]["id"].as_str().unwrap())
        .await
        .expect("start done");
    assert_eq!(
        get_site(&srv, &owner, &site_id).await["status"],
        json!("running")
    );

    let kinds: Vec<String> = srv.agent.transcript();
    assert!(kinds.contains(&"stop".to_string()), "transcript: {kinds:?}");
    assert!(
        kinds.contains(&"start".to_string()),
        "transcript: {kinds:?}"
    );
}

#[tokio::test]
async fn logs_sse_streams_lines() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "logs-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("logs")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    srv.agent
        .set_log_lines(vec!["sse-alpha-1".to_string(), "sse-alpha-2".to_string()]);

    let mut resp = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/logs")))
        .send()
        .await
        .expect("logs sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        resp.headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or(""),
        "text/event-stream"
    );
    let mut acc = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if acc.contains("sse-alpha-1") && acc.contains("sse-alpha-2") {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("SSE lines missing after 15 s, got: {acc:?}");
        }
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("chunk arrives")
            .expect("chunk ok");
        match chunk {
            Some(bytes) => acc.push_str(&String::from_utf8_lossy(&bytes)),
            None => panic!("SSE stream ended early, got: {acc:?}"),
        }
    }
    assert!(acc.contains("data: sse-alpha-1"), "framing: {acc:?}");
}

#[tokio::test]
async fn job_log_sse_contains_steps() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "jlog-1", 8192).await;
    let (_site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("jlog")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    let mut resp = owner
        .get(srv.url(&format!("/api/v1/jobs/{job_id}/log")))
        .send()
        .await
        .expect("job log sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut acc = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        if acc.contains("step finish: ok") {
            break;
        }
        if tokio::time::Instant::now() > deadline {
            panic!("job log missing finish line, got: {acc:?}");
        }
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("chunk arrives")
            .expect("chunk ok");
        match chunk {
            Some(bytes) => acc.push_str(&String::from_utf8_lossy(&bytes)),
            None => break,
        }
    }
    assert!(acc.contains("step validate: ok"), "steps: {acc:?}");
    assert!(acc.contains("step health: ok"), "steps: {acc:?}");
}

#[tokio::test]
async fn backups_roundtrip_and_restore_enqueues() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "bk-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("bk")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    let take_resp = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/backups")))
        .send()
        .await
        .expect("backup sends");
    assert_eq!(take_resp.status(), reqwest::StatusCode::ACCEPTED);
    let take: Value = take_resp.json().await.expect("backup body is json");
    let bk_job = take["job"]["id"].as_str().expect("job id").to_string();
    let done = poll_job(&srv, &owner, &bk_job)
        .await
        .expect("backup job row");
    assert_eq!(done["status"], json!("done"));

    let list: Value = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/backups")))
        .send()
        .await
        .expect("list sends")
        .json()
        .await
        .expect("list body is json");
    let backups = list["backups"].as_array().expect("backups array");
    assert!(!backups.is_empty(), "manual backup row must exist");
    assert!(backups.iter().any(|b| b["status"] == json!("completed")));
    let bid = backups[0]["id"].as_str().expect("backup id").to_string();

    let restore_resp = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/backups/{bid}/restore")))
        .send()
        .await
        .expect("restore sends");
    assert_eq!(restore_resp.status(), reqwest::StatusCode::ACCEPTED);
    let restore: Value = restore_resp.json().await.expect("restore body is json");
    let rs_job = restore["job"]["id"].as_str().expect("job id").to_string();
    let rs_done = poll_job(&srv, &owner, &rs_job)
        .await
        .expect("restore job row");
    assert_eq!(rs_done["status"], json!("done"));
    assert_eq!(
        get_site(&srv, &owner, &site_id).await["status"],
        json!("running")
    );
}

#[tokio::test]
async fn audit_rows_written_for_mutations() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "audit-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("audit")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    let list: Value = owner
        .get(srv.url("/api/v1/audit?limit=50"))
        .send()
        .await
        .expect("audit sends")
        .json()
        .await
        .expect("audit body is json");
    let rows = list["audit"].as_array().expect("audit array");
    let actions: Vec<String> = rows
        .iter()
        .filter_map(|r| r["action"].as_str().map(str::to_string))
        .collect();
    for expect in ["app.publish", "site.create"] {
        assert!(
            actions.contains(&expect.to_string()),
            "actions: {actions:?}"
        );
    }
    let created = rows
        .iter()
        .find(|r| r["action"] == json!("site.create"))
        .expect("site.create row");
    assert_eq!(created["target"], json!(site_id));
}

/// Review-Focus #4 at API level: health passes but the app crashes
/// before the route check — the site ends `failed` (step
/// `verify_status`) and the route is removed again.
#[tokio::test]
async fn crash_after_health_before_route_is_failed() {
    let tdb = TestDb::new().await;
    let agent = FakeAgent::new();
    agent.set_status_active(false);
    let srv = TestServer::spawn(tdb, agent).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "crash-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("crash")),
    )
    .await;

    let job = poll_job(&srv, &owner, &job_id)
        .await
        .expect("provision job row survives");
    assert_eq!(job["status"], json!("failed"));

    let site = get_site(&srv, &owner, &site_id).await;
    assert_eq!(site["status"], json!("failed"));
    assert_eq!(site["failed_step"], json!("verify"));

    assert_eq!(
        srv.agent.transcript(),
        vec![
            "inventory",
            "site_create",
            "create_database",
            "install",
            "switch_version",
            "write_unit",
            "write_env",
            "start",
            "health",
            "set_route",
            "status",
            "remove_route",
        ]
    );
}

/// Review-Focus #5 at API level: an update whose binary checksum
/// mismatches fails before stopping the old site — no downtime, the
/// site stays `running` on the old version.
#[tokio::test]
async fn bad_checksum_update_never_stops_the_site() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    publish(&srv, &owner, &hello_toml("0.1.1")).await;
    let server_id = online_server(&srv, "cksum-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("cksum")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");
    assert_eq!(
        get_site(&srv, &owner, &site_id).await["status"],
        json!("running")
    );

    // The provision used Install #1; the update's Install #2 fails with
    // a checksum error.
    srv.agent.fail_with("install", 2, "checksum mismatch");

    let upd_resp = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/update")))
        .json(&json!({"version": "0.1.1"}))
        .send()
        .await
        .expect("update sends");
    assert_eq!(upd_resp.status(), reqwest::StatusCode::ACCEPTED);
    let upd: Value = upd_resp.json().await.expect("update body is json");
    let upd_job = upd["job"]["id"].as_str().expect("job id").to_string();
    let done = poll_job(&srv, &owner, &upd_job)
        .await
        .expect("update job row survives");
    assert_eq!(done["status"], json!("failed"));
    assert!(
        done["error"].as_str().unwrap_or("").contains("checksum"),
        "error: {done}"
    );

    let site = get_site(&srv, &owner, &site_id).await;
    assert_eq!(site["status"], json!("running"));
    assert_eq!(site["app_version"], json!("0.1.0"));
    assert!(
        !srv.agent.transcript().iter().any(|k| k == "stop"),
        "Install runs before Stop: transcript {:?}",
        srv.agent.transcript()
    );
}

/// `env_put` persists manifest-valid keys missing from stored env (the
/// manifest gained a var after provisioning): they must be stored and
/// returned, not silently dropped with a 202.
#[tokio::test]
async fn env_put_persists_var_missing_from_stored_env() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "envnew-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("envnew")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    // The manifest gains a var after provisioning (republish same
    // version with an extra `value` entry).
    let mut grown = hello_toml("0.1.0");
    grown = grown.replace(
        "HELLO_LABEL = { value = \"hello-world\" }",
        "HELLO_LABEL = { value = \"hello-world\" }\nHELLO_EXTRA = { value = \"dflt\" }",
    );
    publish(&srv, &owner, &grown).await;

    let put = owner
        .put(srv.url(&format!("/api/v1/sites/{site_id}/env")))
        .json(&json!({"env": {"HELLO_EXTRA": "brand-new"}}))
        .send()
        .await
        .expect("put env sends");
    assert_eq!(put.status(), reqwest::StatusCode::ACCEPTED);
    let put_body: Value = put.json().await.expect("put body is json");
    poll_job(&srv, &owner, put_body["job"]["id"].as_str().unwrap())
        .await
        .expect("restart done");

    let env: Value = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/env")))
        .send()
        .await
        .expect("get env sends")
        .json()
        .await
        .expect("env body is json");
    let vars = env["env"].as_array().expect("env array");
    assert_eq!(
        vars.iter()
            .find(|v| v["name"] == json!("HELLO_EXTRA"))
            .expect("HELLO_EXTRA persisted")["value"],
        json!("brand-new")
    );
}

/// Security finding 1: the database URL (it embeds the site's Postgres
/// password) never reaches a viewer, even when the stored row's
/// `secret` flag is false (rows written before `postgres_url` was
/// always secret, or tampered): `GET env` masks on the `secret` flag
/// and on the manifest source.
#[tokio::test]
async fn env_get_never_returns_postgres_url_to_viewers() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let with_db = hello_toml("0.1.0")
        .replace(
            "[needs]\ndirs = [\"media\"]",
            "[needs]\npostgres = \">=16\"\ndirs = [\"media\"]",
        )
        .replace(
            "HELLO_LABEL = { value = \"hello-world\" }",
            "HELLO_LABEL = { value = \"hello-world\" }\nHELLO_DB = { from = \"postgres_url\" }",
        );
    publish(&srv, &owner, &with_db).await;
    let server_id = online_server(&srv, "envdb-1", 8192).await;
    let (site_id, job_id) = post_site(
        &srv,
        &owner,
        Some(&server_id),
        &format!("{}.example.com", unique("envdb")),
    )
    .await;
    poll_job(&srv, &owner, &job_id).await.expect("provisioned");

    // Flip the stored flag off: the API must still mask the URL.
    sqlx::query(
        "UPDATE sites SET env = (SELECT jsonb_agg(CASE WHEN e->>'name' = 'HELLO_DB'
                                   THEN e || '{\"secret\": false}' ELSE e END)
                                  FROM jsonb_array_elements(env) e)
         WHERE id = $1",
    )
    .bind(&site_id)
    .execute(srv.db.pool())
    .await
    .expect("tamper flag");

    let viewer = srv.login_as("viewer").await;
    for client in [&viewer, &owner] {
        let resp = client
            .get(srv.url(&format!("/api/v1/sites/{site_id}/env")))
            .send()
            .await
            .expect("get env sends");
        assert_eq!(resp.status(), reqwest::StatusCode::OK);
        let text = resp.text().await.expect("env body");
        assert!(!text.contains("postgres://"), "database URL leaked");
        let env: Value = serde_json::from_str(&text).expect("env json");
        let db = env["env"]
            .as_array()
            .expect("env array")
            .iter()
            .find(|v| v["name"] == json!("HELLO_DB"))
            .expect("HELLO_DB listed")
            .clone();
        assert_eq!(db["masked"], json!(true));
        assert_eq!(db["secret"], json!(true));
    }
}

/// Publish 0.1.0, join a server and provision one site to `running`;
/// returns `(site_id, server_id)`.
async fn running_site(srv: &TestServer, owner: &reqwest::Client, tag: &str) -> (String, String) {
    publish(srv, owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(srv, &unique(tag), 8192).await;
    let (site_id, job_id) = post_site(
        srv,
        owner,
        Some(&server_id),
        &format!("{}.example.com", unique(tag)),
    )
    .await;
    let done = poll_job(srv, owner, &job_id).await.expect("provision job");
    assert_eq!(done["status"], json!("done"), "provision: {done}");
    (site_id, server_id)
}

async fn reveal(
    srv: &TestServer,
    client: &reqwest::Client,
    site_id: &str,
    name: &str,
    password: &str,
) -> reqwest::Response {
    client
        .post(srv.url(&format!("/api/v1/sites/{site_id}/env/{name}/reveal")))
        .json(&json!({"password": password}))
        .send()
        .await
        .expect("reveal sends")
}

async fn audit_actions(srv: &TestServer, action: &str) -> Vec<setu_db::AuditEntry> {
    srv.db
        .audit()
        .list_recent(200)
        .await
        .expect("audit")
        .into_iter()
        .filter(|a| a.action == action)
        .collect()
}

#[tokio::test]
async fn get_site_includes_latest_metric() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let viewer = srv.login_as("viewer").await;
    let (site_id, server_id) = running_site(&srv, &owner, "metric").await;

    let before = get_site(&srv, &viewer, &site_id).await;
    assert_eq!(before["id"], json!(site_id), "site fields are flattened");
    assert_eq!(before["metrics"], Value::Null, "no sample yet");

    let short = srv.db.sites().get(&site_id).await.expect("site").short_id;
    let now = chrono::Utc::now();
    let sample = |at: chrono::DateTime<chrono::Utc>, cpu: f64| setu_proto::SiteMetric {
        short_id: short.clone(),
        at,
        cpu_seconds: cpu,
        memory_bytes: 64 << 20,
        disk_bytes: 5 << 20,
    };
    srv.db
        .metrics()
        .insert_batch(
            &server_id,
            &[
                sample(now - chrono::Duration::minutes(1), 1.0),
                sample(now, 2.5),
            ],
        )
        .await
        .expect("insert metrics");
    let after = get_site(&srv, &viewer, &site_id).await;
    let m = &after["metrics"];
    assert_eq!(m["cpu_seconds"], json!(2.5), "the newest sample: {after}");
    assert_eq!(m["memory_bytes"], json!(64 << 20));
    assert_eq!(m["disk_bytes"], json!(5 << 20));
    assert_eq!(after["status"], json!("running"));
}

/// Review Focus 4: the control plane dies between `stop` and
/// `switch_version`; a new runner resumes the update from its cursor and
/// the site ends `running` on the new version.
#[tokio::test]
async fn update_resumes_after_runner_restart() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let (site_id, _) = running_site(&srv, &owner, "resume").await;
    publish(&srv, &owner, &hello_toml("0.1.1")).await;

    srv.agent.delay("switch_version", Duration::from_secs(30));
    let resp = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/update")))
        .json(&json!({"version": "0.1.1"}))
        .send()
        .await
        .expect("update sends");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let job_id = resp.json::<Value>().await.expect("json")["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();

    // Wait until the update stopped the site and is inside
    // `switch_version`, then kill the runner there.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let step = srv.db.jobs().get_step(&job_id).await.expect("step");
        if step.as_deref() == Some("switch_version") && srv.agent.switch_versions().len() == 2 {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "update never reached switch_version: {:?}",
            srv.agent.transcript()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    srv.kill_runner().await;
    let stops_before = srv.agent.call_count("stop");
    let installs_before = srv.agent.call_count("install");
    let mid = srv.db.sites().get(&site_id).await.expect("site");
    assert_eq!(mid.status, "updating", "stopped mid-update");
    assert_eq!(mid.app_version, "0.1.0");

    srv.agent.clear_delay("switch_version");
    srv.start_runner();
    let done = poll_job(&srv, &owner, &job_id).await.expect("job row");
    assert_eq!(done["status"], json!("done"), "resumed update: {done}");
    assert_eq!(done["attempts"], json!(2), "reclaimed by the new runner");

    let site = get_site(&srv, &owner, &site_id).await;
    assert_eq!(site["status"], json!("running"));
    assert_eq!(site["app_version"], json!("0.1.1"));
    // Resumed at the cursor: `switch_version` again, nothing before it.
    assert_eq!(
        srv.agent.switch_versions().last().map(String::as_str),
        Some("0.1.1")
    );
    assert_eq!(
        srv.agent.call_count("stop"),
        stops_before,
        "stop not re-run"
    );
    assert_eq!(srv.agent.call_count("install"), installs_before);
}

#[tokio::test]
async fn env_reveal_requires_password_and_is_audited_each_call() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let (site_id, _) = running_site(&srv, &owner, "reveal").await;
    let operator = srv.login_as("operator").await;
    let viewer = srv.login_as("viewer").await;

    // Anonymous 401, viewer 403 (operators and owners only).
    let anon = reveal(
        &srv,
        &setu_api::testing::new_client(),
        &site_id,
        "HELLO_NAME",
        "x",
    )
    .await;
    assert_eq!(anon.status(), reqwest::StatusCode::UNAUTHORIZED);
    let denied = reveal(&srv, &viewer, &site_id, "HELLO_NAME", "correct-horse-1").await;
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);

    // A wrong password is refused, audited, and reveals nothing.
    let wrong = reveal(&srv, &operator, &site_id, "HELLO_NAME", "not-my-password").await;
    assert_eq!(wrong.status(), reqwest::StatusCode::FORBIDDEN);
    let wrong_body = wrong.text().await.expect("body");
    assert!(!wrong_body.contains("test-display-name"));
    assert_eq!(audit_actions(&srv, "env.reveal_denied").await.len(), 1);

    // The right password reveals the secret, and every call is audited.
    for _ in 0..2 {
        let ok = reveal(&srv, &operator, &site_id, "HELLO_NAME", "correct-horse-1").await;
        assert_eq!(ok.status(), reqwest::StatusCode::OK);
        let body: Value = ok.json().await.expect("json");
        assert_eq!(body["value"], json!("test-display-name"));
        assert_eq!(body["name"], json!("HELLO_NAME"));
    }
    let reveals = audit_actions(&srv, "env.reveal").await;
    assert_eq!(reveals.len(), 2, "each call audited");
    assert!(reveals.iter().all(|a| {
        a.target_id.as_deref() == Some(format!("{site_id}:HELLO_NAME").as_str())
            && a.target_kind.as_deref() == Some("env")
            && a.ip.as_deref() == Some("127.0.0.1")
    }));

    // `once` values stay revealable; unknown names are 404.
    let once = reveal(&srv, &owner, &site_id, "HELLO_PASSWORD", "correct-horse-1").await;
    assert_eq!(once.status(), reqwest::StatusCode::OK);
    let once: Value = once.json().await.expect("json");
    assert_eq!(once["value"].as_str().map(str::len), Some(20));
    let missing = reveal(&srv, &owner, &site_id, "NOPE", "correct-horse-1").await;
    assert_eq!(missing.status(), reqwest::StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn env_put_rejects_generated_vars_and_enqueues_env_update() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let viewer = srv.login_as("viewer").await;
    let (site_id, _) = running_site(&srv, &owner, "envput").await;
    let url = srv.url(&format!("/api/v1/sites/{site_id}/env"));
    let put =
        |client: &reqwest::Client, env: Value| client.put(&url).json(&json!({"env": env})).send();

    for (name, why) in [
        ("HELLO_SECRET", "generated"),
        ("HELLO_PASSWORD", "generated once"),
        ("HELLO_PORT", "derived"),
        ("NOT_IN_MANIFEST", "unknown"),
    ] {
        let resp = put(&owner, json!({ name: "x" })).await.expect("put");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "{name} ({why})"
        );
    }
    let control = put(&owner, json!({"HELLO_LABEL": "a\nb"}))
        .await
        .expect("put");
    assert_eq!(control.status(), reqwest::StatusCode::BAD_REQUEST);
    let denied = put(&viewer, json!({"HELLO_LABEL": "v"}))
        .await
        .expect("put");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(srv.agent.call_count("restart"), 0, "nothing ran");

    let writes_before = srv.agent.write_env_calls().len();
    let ok = put(
        &owner,
        json!({"HELLO_NAME": "renamed-secret-9", "HELLO_LABEL": "relabelled"}),
    )
    .await
    .expect("put");
    assert_eq!(ok.status(), reqwest::StatusCode::ACCEPTED);
    let body: Value = ok.json().await.expect("json");
    assert_eq!(body["job"]["kind"], json!("env_update"));
    let job_id = body["job"]["id"].as_str().expect("job id").to_string();
    // The changes ride sealed: no plaintext in the job row.
    let row = srv.db.jobs().get(&job_id).await.expect("job row");
    let payload = row.payload.to_string();
    assert!(!payload.contains("renamed-secret-9"), "payload: {payload}");
    assert!(!payload.contains("relabelled"), "even non-secrets sealed");
    assert!(row.payload["changes"]["HELLO_NAME"]
        .as_str()
        .is_some_and(|v| v.starts_with("v1:")));

    let done = poll_job(&srv, &owner, &job_id).await.expect("job");
    assert_eq!(done["status"], json!("done"), "env_update: {done}");
    assert!(!done["log"]
        .as_str()
        .unwrap_or("")
        .contains("renamed-secret-9"));
    // write_env without `once`, then a restart of the running site.
    let writes = srv.agent.write_env_calls();
    assert_eq!(writes.len(), writes_before + 1);
    let last = writes.last().expect("write_env");
    let get = |n: &str| last.iter().find(|v| v.name == n).map(|v| v.value.clone());
    assert_eq!(get("HELLO_NAME").as_deref(), Some("renamed-secret-9"));
    assert_eq!(get("HELLO_LABEL").as_deref(), Some("relabelled"));
    assert_eq!(get("HELLO_PASSWORD"), None, "once values never rewritten");
    assert_eq!(srv.agent.call_count("restart"), 1);
    let shown = reveal(&srv, &owner, &site_id, "HELLO_NAME", "correct-horse-1").await;
    assert_eq!(
        shown.json::<Value>().await.expect("json")["value"],
        json!("renamed-secret-9")
    );
}

#[tokio::test]
async fn update_seals_prompt_answers_for_new_prompts() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let (site_id, _) = running_site(&srv, &owner, "prompts").await;
    let v2 = hello_toml("0.1.1").replace(
        "HELLO_LABEL = { value = \"hello-world\" }",
        "HELLO_LABEL = { value = \"hello-world\" }\nHELLO_CITY = { prompt = \"City\", secret = true }",
    );
    publish(&srv, &owner, &v2).await;
    let update = |body: Value| {
        owner
            .post(srv.url(&format!("/api/v1/sites/{site_id}/update")))
            .json(&body)
            .send()
    };

    // Only prompts of the target version are accepted.
    let bad = update(json!({"version": "0.1.1", "prompts": {"HELLO_LABEL": "x"}}))
        .await
        .expect("update");
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);

    let resp = update(json!({"version": "0.1.1", "prompts": {"HELLO_CITY": "atlantis-77"}}))
        .await
        .expect("update");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let job_id = resp.json::<Value>().await.expect("json")["job"]["id"]
        .as_str()
        .expect("job id")
        .to_string();
    let queued = srv.db.jobs().get(&job_id).await.expect("job").payload;
    assert!(!queued.to_string().contains("atlantis-77"), "{queued}");
    let done = poll_job(&srv, &owner, &job_id).await.expect("job");
    assert_eq!(done["status"], json!("done"), "update: {done}");
    assert!(!done["log"].as_str().unwrap_or("").contains("atlantis-77"));
    let finished = srv.db.jobs().get(&job_id).await.expect("job").payload;
    assert!(
        finished.get("prompts").is_none_or(Value::is_null),
        "answers leave the payload once stored: {finished}"
    );

    let site = get_site(&srv, &owner, &site_id).await;
    assert_eq!(site["app_version"], json!("0.1.1"));
    for (name, want) in [
        ("HELLO_CITY", "atlantis-77"),
        ("HELLO_NAME", "test-display-name"),
    ] {
        let shown = reveal(&srv, &owner, &site_id, name, "correct-horse-1").await;
        assert_eq!(
            shown.json::<Value>().await.expect("json")["value"],
            json!(want),
            "{name}"
        );
    }
    let last = srv.agent.write_env_calls();
    assert!(last
        .last()
        .expect("write_env")
        .iter()
        .any(|v| v.name == "HELLO_CITY" && v.value == "atlantis-77"));
}

#[tokio::test]
async fn delete_backup_enqueues_job_and_marks_row() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let viewer = srv.login_as("viewer").await;
    let (site_id, server_id) = running_site(&srv, &owner, "delbk").await;

    let take: Value = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/backups")))
        .send()
        .await
        .expect("backup sends")
        .json()
        .await
        .expect("json");
    let bk_job = take["job"]["id"].as_str().expect("job id").to_string();
    poll_job(&srv, &owner, &bk_job).await.expect("backup done");
    let list: Value = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/backups")))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    let bid = list["backups"][0]["id"].as_str().expect("bid").to_string();
    assert_eq!(list["backups"][0]["site_id"], json!(site_id));
    let url = srv.url(&format!("/api/v1/sites/{site_id}/backups/{bid}"));

    let denied = viewer.delete(&url).send().await.expect("delete");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    let wrong_site = owner
        .delete(srv.url(&format!("/api/v1/sites/{site_id}/backups/nope")))
        .send()
        .await
        .expect("delete");
    assert_eq!(wrong_site.status(), reqwest::StatusCode::NOT_FOUND);

    let resp = owner.delete(&url).send().await.expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["job"]["kind"], json!("delete_backup"));
    assert_eq!(
        body["job"]["site_id"],
        json!(site_id),
        "serialised with the site's jobs (M6)"
    );
    assert_eq!(body["job"]["server_id"], json!(server_id));
    let job_id = body["job"]["id"].as_str().expect("job id").to_string();
    let done = poll_job(&srv, &owner, &job_id).await.expect("job");
    assert_eq!(done["status"], json!("done"), "delete_backup: {done}");

    let row = srv.db.backups().get(&bid).await.expect("row kept");
    assert_eq!(row.status, "deleted");
    let cmds = srv.agent.commands_of("delete_backup");
    assert_eq!(cmds.len(), 1);
    assert_eq!(cmds[0]["backup_id"], json!(bid));
    let list: Value = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/backups")))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    assert!(list["backups"]
        .as_array()
        .expect("array")
        .iter()
        .all(|b| b["id"] != json!(bid)));
    let again = owner.delete(&url).send().await.expect("delete");
    assert_eq!(again.status(), reqwest::StatusCode::CONFLICT);
    assert_eq!(audit_actions(&srv, "backup.delete").await.len(), 1);
}

/// A deleted site's `pre_delete` backup stays listed (with its domain)
/// under `GET /backups` and is deleted through `DELETE /backups/{bid}`:
/// the site-scoped routes 404 once the site row is gone (e2e
/// `delete_site`, ledger R117).
#[tokio::test]
async fn deleted_sites_pre_delete_backup_is_listed_and_deletable() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let viewer = srv.login_as("viewer").await;
    let (site_id, server_id) = running_site(&srv, &owner, "orphan").await;
    let domain = srv.db.sites().get(&site_id).await.expect("site").domain;

    let del: Value = owner
        .delete(srv.url(&format!("/api/v1/sites/{site_id}")))
        .send()
        .await
        .expect("delete sends")
        .json()
        .await
        .expect("json");
    let del_job = del["job"]["id"].as_str().expect("job id").to_string();
    poll_job(&srv, &owner, &del_job).await;
    let mut gone = false;
    for _ in 0..40 {
        if srv.db.sites().get(&site_id).await.is_err() {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(gone, "site row still present");

    let list: Value = viewer
        .get(srv.url("/api/v1/backups"))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    let pre = list["backups"]
        .as_array()
        .expect("array")
        .iter()
        .find(|b| b["kind"] == json!("pre_delete") && b["domain"] == json!(domain))
        .cloned()
        .unwrap_or_else(|| panic!("pre_delete backup not listed: {list}"));
    assert_eq!(pre["site_id"], Value::Null);
    let bid = pre["id"].as_str().expect("bid").to_string();
    let url = srv.url(&format!("/api/v1/backups/{bid}"));

    let denied = viewer.delete(&url).send().await.expect("delete");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    let resp = owner.delete(&url).send().await.expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
    let body: Value = resp.json().await.expect("json");
    assert_eq!(body["job"]["kind"], json!("delete_backup"));
    assert_eq!(body["job"]["server_id"], json!(server_id));
    let job_id = body["job"]["id"].as_str().expect("job id").to_string();
    let done = poll_job(&srv, &owner, &job_id).await.expect("job");
    assert_eq!(done["status"], json!("done"), "delete_backup: {done}");
    assert_eq!(
        srv.db.backups().get(&bid).await.expect("row").status,
        "deleted"
    );
    let list: Value = owner
        .get(srv.url("/api/v1/backups"))
        .send()
        .await
        .expect("list")
        .json()
        .await
        .expect("json");
    assert!(list["backups"]
        .as_array()
        .expect("array")
        .iter()
        .all(|b| b["id"] != json!(bid)));
    let again = owner.delete(&url).send().await.expect("delete");
    assert_eq!(again.status(), reqwest::StatusCode::CONFLICT);
}

#[tokio::test]
async fn logs_sse_uses_cursor_without_duplicates() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let (site_id, _) = running_site(&srv, &owner, "cursor").await;
    let lines: Vec<String> = (1..=3).map(|i| format!("cursor-line-{i}")).collect();
    srv.agent.set_log_lines(lines.clone());

    let mut resp = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/logs")))
        .send()
        .await
        .expect("logs sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut acc = String::new();
    let mut read_for = async |acc: &mut String, how_long: Duration| {
        let until = tokio::time::Instant::now() + how_long;
        while let Ok(chunk) = tokio::time::timeout_at(until, resp.chunk()).await {
            match chunk.expect("chunk ok") {
                Some(bytes) => acc.push_str(&String::from_utf8_lossy(&bytes)),
                None => break,
            }
        }
    };
    // Three pages at 2 s: the first returns every line, the rest are
    // empty because they start from the returned cursor.
    read_for(&mut acc, Duration::from_millis(4500)).await;
    let mut more = lines.clone();
    more.push("cursor-line-4".to_string());
    srv.agent.set_log_lines(more);
    read_for(&mut acc, Duration::from_millis(2500)).await;

    for line in [
        "cursor-line-1",
        "cursor-line-2",
        "cursor-line-3",
        "cursor-line-4",
    ] {
        assert_eq!(
            acc.matches(&format!("data: {line}\n")).count(),
            1,
            "{line} exactly once in {acc:?}"
        );
    }
    let fetches = srv.agent.commands_of("fetch_logs");
    assert!(fetches.len() >= 3, "{fetches:?}");
    assert_eq!(
        fetches[0]["cursor"],
        Value::Null,
        "first page from the tail"
    );
    assert!(
        fetches[1..].iter().all(|f| f["cursor"].as_str().is_some()),
        "later pages pass the previous cursor: {fetches:?}"
    );
}

/// A site domain equal to the panel's host (or `localhost`, or a joined
/// server's hostname) would let the site route shadow `setu-panel`.
#[tokio::test]
async fn create_site_rejects_panel_host_domain() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn_with_public_url(
        tdb,
        FakeAgent::new(),
        "https://Panel.Example.com/".parse().expect("url"),
    )
    .await;
    let owner = srv.login_as_owner().await;
    publish(&srv, &owner, &hello_toml("0.1.0")).await;
    let server_id = online_server(&srv, "box-1.example.net", 8192).await;
    let plan_id = default_plan_id(&srv).await;
    for domain in [
        "panel.example.com",
        "PANEL.example.com.",
        "box-1.example.net",
        "localhost.localdomain",
        "app.localhost",
    ] {
        let resp = owner
            .post(srv.url("/api/v1/sites"))
            .json(&json!({
                "name": unique("site"),
                "app_id": "hello",
                "version": "0.1.0",
                "plan_id": plan_id,
                "server_id": server_id,
                "domain": domain,
                "owner_email": "owner@example.com",
                "prompts": {"HELLO_NAME": "x"},
            }))
            .send()
            .await
            .expect("post sends");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::BAD_REQUEST,
            "domain {domain} must be refused"
        );
        let body: Value = resp.json().await.expect("error body is json");
        assert_eq!(body["error"]["code"], json!("validation"), "{domain}");
    }
    assert!(srv.agent.transcript().is_empty(), "no agent call may fire");
}

/// The logs SSE task notices a gone client: it sends a keep-alive
/// comment every poll, checks `is_closed`, and stops calling the agent.
#[tokio::test]
async fn logs_sse_stops_when_client_disconnects() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let (site_id, _) = running_site(&srv, &owner, "disc").await;
    srv.agent.set_log_lines(vec!["disc-line-1".to_string()]);

    let mut resp = owner
        .get(srv.url(&format!("/api/v1/sites/{site_id}/logs")))
        .send()
        .await
        .expect("logs sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let mut acc = String::new();
    while !acc.contains("disc-line-1") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("chunk arrives")
            .expect("chunk ok")
            .expect("stream open");
        acc.push_str(&String::from_utf8_lossy(&chunk));
    }
    // A keep-alive comment arrives on an idle poll.
    while !acc.contains(": keep-alive") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), resp.chunk())
            .await
            .expect("keep-alive arrives")
            .expect("chunk ok")
            .expect("stream open");
        acc.push_str(&String::from_utf8_lossy(&chunk));
    }
    drop(resp);
    drop(owner);
    // At most one poll in flight when the client went away.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let after_drop = srv.agent.commands_of("fetch_logs").len();
    tokio::time::sleep(Duration::from_secs(6)).await;
    let later = srv.agent.commands_of("fetch_logs").len();
    assert_eq!(
        later, after_drop,
        "no FetchLogs after the client disconnected ({after_drop} → {later})"
    );
}

/// The job log SSE tails until the job finishes (no fixed 30 s), ends
/// with an `end` event naming the status, and escapes CR/LF inside a
/// line so one log line is one `data:` field.
#[tokio::test]
async fn job_log_sse_tails_until_done_and_escapes_lines() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    srv.kill_runner().await;
    let owner = srv.login_as_owner().await;
    let job = srv
        .db
        .jobs()
        .enqueue("delete_backup", None, None, None)
        .await
        .expect("enqueue");
    let claimed = srv
        .db
        .jobs()
        .claim("w-sse", Duration::from_secs(120))
        .await
        .expect("claim")
        .expect("a job");
    assert_eq!(claimed.id, job.id);

    let mut resp = owner
        .get(srv.url(&format!("/api/v1/jobs/{}/log", job.id)))
        .send()
        .await
        .expect("job log sends");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let db = srv.db.clone();
    let id = job.id.clone();
    let writer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_secs(1)).await;
        db.jobs()
            .append_log(&id, "w-sse", "first\rinjected: x\n")
            .await
            .expect("append");
        tokio::time::sleep(Duration::from_secs(1)).await;
        db.jobs()
            .append_log(&id, "w-sse", "second line\n")
            .await
            .expect("append");
        db.jobs()
            .finish(&id, "w-sse", true, None)
            .await
            .expect("finish");
    });
    let mut acc = String::new();
    let ended = loop {
        match tokio::time::timeout(Duration::from_secs(10), resp.chunk())
            .await
            .expect("chunk arrives")
            .expect("chunk ok")
        {
            Some(bytes) => acc.push_str(&String::from_utf8_lossy(&bytes)),
            None => break true,
        }
    };
    writer.await.expect("writer");
    assert!(ended);
    assert!(acc.contains("data: first\\rinjected: x\n\n"), "{acc:?}");
    assert!(acc.contains("data: second line\n\n"), "{acc:?}");
    assert!(
        acc.trim_end().ends_with("event: end\ndata: done"),
        "{acc:?}"
    );
}

/// A backup of a live site is deleted by a job on that site, so the
/// one-active-job-per-site index keeps it from racing the site's update
/// or restore that may read the backup (M6): while the site has an
/// active job the delete is 409, and the other way round.
#[tokio::test]
async fn delete_backup_serialises_with_the_sites_jobs() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let (site_id, _) = running_site(&srv, &owner, "delser").await;
    let take: Value = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/backups")))
        .send()
        .await
        .expect("backup sends")
        .json()
        .await
        .expect("json");
    let bk_job = take["job"]["id"].as_str().expect("job id").to_string();
    poll_job(&srv, &owner, &bk_job).await.expect("backup done");
    let bid = srv.db.backups().list_all().await.expect("list")[0]
        .id
        .clone();
    let url = srv.url(&format!("/api/v1/sites/{site_id}/backups/{bid}"));

    srv.kill_runner().await;
    let restart = owner
        .post(srv.url(&format!("/api/v1/sites/{site_id}/restart")))
        .send()
        .await
        .expect("restart");
    assert_eq!(restart.status(), reqwest::StatusCode::ACCEPTED);
    let busy = owner.delete(&url).send().await.expect("delete");
    assert_eq!(busy.status(), reqwest::StatusCode::CONFLICT);
    let any = owner
        .delete(srv.url(&format!("/api/v1/backups/{bid}")))
        .send()
        .await
        .expect("delete any");
    assert_eq!(any.status(), reqwest::StatusCode::CONFLICT);

    srv.start_runner();
    let restart_job: Value = restart.json().await.expect("json");
    let rj = restart_job["job"]["id"].as_str().expect("id").to_string();
    poll_job(&srv, &owner, &rj).await.expect("restart done");
    let resp = owner.delete(&url).send().await.expect("delete");
    assert_eq!(resp.status(), reqwest::StatusCode::ACCEPTED);
}
