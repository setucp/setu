//! `hello`: the tiny Axum app the end-to-end test provisions (spec §9).
//!
//! - `GET /` answers `hello from <HELLO_NAME>` (`hello` when unset).
//! - `GET /healthz` runs a Postgres `SELECT 1` on `DATABASE_URL` (skipped
//!   with 200 when `DATABASE_URL` is unset); `HELLO_FAIL=1` forces a 500
//!   so the e2e can exercise update rollback.
//! - Listens on `HELLO_BIND` (`127.0.0.1:<port>`, from the manifest's
//!   `from = "bind_addr"`); without it, on `0.0.0.0:$PORT` (default 3000).
//!
//! Error bodies are static strings: connection errors can echo hostnames,
//! and env values must never reach logs or responses.

use std::net::SocketAddr;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Router};

/// Runtime knobs, read once at boot (handlers only re-read
/// `DATABASE_URL` per health check so tests stay deterministic).
#[derive(Debug, Clone)]
struct AppConfig {
    name: String,
    fail: bool,
}

impl AppConfig {
    fn load() -> Self {
        Self {
            name: std::env::var("HELLO_NAME")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "hello".to_string()),
            fail: std::env::var("HELLO_FAIL").as_deref() == Ok("1"),
        }
    }
}

/// Greeting body for `GET /` (pure, unit-tested).
fn greeting(name: &str) -> String {
    format!("hello from {name}")
}

/// `HELLO_BIND` when it parses, else `0.0.0.0:$PORT` (pure, unit-tested).
fn listen_addr(bind: Option<&str>, port: Option<&str>) -> SocketAddr {
    if let Some(addr) = bind.and_then(|b| b.parse().ok()) {
        return addr;
    }
    let port = port.and_then(|p| p.parse().ok()).unwrap_or(3000);
    SocketAddr::from(([0, 0, 0, 0], port))
}

async fn index(State(cfg): State<AppConfig>) -> String {
    greeting(&cfg.name)
}

async fn healthz(State(cfg): State<AppConfig>) -> impl IntoResponse {
    if cfg.fail {
        return (StatusCode::INTERNAL_SERVER_ERROR, "failing (HELLO_FAIL=1)").into_response();
    }
    match std::env::var("DATABASE_URL") {
        Err(_) => (StatusCode::OK, "ok (no db configured)").into_response(),
        Ok(url) => match db_ping(&url).await {
            Ok(()) => (StatusCode::OK, "ok").into_response(),
            // Static body: sqlx errors may name the host, and must never
            // leak credentials or env values.
            Err(()) => (StatusCode::INTERNAL_SERVER_ERROR, "db unreachable").into_response(),
        },
    }
}

/// One `SELECT 1` over a fresh connection; no pool (one check per poll).
async fn db_ping(url: &str) -> Result<(), ()> {
    use sqlx::Connection;
    let mut conn = sqlx::postgres::PgConnection::connect(url)
        .await
        .map_err(|_| ())?;
    sqlx::query("SELECT 1")
        .execute(&mut conn)
        .await
        .map_err(|_| ())?;
    Ok(())
}

fn router(cfg: AppConfig) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .with_state(cfg)
}

#[tokio::main]
async fn main() {
    let cfg = AppConfig::load();
    let addr = listen_addr(
        std::env::var("HELLO_BIND").ok().as_deref(),
        std::env::var("PORT").ok().as_deref(),
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("bind listen port");
    axum::serve(listener, router(cfg))
        .await
        .expect("serve hello");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_addr_prefers_hello_bind() {
        assert_eq!(
            listen_addr(Some("127.0.0.1:20001"), Some("9")),
            "127.0.0.1:20001".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(
            listen_addr(None, Some("4000")),
            "0.0.0.0:4000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(listen_addr(Some("nope"), None).port(), 3000);
    }

    #[test]
    fn greeting_names_the_site() {
        assert_eq!(greeting("hello"), "hello from hello");
        assert_eq!(greeting("hello-1"), "hello from hello-1");
    }

    /// Spin the real router on an ephemeral port: `/` greets and
    /// `/healthz` is 200 when `DATABASE_URL` is unset (check skipped).
    /// Only this module's tests read `DATABASE_URL`, so mutating it here
    /// cannot race anything else.
    #[tokio::test]
    async fn root_and_healthz_serve_without_db() {
        std::env::remove_var("DATABASE_URL");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(AppConfig {
                    name: "test".to_string(),
                    fail: false,
                }),
            )
            .await
        });
        let root = reqwest::get(format!("{base}/")).await.expect("GET /");
        assert_eq!(root.status(), StatusCode::OK);
        assert_eq!(root.text().await.expect("body"), "hello from test");
        let health = reqwest::get(format!("{base}/healthz"))
            .await
            .expect("GET /healthz");
        assert_eq!(health.status(), StatusCode::OK);
        server.abort();
    }

    #[tokio::test]
    async fn fail_flag_makes_healthz_500() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let base = format!("http://127.0.0.1:{port}");
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                router(AppConfig {
                    name: "broken".to_string(),
                    fail: true,
                }),
            )
            .await
        });
        // `/` still serves (only health fails: rollback, not crash).
        let root = reqwest::get(format!("{base}/")).await.expect("GET /");
        assert_eq!(root.text().await.expect("body"), "hello from broken");
        let health = reqwest::get(format!("{base}/healthz"))
            .await
            .expect("GET /healthz");
        assert_eq!(health.status(), StatusCode::INTERNAL_SERVER_ERROR);
        server.abort();
    }
}
