// Test double / test harness: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! [`TestServer`]: the whole control plane against a throwaway database.
//!
//! Ruling (Task 7): the plan names this `testkit::TestServer`, but building
//! the router requires `setu-api` while api integration tests already
//! depend on `testkit` — a `testkit → api` dependency would be a cycle.
//! `TestServer` therefore lives here as `setu_api::testing::TestServer`.
//!
//! [`TestServer::spawn`] takes the [`TestDb`] (kept alive inside the
//! server so the database outlives the test) and a [`FakeAgent`], runs
//! the `serve` boot steps (default plan, setup token in a private temp
//! `data_dir`), binds an ephemeral `127.0.0.1` port and serves the real
//! router — sessions, `Origin` guard, rate limiter included — with a
//! background job runner like `setu serve`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use setu_common::{Config, Listen, Sealer, SecretKey};
use setu_db::{Db, Operator};
use setu_jobs::testing::FakeAgent;
use setu_testkit::TestDb;

/// Fixed test sealing key (64 hex chars → 32 bytes). Never used outside
/// tests; production keys come from `SETU_SECRET_KEY`.
const TEST_SECRET_HEX: &str = "abababababababababababababababababababababababababababababababab";

/// Knobs for [`TestServer::spawn_with`].
#[derive(Debug, Clone, Default)]
pub struct TestOptions {
    /// `public_url` (default `http://<addr>`).
    pub public_url: Option<url::Url>,
    /// Per-server agent poll ceiling (default 600/min).
    pub agent_poll_limit: Option<usize>,
    /// `trusted_proxies` (default `127.0.0.1/32`).
    pub trusted_proxies: Option<Vec<ipnet::IpNet>>,
    /// The `listen` the config reports (the server itself always serves
    /// TCP; this only changes what handlers read, e.g. the local install
    /// command).
    pub config_listen: Option<Listen>,
    /// The manifest fetcher (default: the guarded HTTP fetch).
    pub fetcher: Option<Arc<dyn crate::follow::ManifestFetcher>>,
}

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
    /// The state the router serves (config, sealer, notifier, clock).
    pub state: crate::AppState,
    test_db: TestDb,
    data_dir: tempfile::TempDir,
    app: axum::Router,
    runner: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Ok(mut runner) = self.runner.lock() {
            if let Some(handle) = runner.take() {
                handle.abort();
            }
        }
    }
}

/// The runner configuration tests use: like `setu serve` (concurrency
/// 4) but with a fast idle poll, no `verify` pause and a 2 s lease, so a
/// killed runner's job is reclaimable within seconds.
pub fn test_runner_config() -> setu_jobs::RunnerConfig {
    setu_jobs::RunnerConfig {
        idle_poll: Duration::from_millis(100),
        verify_delay: Duration::ZERO,
        lease: Duration::from_secs(2),
        renew_every: Duration::from_millis(500),
        ..setu_jobs::RunnerConfig::default()
    }
}

impl TestServer {
    /// Boot the control plane on `test_db` with `agent` as its agent
    /// client. `public_url` is `http://<addr>`.
    pub async fn spawn(test_db: TestDb, agent: FakeAgent) -> Self {
        Self::spawn_with(test_db, agent, TestOptions::default()).await
    }

    /// [`TestServer::spawn`] with an explicit `public_url` (e.g. `https`
    /// to assert the `Secure` cookie flag; transport stays plain http).
    pub async fn spawn_with_public_url(
        test_db: TestDb,
        agent: FakeAgent,
        public_url: url::Url,
    ) -> Self {
        let opts = TestOptions {
            public_url: Some(public_url),
            ..TestOptions::default()
        };
        Self::spawn_with(test_db, agent, opts).await
    }

    /// [`TestServer::spawn`] with a tiny agent poll ceiling.
    pub async fn spawn_with_agent_poll_limit(
        test_db: TestDb,
        agent: FakeAgent,
        limit: usize,
    ) -> Self {
        let opts = TestOptions {
            agent_poll_limit: Some(limit),
            ..TestOptions::default()
        };
        Self::spawn_with(test_db, agent, opts).await
    }

    /// Boot with [`TestOptions`].
    pub async fn spawn_with(test_db: TestDb, agent: FakeAgent, opts: TestOptions) -> Self {
        let db = Db::new(test_db.pool.clone());
        db.plans().seed_default().await.expect("seed default plan");
        let data_dir = tempfile::tempdir().expect("test data dir");
        crate::rest::setup::ensure_setup_token(&db, data_dir.path())
            .await
            .expect("setup token");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");

        let secret = SecretKey::from_hex(TEST_SECRET_HEX).expect("test secret key parses");
        let public_url = opts
            .public_url
            .unwrap_or_else(|| format!("http://{addr}").parse().expect("test public url"));
        let config = Config {
            listen: opts.config_listen.unwrap_or(Listen::Tcp(addr)),
            database_url: test_db.url.clone(),
            secret_key: secret.clone(),
            public_url,
            trusted_proxies: opts
                .trusted_proxies
                .unwrap_or_else(|| vec!["127.0.0.1/32".parse().expect("loopback net")]),
            data_dir: data_dir.path().to_path_buf(),
        };
        let sealer = Sealer::new(&secret);
        let mut state = crate::AppState::new(
            db.clone(),
            sealer,
            config,
            Arc::new(setu_jobs::SystemClock),
            Arc::new(agent.clone()),
        );
        if let Some(limit) = opts.agent_poll_limit {
            state = state.with_agent_poll_limit(limit);
        }
        if let Some(fetcher) = opts.fetcher {
            state = state.with_fetcher(fetcher);
        }
        let app = crate::rest::router(state.clone());
        let serve_app = app.clone();
        tokio::spawn(async move {
            crate::listen::serve(crate::listen::Bound::Tcp(listener), serve_app)
                .await
                .expect("test server serves");
        });
        let server = Self {
            addr,
            client: new_client(),
            db,
            agent,
            state,
            test_db,
            data_dir,
            app,
            runner: Mutex::new(None),
        };
        // Background runner over the same `FakeAgent`, so enqueued jobs
        // complete on their own; tests poll `GET /jobs/{id}`.
        server.start_runner();
        // Scheduler slack only: the bind already succeeded.
        tokio::time::sleep(Duration::from_millis(50)).await;
        server
    }

    /// Start a job runner ([`test_runner_config`]) if none is running.
    pub fn start_runner(&self) {
        let mut slot = self.runner.lock().expect("runner slot");
        if slot.is_some() {
            return;
        }
        let agent: Arc<dyn setu_jobs::AgentClient> = Arc::new(self.agent.clone());
        *slot = Some(setu_jobs::Runner::spawn(
            self.db.clone(),
            agent,
            self.state.sealer.clone(),
            self.state.clock.clone(),
            test_runner_config(),
        ));
    }

    /// Abort the job runner (a control-plane crash: its leases run out).
    pub async fn kill_runner(&self) {
        let handle = self.runner.lock().expect("runner slot").take();
        if let Some(handle) = handle {
            handle.abort();
            let _ = handle.await;
        }
    }

    /// Also serve the same router on a unix socket at `path` (the way
    /// `setu serve` does with `listen = "unix:…"`).
    pub async fn serve_unix(&self, path: &Path) -> tokio::task::JoinHandle<()> {
        let bound = crate::listen::bind(&Listen::Unix(path.to_path_buf()))
            .await
            .expect("bind unix socket");
        let app = self.app.clone();
        tokio::spawn(async move {
            let _ = crate::listen::serve(bound, app).await;
        })
    }

    /// `http://<addr><path>`.
    pub fn url(&self, path: &str) -> String {
        format!("http://{}{}", self.addr, path)
    }

    /// The configured `public_url` without its trailing slash (the value
    /// a browser sends as `Origin`).
    pub fn origin(&self) -> String {
        self.state
            .config
            .public_url
            .as_str()
            .trim_end_matches('/')
            .to_string()
    }

    /// The throwaway database URL (for debugging; never logged by tests).
    pub fn db_url(&self) -> &str {
        &self.test_db.url
    }

    /// The control plane's `data_dir` (a private temp dir).
    pub fn data_dir(&self) -> PathBuf {
        self.data_dir.path().to_path_buf()
    }

    /// The setup token `serve` wrote at boot, if it is still there.
    pub fn setup_token(&self) -> Option<String> {
        std::fs::read_to_string(
            self.data_dir
                .path()
                .join(crate::rest::setup::SETUP_TOKEN_FILE),
        )
        .ok()
        .map(|s| s.trim().to_string())
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
            setu_common::new_id()[18..].to_lowercase()
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

/// A `reqwest` client with a cookie store (holds the session cookie)
/// that behaves like the panel's own pages: every request carries
/// `Sec-Fetch-Site: same-origin`, as a browser sets it, so mutations pass
/// the `Origin` guard. A request can override the header.
pub fn new_client() -> reqwest::Client {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        "sec-fetch-site",
        reqwest::header::HeaderValue::from_static("same-origin"),
    );
    reqwest::Client::builder()
        .cookie_store(true)
        .default_headers(headers)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("test client builds")
}

/// A client with a cookie store and no browser headers at all.
pub fn bare_client() -> reqwest::Client {
    reqwest::Client::builder()
        .cookie_store(true)
        .timeout(Duration::from_secs(10))
        .build()
        .expect("test client builds")
}
