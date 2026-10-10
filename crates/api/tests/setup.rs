//! Setup wizard integration tests (plan Task 7, spec §7–§8): the boot
//! setup token, the token-gated single-use owner creation, and the
//! local-server step's install command.

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use serde_json::{json, Value};
use setu_api::testing::{new_client, TestOptions, TestServer};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

async fn post_setup(srv: &TestServer, token: &str, email: &str) -> reqwest::StatusCode {
    new_client()
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"token": token, "email": email, "password": "correct-horse-1"}))
        .send()
        .await
        .expect("setup sends")
        .status()
}

#[tokio::test]
async fn boot_writes_setup_token_once() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let path = srv.data_dir().join("setup-token");

    // Boot (zero operators) wrote the token 0600 and stored its hash.
    let token = srv.setup_token().expect("boot wrote the setup token");
    assert!(
        token.len() >= 43,
        "32 random bytes base64url: {}",
        token.len()
    );
    let mode = std::fs::metadata(&path)
        .expect("token file")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "setup-token mode {mode:o}");
    let stored = srv
        .db
        .settings()
        .get(setu_db::SETUP_TOKEN_HASH)
        .await
        .expect("settings read");
    assert_eq!(
        stored,
        Some(json!(sha256_hex(&token))),
        "hash, never the token"
    );

    // A second boot keeps the same token.
    let again = setu_api::rest::setup::ensure_setup_token(&srv.db, &srv.data_dir())
        .await
        .expect("second boot");
    assert_eq!(again, None, "nothing written on the second boot");
    assert_eq!(srv.setup_token().as_deref(), Some(token.as_str()));

    // A boot whose file vanished mints a fresh one (only readable there).
    std::fs::remove_file(&path).expect("remove token file");
    let rewritten = setu_api::rest::setup::ensure_setup_token(&srv.db, &srv.data_dir())
        .await
        .expect("boot after the file vanished");
    assert_eq!(rewritten, Some(path.clone()));
    let fresh = srv.setup_token().expect("fresh token");
    assert_ne!(fresh, token);

    // Setup consumes it: file and hash are gone, and later boots (an
    // operator exists) write nothing.
    assert_eq!(
        post_setup(&srv, &fresh, "owner@example.com").await,
        reqwest::StatusCode::CREATED
    );
    assert!(!path.exists(), "setup removes the token file");
    assert_eq!(
        srv.db
            .settings()
            .get(setu_db::SETUP_TOKEN_HASH)
            .await
            .expect("settings read"),
        None
    );
    let after = setu_api::rest::setup::ensure_setup_token(&srv.db, &srv.data_dir())
        .await
        .expect("boot with an owner");
    assert_eq!(after, None);
    assert!(!path.exists());
}

#[tokio::test]
async fn setup_requires_token_and_creates_owner_once() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let anon = new_client();
    let token = srv.setup_token().expect("setup token");

    let status: Value = anon
        .get(srv.url("/api/v1/setup"))
        .send()
        .await
        .expect("status sends")
        .json()
        .await
        .expect("status json");
    assert_eq!(status["needed"], json!(true));

    // No token, or a wrong one: 401, and nothing is created.
    let missing = anon
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"email": "owner@example.com", "password": "correct-horse-1"}))
        .send()
        .await
        .expect("setup sends");
    assert_eq!(missing.status(), reqwest::StatusCode::BAD_REQUEST);
    assert_eq!(
        post_setup(&srv, "not-the-token", "owner@example.com").await,
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(srv.db.operators().count().await.expect("count"), 0);
    // The token is checked first (constant-time, before any argon2
    // hashing): a wrong token is 401 whatever the rest of the body (M12).
    let wrong_and_short = anon
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"token": "not-the-token", "email": "owner@example.com", "password": "short"}))
        .send()
        .await
        .expect("setup sends");
    assert_eq!(wrong_and_short.status(), reqwest::StatusCode::UNAUTHORIZED);

    // Invalid owner input is 400 even with the token.
    let bad = anon
        .post(srv.url("/api/v1/setup"))
        .json(&json!({"token": token, "email": "nope", "password": "correct-horse-1"}))
        .send()
        .await
        .expect("setup sends");
    assert_eq!(bad.status(), reqwest::StatusCode::BAD_REQUEST);

    // Two concurrent POSTs with the token and different emails: exactly
    // one owner (the advisory-locked transaction, not UNIQUE(email)).
    let (a, b) = tokio::join!(
        post_setup(&srv, &token, "first@example.com"),
        post_setup(&srv, &token, "second@example.com"),
    );
    let mut codes = [a.as_u16(), b.as_u16()];
    codes.sort_unstable();
    assert_eq!(codes, [201, 409], "exactly one owner, got {codes:?}");
    assert_eq!(srv.db.operators().count().await.expect("count"), 1);
    let owners = srv.db.operators().list().await.expect("list");
    assert_eq!(owners[0].role, "owner");

    // The owner logs in; setup is no longer needed and is 409 for
    // everyone afterwards, with or without the old token.
    let login = srv.login(&anon, &owners[0].email, "correct-horse-1").await;
    assert_eq!(login.status(), reqwest::StatusCode::OK);
    let status: Value = anon
        .get(srv.url("/api/v1/setup"))
        .send()
        .await
        .expect("status sends")
        .json()
        .await
        .expect("status json");
    assert_eq!(status["needed"], json!(false));
    for t in [token.as_str(), "anything"] {
        assert_eq!(
            post_setup(&srv, t, "late@example.com").await,
            reqwest::StatusCode::CONFLICT
        );
    }
    let audit = srv.db.audit().list_recent(10).await.expect("audit");
    assert!(audit.iter().any(|a| a.action == "setup.create-owner"));
}

#[tokio::test]
async fn local_server_command_targets_loopback_or_public_url() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;

    // Owner only: anonymous 401, an operator 403.
    let denied = new_client()
        .post(srv.url("/api/v1/setup/local-server"))
        .json(&json!({}))
        .send()
        .await
        .expect("anon sends");
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
    let operator = srv.login_as("operator").await;
    let forbidden = operator
        .post(srv.url("/api/v1/setup/local-server"))
        .json(&json!({}))
        .send()
        .await
        .expect("operator sends");
    assert_eq!(forbidden.status(), reqwest::StatusCode::FORBIDDEN);

    let owner = srv.login_as_owner().await;
    let resp = owner
        .post(srv.url("/api/v1/setup/local-server"))
        .json(&json!({}))
        .send()
        .await
        .expect("local-server sends");
    assert_eq!(resp.status(), reqwest::StatusCode::CREATED);
    let body: Value = resp.json().await.expect("body");
    let join_token = body["join_token"].as_str().expect("join_token");
    assert!(join_token.starts_with("setu_join_"));
    let command = body["install_command"].as_str().expect("install_command");
    // TCP listener: the local agent joins over loopback.
    assert_eq!(
        command,
        format!(
            "setu agent install --control-plane http://127.0.0.1:{}",
            srv.addr.port()
        )
    );
    assert!(!command.contains(join_token), "token never in the command");
    assert_eq!(body["server"]["status"], json!("pending"));
    assert_eq!(body["server"]["is_control_plane"], json!(true));
    let row = srv
        .db
        .servers()
        .get(body["server"]["id"].as_str().expect("id"))
        .await
        .expect("server row");
    assert!(row.is_control_plane);

    // The join token joins (agent side, no session).
    let join = reqwest::Client::new()
        .post(srv.url("/agent/v1/join"))
        .json(&json!({
            "join_token": join_token, "nonce": "setup-nonce", "hostname": "local",
            "arch": "x86_64", "os": "linux", "cpu_cores": 4, "memory_mb": 8192,
            "disk_mb": 102400,
        }))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .expect("join sends");
    assert_eq!(join.status(), reqwest::StatusCode::OK);

    // Unix-socket install: through Caddy at the public URL, plus Caddy's
    // local CA when `tls internal`.
    let tdb = TestDb::new().await;
    let opts = TestOptions {
        public_url: Some("https://panel.example/".parse().expect("url")),
        config_listen: Some(setu_common::Listen::Unix("/run/setu/http.sock".into())),
        ..TestOptions::default()
    };
    let unix = TestServer::spawn_with(tdb, FakeAgent::new(), opts).await;
    let owner = unix.login_as_owner().await;
    let local = |client: reqwest::Client| {
        let url = unix.url("/api/v1/setup/local-server");
        async move {
            let body: Value = client
                .post(url)
                .json(&json!({"name": "panel"}))
                .send()
                .await
                .expect("local-server sends")
                .json()
                .await
                .expect("body");
            body["install_command"]
                .as_str()
                .expect("command")
                .to_string()
        }
    };
    assert_eq!(
        local(owner.clone()).await,
        "setu agent install --control-plane https://panel.example --panel-host panel.example"
    );
    unix.db
        .settings()
        .set(setu_db::TLS_INTERNAL, &json!(true))
        .await
        .expect("set tls_internal");
    assert_eq!(
        local(owner).await,
        "setu agent install --control-plane https://panel.example \
         --panel-host panel.example --tls internal \
         --ca /var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt"
    );
}
