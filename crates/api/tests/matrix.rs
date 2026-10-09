//! Authz route matrix: every route × {anonymous, viewer, operator, owner}
//! → the expected status (mirrors the plan's Task 7 matrix).

use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use setu_api::testing::{new_client, TestServer};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

const HELLO_TOML: &str = r#"schema = 1

[app]
id = "hello"
name = "Hello"
version = "0.1.0"
homepage = "https://example.com"
health = "/healthz"
admin_path = "/"

[run]
kind = "binary"
args = ["serve"]

[run.binary]
"x86_64-linux" = { url = "https://example.com/hello-0.1.0-x86_64.tar.gz", sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", exec = "hello" }

[needs]
dirs = []

[env]
HELLO_PORT = { from = "port" }
HELLO_NAME = { prompt = "Display name", secret = false }

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = false

[update]
strategy = "replace"
"#;

struct Ctx {
    srv: TestServer,
    anon: Client,
    viewer: Client,
    operator: Client,
    owner: Client,
    viewer_email: String,
    operator_email: String,
    owner_email: String,
}

impl Ctx {
    async fn new() -> Self {
        let tdb = TestDb::new().await;
        let srv = TestServer::spawn(tdb, FakeAgent::new()).await;
        let mk = async |role: &str| {
            let op = srv.create_operator(role, "correct-horse-1", role).await;
            let client = new_client();
            let resp = srv.login(&client, &op.email, "correct-horse-1").await;
            assert_eq!(resp.status(), StatusCode::OK, "login_as({role})");
            (client, op.email)
        };
        let (viewer, viewer_email) = mk("viewer").await;
        let (operator, operator_email) = mk("operator").await;
        let (owner, owner_email) = mk("owner").await;
        Self {
            srv,
            anon: new_client(),
            viewer,
            operator,
            owner,
            viewer_email,
            operator_email,
            owner_email,
        }
    }

    async fn call(&self, who: &str, method: &str, path: &str, body: Option<Value>) -> StatusCode {
        let client = match who {
            "anon" => &self.anon,
            "viewer" => &self.viewer,
            "operator" => &self.operator,
            "owner" => &self.owner,
            _ => panic!("unknown role {who}"),
        };
        let url = self.srv.url(path);
        let mut req = match method {
            "GET" => client.get(&url),
            "POST" => client.post(&url),
            "PUT" => client.put(&url),
            "DELETE" => client.delete(&url),
            _ => panic!("unknown method {method}"),
        };
        if let Some(b) = body {
            req = req.json(&b);
        }
        req.send().await.expect("matrix request sends").status()
    }

    /// One matrix row: `call` for each role must return the expected status.
    #[allow(clippy::too_many_arguments)]
    async fn row(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        anon: u16,
        viewer: u16,
        operator: u16,
        owner: u16,
    ) {
        // Fresh bodies per call: `Value` is cheaply cloned.
        for (who, expect) in [
            ("anon", anon),
            ("viewer", viewer),
            ("operator", operator),
            ("owner", owner),
        ] {
            let got = self.call(who, method, path, body.clone()).await;
            assert_eq!(
                got.as_u16(),
                expect,
                "{method} {path} as {who}: expected {expect}, got {got}"
            );
        }
    }
}

fn unique(suffix: &str) -> String {
    format!("{suffix}-{}", setu_common::new_id()[..8].to_lowercase())
}

#[tokio::test]
async fn route_matrix() {
    let ctx = Ctx::new().await;

    // Probes are public.
    ctx.row("GET", "/healthz", None, 200, 200, 200, 200).await;
    ctx.row("GET", "/readyz", None, 200, 200, 200, 200).await;

    // Auth: bad credentials are 401 for everyone; good ones 200.
    ctx.row(
        "POST",
        "/api/v1/auth/login",
        Some(json!({"email": "nobody@example.com", "password": "nope-nope-nope"})),
        401,
        401,
        401,
        401,
    )
    .await;
    for (who, email) in [
        ("viewer", &ctx.viewer_email),
        ("operator", &ctx.operator_email),
        ("owner", &ctx.owner_email),
    ] {
        let got = ctx
            .call(
                who,
                "POST",
                "/api/v1/auth/login",
                Some(json!({"email": email, "password": "correct-horse-1"})),
            )
            .await;
        assert_eq!(got.as_u16(), 200, "login as {who}");
    }

    // /me: authed 200, anon 401.
    ctx.row("GET", "/api/v1/auth/me", None, 401, 200, 200, 200)
        .await;

    // Operators: owner only.
    ctx.row("GET", "/api/v1/operators", None, 401, 403, 403, 200)
        .await;
    ctx.row(
        "POST",
        "/api/v1/operators",
        Some(json!({"email": format!("{}@example.com", unique("mx-op")), "password": "password-123", "role": "viewer"})),
        401, 403, 403, 201,
    )
    .await;

    // Setup: GET is public; POST is public-once (operators already
    // exist in this context, so every role — anonymous included — gets
    // 409, never 401); local-server needs operator+.
    ctx.row("GET", "/api/v1/setup", None, 200, 200, 200, 200)
        .await;
    ctx.row(
        "POST",
        "/api/v1/setup",
        Some(json!({"token": "whatever", "email": "late@example.com", "password": "correct-horse-1"})),
        409,
        409,
        409,
        409,
    )
    .await;
    // local-server is the owner's (it creates the control-plane server).
    ctx.row(
        "POST",
        "/api/v1/setup/local-server",
        Some(json!({})),
        401,
        403,
        403,
        201,
    )
    .await;

    // Password changes: someone else's is the owner's alone.
    ctx.row(
        "PUT",
        "/api/v1/operators/does-not-exist/password",
        Some(json!({"password": "new-password-1"})),
        401,
        403,
        403,
        404,
    )
    .await;

    // The OpenAPI document needs a session; Swagger UI is gone.
    ctx.row("GET", "/api/v1/openapi.json", None, 401, 200, 200, 200)
        .await;
    ctx.row("GET", "/api/docs/", None, 404, 404, 404, 404).await;
    ctx.row("GET", "/api/openapi.json", None, 404, 404, 404, 404)
        .await;

    // Servers: reads viewer+, writes operator+.
    ctx.row("GET", "/api/v1/servers", None, 401, 200, 200, 200)
        .await;
    ctx.row(
        "POST",
        "/api/v1/servers",
        Some(json!({"name": unique("mx-srv-viewer")})),
        401,
        403,
        201,
        201,
    )
    .await;
    // NOTE: operator and owner each created one server above (unique names),
    // so both writes returned 201 without name collisions.

    // Detail + delete fixtures.
    let mk_server = async |name: &str| -> String {
        let resp = ctx
            .owner
            .post(ctx.srv.url("/api/v1/servers"))
            .json(&json!({"name": name}))
            .send()
            .await
            .expect("create fixture server");
        assert_eq!(resp.status(), StatusCode::CREATED);
        let body: Value = resp.json().await.expect("fixture body");
        body["server"]["id"].as_str().unwrap().to_string()
    };
    let keep = mk_server(&unique("mx-keep")).await;
    let del_op = mk_server(&unique("mx-del-op")).await;
    let del_own = mk_server(&unique("mx-del-own")).await;

    ctx.row(
        "GET",
        &format!("/api/v1/servers/{keep}"),
        None,
        401,
        200,
        200,
        200,
    )
    .await;
    ctx.row(
        "GET",
        "/api/v1/servers/does-not-exist",
        None,
        401,
        404,
        404,
        404,
    )
    .await;
    ctx.row(
        "POST",
        &format!("/api/v1/servers/{keep}/cordon"),
        Some(json!({"cordoned": true})),
        401,
        403,
        200,
        200,
    )
    .await;
    ctx.row(
        "POST",
        "/api/v1/servers/does-not-exist/cordon",
        Some(json!({"cordoned": false})),
        401,
        403,
        404,
        404,
    )
    .await;
    // Deletes: viewer is forbidden even for a real id; operator/owner remove.
    assert_eq!(
        ctx.call("anon", "DELETE", &format!("/api/v1/servers/{del_op}"), None)
            .await
            .as_u16(),
        401
    );
    assert_eq!(
        ctx.call(
            "viewer",
            "DELETE",
            &format!("/api/v1/servers/{del_op}"),
            None
        )
        .await
        .as_u16(),
        403
    );
    assert_eq!(
        ctx.call(
            "operator",
            "DELETE",
            &format!("/api/v1/servers/{del_op}"),
            None
        )
        .await
        .as_u16(),
        204
    );
    assert_eq!(
        ctx.call(
            "owner",
            "DELETE",
            &format!("/api/v1/servers/{del_own}"),
            None
        )
        .await
        .as_u16(),
        204
    );

    // Apps: reads viewer+, validate/publish operator+.
    ctx.row("GET", "/api/v1/apps", None, 401, 200, 200, 200)
        .await;
    ctx.row(
        "POST",
        "/api/v1/apps/validate",
        Some(json!({"toml": HELLO_TOML})),
        401,
        403,
        200,
        200,
    )
    .await;
    ctx.row(
        "POST",
        "/api/v1/apps",
        Some(json!({"toml": HELLO_TOML})),
        401,
        403,
        201,
        201,
    )
    .await;
    // NOTE: operator and owner publish the same version; upsert makes both 201.
    ctx.row(
        "GET",
        "/api/v1/apps/hello/versions",
        None,
        401,
        200,
        200,
        200,
    )
    .await;
    ctx.row(
        "GET",
        "/api/v1/apps/nope/versions",
        None,
        401,
        404,
        404,
        404,
    )
    .await;

    // Plans: reads viewer+, writes owner only.
    ctx.row("GET", "/api/v1/plans", None, 401, 200, 200, 200)
        .await;
    let plan_body = || {
        Some(json!({
            "name": unique("mx-plan"),
            "limits": {"memory_mb": 256, "cpu_percent": 50, "disk_mb": 1024},
        }))
    };
    assert_eq!(
        ctx.call("anon", "POST", "/api/v1/plans", plan_body())
            .await
            .as_u16(),
        401
    );
    assert_eq!(
        ctx.call("viewer", "POST", "/api/v1/plans", plan_body())
            .await
            .as_u16(),
        403
    );
    assert_eq!(
        ctx.call("operator", "POST", "/api/v1/plans", plan_body())
            .await
            .as_u16(),
        403
    );
    // The owner's plan doubles as the PUT fixture (unique names each call).
    let created = ctx
        .owner
        .post(ctx.srv.url("/api/v1/plans"))
        .json(&plan_body().unwrap())
        .send()
        .await
        .expect("create plan");
    assert_eq!(created.status(), StatusCode::CREATED);
    let created_body: Value = created.json().await.expect("plan body");
    let plan_id = created_body["id"].as_str().unwrap().to_string();

    ctx.row(
        "GET",
        &format!("/api/v1/plans/{plan_id}"),
        None,
        401,
        200,
        200,
        200,
    )
    .await;
    let put_body = || {
        Some(json!({
            "name": unique("mx-plan-upd"),
            "limits": {"memory_mb": 512, "cpu_percent": 100, "disk_mb": 2048},
        }))
    };
    assert_eq!(
        ctx.call(
            "anon",
            "PUT",
            &format!("/api/v1/plans/{plan_id}"),
            put_body()
        )
        .await
        .as_u16(),
        401
    );
    assert_eq!(
        ctx.call(
            "viewer",
            "PUT",
            &format!("/api/v1/plans/{plan_id}"),
            put_body()
        )
        .await
        .as_u16(),
        403
    );
    assert_eq!(
        ctx.call(
            "operator",
            "PUT",
            &format!("/api/v1/plans/{plan_id}"),
            put_body()
        )
        .await
        .as_u16(),
        403
    );
    assert_eq!(
        ctx.call(
            "owner",
            "PUT",
            &format!("/api/v1/plans/{plan_id}"),
            put_body()
        )
        .await
        .as_u16(),
        200
    );

    // Sites: reads viewer+, writes operator+. `POST /sites` uses a
    // well-formed body with an unknown app so auth failures (401/403)
    // stay distinct from the 400 the validation produces for
    // operator/owner; action routes use a bogus id (404 past auth).
    ctx.row("GET", "/api/v1/sites", None, 401, 200, 200, 200)
        .await;
    ctx.row(
        "POST",
        "/api/v1/sites",
        Some(json!({
            "name": unique("mx-site"),
            "app_id": "no-such-app",
            "version": "0.1.0",
            "plan_id": "plan-nope",
            "domain": format!("{}.example.com", unique("mx-site")),
            "owner_email": "owner@example.com",
            "prompts": {},
        })),
        401,
        403,
        400,
        400,
    )
    .await;
    ctx.row(
        "GET",
        "/api/v1/sites/does-not-exist",
        None,
        401,
        404,
        404,
        404,
    )
    .await;
    for action in ["restart", "stop", "start", "retry"] {
        ctx.row(
            "POST",
            &format!("/api/v1/sites/does-not-exist/{action}"),
            None,
            401,
            403,
            404,
            404,
        )
        .await;
    }
    ctx.row(
        "POST",
        "/api/v1/sites/does-not-exist/update",
        Some(json!({"version": "0.1.1"})),
        401,
        403,
        404,
        404,
    )
    .await;
    ctx.row(
        "DELETE",
        "/api/v1/sites/does-not-exist",
        None,
        401,
        403,
        404,
        404,
    )
    .await;
    for path in [
        "/api/v1/sites/does-not-exist/logs",
        "/api/v1/sites/does-not-exist/backups",
        "/api/v1/sites/does-not-exist/env",
        "/api/v1/jobs/does-not-exist",
        "/api/v1/jobs/does-not-exist/log",
    ] {
        ctx.row("GET", path, None, 401, 404, 404, 404).await;
    }
    ctx.row(
        "PUT",
        "/api/v1/sites/does-not-exist/env",
        Some(json!({"env": {"FOO": "bar"}})),
        401,
        403,
        404,
        404,
    )
    .await;
    ctx.row(
        "POST",
        "/api/v1/sites/does-not-exist/backups",
        None,
        401,
        403,
        404,
        404,
    )
    .await;
    ctx.row(
        "POST",
        "/api/v1/sites/does-not-exist/backups/nope/restore",
        None,
        401,
        403,
        404,
        404,
    )
    .await;
    ctx.row(
        "DELETE",
        "/api/v1/sites/does-not-exist/backups/nope",
        None,
        401,
        403,
        404,
        404,
    )
    .await;
    // Reveal is operator+ (viewers never read secrets).
    ctx.row(
        "POST",
        "/api/v1/sites/does-not-exist/env/FOO/reveal",
        Some(json!({"password": "correct-horse-1"})),
        401,
        403,
        404,
        404,
    )
    .await;

    // Jobs and audit: viewer+ reads.
    ctx.row("GET", "/api/v1/jobs", None, 401, 200, 200, 200)
        .await;
    ctx.row("GET", "/api/v1/audit", None, 401, 200, 200, 200)
        .await;

    // Logout last: it destroys each session (anon was never logged in).
    ctx.row("POST", "/api/v1/auth/logout", None, 401, 204, 204, 204)
        .await;
    ctx.row("GET", "/api/v1/auth/me", None, 401, 401, 401, 401)
        .await;
}
