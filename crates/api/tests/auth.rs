//! Auth integration tests: login cookie flow and the login rate limit.

use setu_api::testing::TestServer;
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

#[tokio::test]
async fn login_sets_cookie_and_me_returns_operator() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("owner", "correct-horse-1", "owner")
        .await;

    let resp = srv.login(&srv.client, &op.email, "correct-horse-1").await;
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let cookies: Vec<String> = resp
        .headers()
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().unwrap().to_string())
        .collect();
    assert!(
        cookies.iter().any(|c| c.starts_with("setu_session=")),
        "login must set the session cookie, got: {cookies:?}"
    );

    let me = srv
        .client
        .get(srv.url("/api/v1/auth/me"))
        .send()
        .await
        .expect("me request sends");
    assert_eq!(me.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = me.json().await.expect("me body is json");
    assert_eq!(body["email"], serde_json::json!(op.email));
    assert_eq!(body["role"], serde_json::json!("owner"));
}

#[tokio::test]
async fn wrong_password_is_401_and_rate_limited_after_10() {
    let tdb = TestDb::new().await;
    let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
    let op = srv
        .create_operator("owner", "correct-horse-1", "owner")
        .await;

    for i in 1..=10 {
        let resp = srv.login(&srv.client, &op.email, "wrong-password").await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "failure {i} must be 401"
        );
    }
    let limited = srv.login(&srv.client, &op.email, "wrong-password").await;
    assert_eq!(
        limited.status(),
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        "11th failure must be 429"
    );
    let body: serde_json::Value = limited.json().await.expect("429 body is json");
    assert_eq!(body["error"]["code"], serde_json::json!("rate_limited"));
}

fn session_cookie(headers: &reqwest::header::HeaderMap) -> String {
    headers
        .get_all("set-cookie")
        .iter()
        .map(|v| v.to_str().expect("set-cookie is ascii").to_string())
        .find(|c| c.starts_with("setu_session="))
        .expect("login must set the session cookie")
}

#[tokio::test]
async fn secure_cookie_flag_follows_public_url_scheme() {
    // Plain-http control plane: no `Secure` attribute, or browsers would
    // drop the cookie.
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

    // `https` public URL: the `Secure` attribute is set (transport stays
    // plain http in tests; only the Set-Cookie header is inspected).
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
        https_cookie.to_ascii_lowercase().contains("secure"),
        "https cookie must be Secure, got: {https_cookie}"
    );
}
