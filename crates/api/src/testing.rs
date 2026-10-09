//! [`TestServer`]: the whole control plane against a throwaway database.
//!
//! Ruling (Task 7): the plan names this `testkit::TestServer`, but building
//! the router requires `setu-api` while api integration tests already
//! depend on `testkit` — a `testkit → api` dependency would be a cycle
//! (`api` dev-depends on `testkit`). `TestServer` therefore lives here as
//! `setu_api::testing::TestServer`; Task 8/9 tests use this path.
//! Cost if wrong: moving it later means touching every api test import.
//!
//! [`TestServer::spawn`] takes the [`TestDb`] (kept alive inside the
//! server so the database outlives the test) and a [`FakeAgent`], binds an
//! ephemeral `127.0.0.1` port, and serves the real router — sessions,
//! CSRF guard, rate limiter included.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use setu_common::{Config, Sealer, SecretKey};
use setu_db::{Db, Operator};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

/// Fixed test sealing key (64 hex chars → 32 bytes). Never used outside
/// tests; production keys come from `SETU_SECRET_KEY`.
const TEST_SECRET_HEX: &str = "abababababababababababababababababababababababababababababababab";

/// A running control plane for integration tests.
pub struct TestServer {
    /// Where the server listens (ephemeral `127.0.0.1` port).
    pub addr: SocketAddr,
    /// Default client with a cookie store (unauthenticated until login).
    pub client: reqwest::Client,
    /// Database handle on the throwaway database.
    pub db: Db,
    /// The scripted agent the server's state machines call.
    pub agent: FakeAgent,
    test_db: TestDb,
}

impl TestServer {
    /// Boot the control plane on `test_db` with `agent` as its agent
    /// client. The server's `SETU_PUBLIC_URL` is `http://<addr>`, so the
    /// install-command test can assert on it.
    pub async fn spawn(test_db: TestDb, agent: FakeAgent) -> Self {
        Self::boot(test_db, agent, None, None).await
    }

    /// Boot like [`TestServer::spawn`] but with an explicit `public_url`
    /// (e.g. an `https` URL to assert the `Secure` cookie flag follows the
    /// scheme; transport stays plain http).
    pub async fn spawn_with_public_url(
        test_db: TestDb,
        agent: FakeAgent,
        public_url: url::Url,
    ) -> Self {
        Self::boot(test_db, agent, Some(public_url), None).await
    }

    /// Boot like [`TestServer::spawn`] but with a tiny agent poll ceiling
    /// so the 429 test needs only a handful of requests.
    pub async fn spawn_with_agent_poll_limit(
        test_db: TestDb,
        agent: FakeAgent,
        limit: usize,
    ) -> Self {
        Self::boot(test_db, agent, None, Some(limit)).await
    }

    async fn boot(
        test_db: TestDb,
        agent: FakeAgent,
        public_url: Option<url::Url>,
        agent_poll_limit: Option<usize>,
    ) -> Self {
        let pool = test_db.pool.clone();
        let db = Db::new(pool.clone());
        // The seeded Default plan exists for convenience (site tests later).
        db.plans().seed_default().await.expect("seed default plan");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");

        let secret = SecretKey::from_hex(TEST_SECRET_HEX).expect("test secret key parses");
        let public_url = public_url
            .unwrap_or_else(|| format!("http://{addr}").parse().expect("test public url"));
        let config = Config {
            bind_addr: addr,
            database_url: test_db.url.clone(),
            secret_key: secret.clone(),
            public_url,
            data_dir: std::env::temp_dir().join(format!("setu-test-{}", addr.port())),
        };
        let mut state = crate::AppState::new(
            db.clone(),
            Sealer::new(&secret),
            config,
            Arc::new(agent.clone()),
        );
        if let Some(limit) = agent_poll_limit {
            state = state.with_agent_poll_limit(limit);
        }
        let app = crate::rest::router(state);
        tokio::spawn(async move {
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .expect("test server serves");
        });
        // Give the listener a moment to accept (bind already succeeded, so
        // this is only scheduler slack).
        tokio::time::sleep(Duration::from_millis(50)).await;
        Self {
            addr,
            client: new_client(),
            db,
            agent,
            test_db,
        }
    }

    /// `http://<addr><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// The throwaway database URL (for debugging; never logged by tests).
    pub fn db_url(&self) -> &str {
        &self.test_db.url
    }

    /// Create an operator directly in the repo (argon2-hashed password).
    /// Emails are namespaced per call so one server serves many tests.
    pub async fn create_operator(
        &self,
        email_prefix: &str,
        password: &str,
        role: &str,
    ) -> Operator {
        let email = format!(
            "{email_prefix}-{}@example.com",
            setu_common::new_id()[..8].to_lowercase()
        );
        let hash = crate::auth::hash_password(password).expect("hash test password");
        self.db
            .operators()
            .create(&email, &hash, role)
            .await
            .expect("create test operator")
    }

    /// Log `client` in as `email`/`password`; returns the raw response.
    pub async fn login(
        &self,
        client: &reqwest::Client,
        email: &str,
        password: &str,
    ) -> reqwest::Response {
        client
            .post(self.url("/api/v1/auth/login"))
            .json(&serde_json::json!({"email": email, "password": password}))
            .send()
            .await
            .expect("login request sends")
    }

    /// Fresh client logged in as a newly created owner.
    pub async fn login_as_owner(&self) -> reqwest::Client {
        self.login_as("owner").await
    }

    /// Fresh client logged in as a newly created `role`
    /// (`owner` | `operator` | `viewer`).
    pub async fn login_as(&self, role: &str) -> reqwest::Client {
        let op = self.create_operator(role, "correct-horse-1", role).await;
        let client = new_client();
        let resp = self.login(&client, &op.email, "correct-horse-1").await;
        assert_eq!(
            resp.status(),
            reqwest::StatusCode::OK,
            "login_as({role}) must succeed"
        );
        client
    }
}

/// A `reqwest` client with a cookie store (holds the session cookie).
pub fn new_client() -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("test client builds")
}
