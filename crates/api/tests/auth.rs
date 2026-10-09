//! Auth integration tests (plan Task 7, spec §7): the session cookie,
//! logout and password changes ending sessions, the login limiter per
//! client IP, which `X-Forwarded-For` is believed, and the `Origin`
//! guard on mutations.

mod common;

use serde_json::{json, Value};
use setu_api::testing::{bare_client, new_client, TestOptions, TestServer};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

fn session_cookie(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().expect("set-cookie is ascii").to_string())
        .find(|c| c.starts_with("setu_session="))
        .expect("the response sets the session cookie")
}

/// `setu_session=<value>` from a `Set-Cookie` line.
fn cookie_pair(set_cookie: &str) -> String {
    set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string()
}

async fn me_with_cookie(srv: &TestServer, pair: &str) -> reqwest::StatusCode {
    bare_client()
        .get(srv.url("/api/v1/auth/me"))
        .header("cookie", pair)
        .send()
        .await
        .expect("me sends")
        .status()
}

#[tokio::test]
async fn login_sets_cookie_and_me_works() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("owner", "correct-horse-1", "owner")
        .await;

    let resp = srv.login(&srv.client, &op.email, "correct-horse-1").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let set = session_cookie(resp.headers());
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(set.contains(attr), "cookie lacks {attr}: {set}");
    }
    // The cookie is the random value; only its hash is stored.
    let pair = cookie_pair(&set);
    let value = pair.trim_start_matches("setu_session=");
    assert!(value.len() >= 43, "32 random bytes: {value}");
    let stored: Vec<String> = sqlx::query_scalar("SELECT id_hash FROM sessions")
        .fetch_all(srv.db.pool())
        .await
        .expect("sessions");
    assert!(
        stored.iter().all(|h| h != value),
        "cookie stored in plaintext"
    );

    let me = srv
        .client
        .get(srv.url("/api/v1/auth/me"))
        .send()
        .await
        .expect("me sends");
    assert_eq!(me.status(), reqwest::StatusCode::OK);
    // Authenticated responses slide the browser's cookie too.
    let refreshed = session_cookie(me.headers());
    assert_eq!(cookie_pair(&refreshed), pair);
    let body: Value = me.json().await.expect("me json");
    assert_eq!(body["email"], json!(op.email));
    assert_eq!(body["role"], json!("owner"));

    // Logout destroys the row: the old cookie no longer works anywhere.
    let out = srv
        .client
        .post(srv.url("/api/v1/auth/logout"))
        .send()
        .await
        .expect("logout sends");
    assert_eq!(out.status(), reqwest::StatusCode::NO_CONTENT);
    assert!(session_cookie(out.headers()).contains("Max-Age=0"));
    assert_eq!(
        me_with_cookie(&srv, &pair).await,
        reqwest::StatusCode::UNAUTHORIZED
    );
    let n: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions")
        .fetch_one(srv.db.pool())
        .await
        .expect("count");
    assert_eq!(n, 0);
}

#[tokio::test]
async fn cookie_is_secure_only_for_https_public_url() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("owner", "correct-horse-1", "owner")
        .await;
    let resp = srv.login(&srv.client, &op.email, "correct-horse-1").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let http_cookie = session_cookie(resp.headers());
    assert!(
        !http_cookie.to_ascii_lowercase().contains("secure"),
        "http cookie must not be Secure, got: {http_cookie}"
    );

    // `https` public URL (transport stays plain http in tests).
    let tdb = TestDb::new().await;
    let https_url: url::Url = "https://control.example".parse().expect("https url parses");
    let srv = TestServer::spawn_with_public_url(tdb, FakeAgent::new(), https_url).await;
    let op = srv
        .create_operator("owner", "correct-horse-1", "owner")
        .await;
    let resp = srv.login(&srv.client, &op.email, "correct-horse-1").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let https_cookie = session_cookie(resp.headers());
    assert!(
        https_cookie.ends_with("; Secure"),
        "https cookie must be Secure, got: {https_cookie}"
    );
}

#[tokio::test]
async fn password_change_logs_out_other_sessions() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("viewer", "correct-horse-1", "viewer")
        .await;
    let laptop = new_client();
    let phone = new_client();
    for c in [&laptop, &phone] {
        let r = srv.login(c, &op.email, "correct-horse-1").await;
        assert_eq!(r.status(), reqwest::StatusCode::OK);
    }
    let url = srv.url(&format!("/api/v1/operators/{}/password", op.id));

    // Your own password needs the current one.
    let wrong = laptop
        .put(&url)
        .json(&json!({"current_password": "nope-nope-1", "password": "new-horse-22"}))
        .send()
        .await
        .expect("put sends");
    assert_eq!(wrong.status(), reqwest::StatusCode::FORBIDDEN);
    let ok = laptop
        .put(&url)
        .json(&json!({"current_password": "correct-horse-1", "password": "new-horse-22"}))
        .send()
        .await
        .expect("put sends");
    assert_eq!(ok.status(), reqwest::StatusCode::NO_CONTENT);

    // The other session is gone; this browser got a fresh one.
    let phone_me = phone
        .get(srv.url("/api/v1/auth/me"))
        .send()
        .await
        .expect("me sends");
    assert_eq!(phone_me.status(), reqwest::StatusCode::UNAUTHORIZED);
    let laptop_me = laptop
        .get(srv.url("/api/v1/auth/me"))
        .send()
        .await
        .expect("me sends");
    assert_eq!(laptop_me.status(), reqwest::StatusCode::OK);
    let relog = srv.login(&phone, &op.email, "new-horse-22").await;
    assert_eq!(relog.status(), reqwest::StatusCode::OK);

    // A viewer cannot set someone else's password; the owner can, and
    // that ends the target's sessions.
    let other = srv
        .create_operator("operator", "correct-horse-1", "operator")
        .await;
    let other_client = new_client();
    srv.login(&other_client, &other.email, "correct-horse-1")
        .await;
    let other_url = srv.url(&format!("/api/v1/operators/{}/password", other.id));
    let denied = laptop
        .put(&other_url)
        .json(&json!({"password": "taken-over-1"}))
        .send()
        .await
        .expect("put sends");
    assert_eq!(denied.status(), reqwest::StatusCode::FORBIDDEN);
    let owner = srv.login_as_owner().await;
    let reset = owner
        .put(&other_url)
        .json(&json!({"password": "reset-by-owner-1"}))
        .send()
        .await
        .expect("put sends");
    assert_eq!(reset.status(), reqwest::StatusCode::NO_CONTENT);
    let other_me = other_client
        .get(srv.url("/api/v1/auth/me"))
        .send()
        .await
        .expect("me sends");
    assert_eq!(other_me.status(), reqwest::StatusCode::UNAUTHORIZED);
    let audit = srv.db.audit().list_recent(50).await.expect("audit");
    assert_eq!(
        audit
            .iter()
            .filter(|a| a.action == "operator.password")
            .count(),
        2
    );
}

#[tokio::test]
async fn login_rate_limited_after_10_per_ip() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("owner", "correct-horse-1", "owner")
        .await;
    // The test client is a trusted proxy (127.0.0.1): the limiter keys on
    // the forwarded client.
    let login = |ip: &'static str, password: &'static str| {
        new_client()
            .post(srv.url("/api/v1/auth/login"))
            .header("x-forwarded-for", ip)
            .json(&json!({"email": op.email, "password": password}))
            .send()
    };
    for i in 1..=10 {
        let resp = login("198.51.100.1", "wrong-password")
            .await
            .expect("login");
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "failure {i} must be 401"
        );
    }
    let limited = login("198.51.100.1", "correct-horse-1")
        .await
        .expect("login");
    assert_eq!(
        limited.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "11th attempt from that IP is 429, even with the right password"
    );
    let body: Value = limited.json().await.expect("429 body is json");
    assert_eq!(body["error"]["code"], json!("rate_limited"));
    let other = login("198.51.100.2", "correct-horse-1")
        .await
        .expect("login");
    assert_eq!(
        other.status(),
        reqwest::StatusCode::OK,
        "another client IP is not limited"
    );
}

async fn audit_ip_of_login(srv: &TestServer, email: &str) -> Option<String> {
    srv.db
        .audit()
        .list_recent(50)
        .await
        .expect("audit")
        .into_iter()
        .find(|a| a.action == "auth.login" && a.target_id.as_deref() == Some(email))
        .and_then(|a| a.ip)
}

#[tokio::test]
async fn forwarded_for_trusted_only_from_trusted_proxy_or_unix_peer() {
    // TCP peer 127.0.0.1 inside trusted_proxies: the header is believed
    // (rightmost untrusted hop; a forged left part is ignored).
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("viewer", "correct-horse-1", "viewer")
        .await;
    let resp = new_client()
        .post(srv.url("/api/v1/auth/login"))
        .header("x-forwarded-for", "6.6.6.6, 203.0.113.7")
        .json(&json!({"email": op.email, "password": "correct-horse-1"}))
        .send()
        .await
        .expect("login");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        audit_ip_of_login(&srv, &op.email).await.as_deref(),
        Some("203.0.113.7")
    );

    // TCP peer outside trusted_proxies: the header is ignored.
    let tdb = TestDb::new().await;
    let opts = TestOptions {
        trusted_proxies: Some(vec!["10.0.0.0/8".parse().expect("net")]),
        ..TestOptions::default()
    };
    let srv = TestServer::spawn_with(tdb, FakeAgent::new(), opts).await;
    let op = srv
        .create_operator("viewer", "correct-horse-1", "viewer")
        .await;
    let resp = new_client()
        .post(srv.url("/api/v1/auth/login"))
        .header("x-forwarded-for", "203.0.113.7")
        .json(&json!({"email": op.email, "password": "correct-horse-1"}))
        .send()
        .await
        .expect("login");
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    assert_eq!(
        audit_ip_of_login(&srv, &op.email).await.as_deref(),
        Some("127.0.0.1")
    );

    // Unix peer (Caddy by construction): believed whatever
    // trusted_proxies says.
    let dir = tempfile::tempdir().expect("tempdir");
    let sock = dir.path().join("http.sock");
    let _unix = srv.serve_unix(&sock).await;
    let op = srv
        .create_operator("viewer", "correct-horse-1", "viewer")
        .await;
    let body = json!({"email": op.email, "password": "correct-horse-1"}).to_string();
    let (status, _, _) = common::unix_request(
        &sock,
        "POST",
        "/api/v1/auth/login",
        &[
            ("x-forwarded-for", "198.51.100.23"),
            ("sec-fetch-site", "same-origin"),
            ("content-type", "application/json"),
        ],
        body,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        audit_ip_of_login(&srv, &op.email).await.as_deref(),
        Some("198.51.100.23")
    );
}

#[tokio::test]
async fn mutation_without_matching_origin_is_403() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let owner = srv.login_as_owner().await;
    let post = |client: &reqwest::Client, headers: &[(&'static str, String)]| {
        let mut req = client
            .post(srv.url("/api/v1/servers"))
            .json(&json!({"name": format!("o-{}", setu_common::new_id())}));
        for (k, v) in headers {
            req = req.header(*k, v.clone());
        }
        req.send()
    };

    // A cross-site page riding the owner's cookie.
    let cross = post(
        &owner,
        &[
            ("origin", "https://evil.example".to_string()),
            ("sec-fetch-site", "cross-site".to_string()),
        ],
    )
    .await
    .expect("post");
    assert_eq!(cross.status(), reqwest::StatusCode::FORBIDDEN);
    let err: Value = cross.json().await.expect("json");
    assert_eq!(err["error"]["code"], json!("forbidden"));
    // Same-site but another origin is still refused.
    let sibling = post(
        &owner,
        &[
            ("origin", "http://127.0.0.1:1".to_string()),
            ("sec-fetch-site", "same-site".to_string()),
        ],
    )
    .await
    .expect("post");
    assert_eq!(sibling.status(), reqwest::StatusCode::FORBIDDEN);
    // Neither header (a non-browser client): refused too, even for login.
    let bare = bare_client();
    let login = bare
        .post(srv.url("/api/v1/auth/login"))
        .json(&json!({"email": "x@example.com", "password": "whatever-1"}))
        .send()
        .await
        .expect("login");
    assert_eq!(login.status(), reqwest::StatusCode::FORBIDDEN);
    // `Origin` equal to the public URL passes the guard (then 401: no
    // session) — and with the owner's cookie it creates the server.
    let named = bare
        .post(srv.url("/api/v1/servers"))
        .header("origin", srv.origin())
        .json(&json!({"name": "o-anon"}))
        .send()
        .await
        .expect("post");
    assert_eq!(named.status(), reqwest::StatusCode::UNAUTHORIZED);
    let ok = post(&owner, &[("origin", srv.origin())])
        .await
        .expect("post");
    assert_eq!(ok.status(), reqwest::StatusCode::CREATED);
    // Reads are not guarded.
    let read = bare
        .get(srv.url("/api/v1/setup"))
        .send()
        .await
        .expect("get");
    assert_eq!(read.status(), reqwest::StatusCode::OK);
}
