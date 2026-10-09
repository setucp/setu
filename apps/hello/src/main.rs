//! `hello`: the tiny Axum app the end-to-end test provisions (spec §9,
//! plan Task 11).
//!
//! - Listens on `HELLO_BIND` (`127.0.0.1:<port>`, the manifest's
//!   `from = "bind_addr"`); without it on `127.0.0.1:$PORT` (default 3000).
//! - `GET /` answers `hello from <HELLO_NAME> v<HELLO_VERSION>`.
//! - `GET /healthz`: 500 when `HELLO_FAIL=1`, else 200 after a `SELECT 1`
//!   on `DATABASE_URL` (200 without a database configured).
//! - `GET /notes` → JSON array of note texts (oldest first);
//!   `POST /notes {"text": …}` inserts one and, when `HELLO_UPLOADS_DIR`
//!   is set, writes `note-<id>.txt` there so a restore of the site's dirs
//!   is observable; `GET /uploads` lists that directory.
//! - Boot migrations by `HELLO_VERSION` ([`plan_for`]): every version
//!   creates `notes`; `0.1.1` creates `notes_v2` and **refuses to boot**
//!   when `notes_v3` exists (a rollback that did not restore the database
//!   is visibly broken); `0.1.2` and later create `notes_v2` and
//!   `notes_v3`, then serve.
//!
//! Error bodies are static strings: database errors can echo hostnames,
//! and env values must never reach logs or responses.

use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{extract::State, http::StatusCode, response::IntoResponse, routing::get, Json, Router};
use sqlx::postgres::{PgPool, PgPoolOptions};

/// Runtime knobs, read once at boot.
#[derive(Debug, Clone)]
struct AppConfig {
    name: String,
    version: String,
    fail: bool,
    uploads: Option<PathBuf>,
}

impl AppConfig {
    fn load() -> Self {
        let var = |k: &str| std::env::var(k).ok().filter(|s| !s.trim().is_empty());
        Self {
            name: var("HELLO_NAME").unwrap_or_else(|| "hello".to_string()),
            version: var("HELLO_VERSION").unwrap_or_else(|| "0.1.0".to_string()),
            fail: var("HELLO_FAIL").as_deref() == Some("1"),
            uploads: var("HELLO_UPLOADS_DIR").map(PathBuf::from),
        }
    }
}

#[derive(Clone)]
struct AppState {
    cfg: AppConfig,
    db: Option<PgPool>,
}

/// Greeting body for `GET /`.
fn greeting(name: &str, version: &str) -> String {
    format!("hello from {name} v{version}")
}

/// `HELLO_BIND` when it parses, else `127.0.0.1:$PORT` (default 3000).
fn listen_addr(bind: Option<&str>, port: Option<&str>) -> SocketAddr {
    if let Some(addr) = bind.and_then(|b| b.parse().ok()) {
        return addr;
    }
    let port = port.and_then(|p| p.parse().ok()).unwrap_or(3000);
    SocketAddr::from(([127, 0, 0, 1], port))
}

/// What one version does to the schema at boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct MigrationPlan {
    create_v2: bool,
    create_v3: bool,
    /// Exit non-zero when `notes_v3` exists (0.1.1 cannot read it).
    refuse_if_v3: bool,
}

/// `major.minor.patch` (pre-release/build suffixes ignored); anything
/// unparsable counts as 0.0.0 (no migrations).
fn parse_version(v: &str) -> (u64, u64, u64) {
    let core = v.split(['-', '+']).next().unwrap_or("");
    let mut parts = core.split('.').map(|p| p.parse::<u64>().ok());
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Some(a)), Some(Some(b)), Some(Some(c))) => (a, b, c),
        _ => (0, 0, 0),
    }
}

/// The boot migrations of `version`.
fn plan_for(version: &str) -> MigrationPlan {
    let v = parse_version(version);
    MigrationPlan {
        create_v2: v >= (0, 1, 1),
        create_v3: v >= (0, 1, 2),
        refuse_if_v3: v >= (0, 1, 1) && v < (0, 1, 2),
    }
}

/// The boot gate: `Err` (a message for stderr) when this version must
/// not run on a schema that already has `notes_v3`.
fn gate(plan: MigrationPlan, version: &str, v3_exists: bool) -> Result<(), String> {
    if plan.refuse_if_v3 && v3_exists {
        return Err(format!(
            "hello v{version}: table notes_v3 exists (created by a newer version); \
             refusing to boot on a schema this version does not understand"
        ));
    }
    Ok(())
}

/// Run the boot migrations; `Err` means "do not serve".
async fn migrate(db: &PgPool, version: &str) -> Result<(), String> {
    let plan = plan_for(version);
    let sql_err = |what: &str| format!("hello v{version}: migration failed ({what})");
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS notes (id BIGSERIAL PRIMARY KEY, text TEXT NOT NULL, \
         created_at TIMESTAMPTZ NOT NULL DEFAULT now())",
    )
    .execute(db)
    .await
    .map_err(|_| sql_err("notes"))?;
    let v3_exists: bool = sqlx::query_scalar("SELECT to_regclass('public.notes_v3') IS NOT NULL")
        .fetch_one(db)
        .await
        .map_err(|_| sql_err("inspect"))?;
    gate(plan, version, v3_exists)?;
    if plan.create_v2 {
        sqlx::query("CREATE TABLE IF NOT EXISTS notes_v2 (id BIGSERIAL PRIMARY KEY, body TEXT)")
            .execute(db)
            .await
            .map_err(|_| sql_err("notes_v2"))?;
    }
    if plan.create_v3 {
        sqlx::query("CREATE TABLE IF NOT EXISTS notes_v3 (id BIGSERIAL PRIMARY KEY, body TEXT)")
            .execute(db)
            .await
            .map_err(|_| sql_err("notes_v3"))?;
    }
    Ok(())
}

async fn index(State(st): State<AppState>) -> String {
    greeting(&st.cfg.name, &st.cfg.version)
}

async fn healthz(State(st): State<AppState>) -> impl IntoResponse {
    if st.cfg.fail {
        return (StatusCode::INTERNAL_SERVER_ERROR, "failing (HELLO_FAIL=1)").into_response();
    }
    match &st.db {
        None => (StatusCode::OK, "ok (no db configured)").into_response(),
        Some(db) => match sqlx::query("SELECT 1").execute(db).await {
            Ok(_) => (StatusCode::OK, "ok").into_response(),
            Err(_) => (StatusCode::INTERNAL_SERVER_ERROR, "db unreachable").into_response(),
        },
    }
}

async fn list_notes(State(st): State<AppState>) -> Result<Json<Vec<String>>, StatusCode> {
    let db = st.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let rows: Vec<String> = sqlx::query_scalar("SELECT text FROM notes ORDER BY id")
        .fetch_all(db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    Ok(Json(rows))
}

#[derive(serde::Deserialize)]
struct NoteIn {
    text: String,
}

async fn add_note(
    State(st): State<AppState>,
    Json(note): Json<NoteIn>,
) -> Result<(StatusCode, Json<serde_json::Value>), StatusCode> {
    let db = st.db.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let id: i64 = sqlx::query_scalar("INSERT INTO notes (text) VALUES ($1) RETURNING id")
        .bind(&note.text)
        .fetch_one(db)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if let Some(dir) = &st.cfg.uploads {
        tokio::fs::write(dir.join(upload_name(id)), note.text.as_bytes())
            .await
            .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    }
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "id": id, "text": note.text })),
    ))
}

fn upload_name(id: i64) -> String {
    format!("note-{id}.txt")
}

async fn list_uploads(State(st): State<AppState>) -> Result<Json<Vec<String>>, StatusCode> {
    let dir = st.cfg.uploads.as_ref().ok_or(StatusCode::NOT_FOUND)?;
    let mut names = Vec::new();
    let mut rd = tokio::fs::read_dir(dir)
        .await
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    while let Ok(Some(entry)) = rd.next_entry().await {
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(Json(names))
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/healthz", get(healthz))
        .route("/notes", get(list_notes).post(add_note))
        .route("/uploads", get(list_uploads))
        .with_state(state)
}

#[tokio::main]
async fn main() {
    let cfg = AppConfig::load();
    let db = match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => {
            // Lazy: a database that is briefly down fails health, not boot.
            let pool = PgPoolOptions::new()
                .max_connections(4)
                .connect_lazy(&url)
                .unwrap_or_else(|_| {
                    eprintln!("hello: DATABASE_URL does not parse");
                    std::process::exit(2);
                });
            if let Err(msg) = migrate(&pool, &cfg.version).await {
                eprintln!("{msg}");
                std::process::exit(1);
            }
            Some(pool)
        }
        _ => None,
    };
    let addr = listen_addr(
        std::env::var("HELLO_BIND").ok().as_deref(),
        std::env::var("PORT").ok().as_deref(),
    );
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .unwrap_or_else(|e| {
            eprintln!("hello: cannot bind {addr}: {e}");
            std::process::exit(1);
        });
    eprintln!("hello v{} listening on {addr}", cfg.version);
    axum::serve(listener, router(AppState { cfg, db }))
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
            "127.0.0.1:4000".parse::<SocketAddr>().unwrap()
        );
        assert_eq!(listen_addr(Some("nope"), None).port(), 3000);
    }

    #[test]
    fn greeting_names_site_and_version() {
        assert_eq!(greeting("hello", "0.1.0"), "hello from hello v0.1.0");
        assert_eq!(greeting("e2e", "0.1.1"), "hello from e2e v0.1.1");
    }

    #[test]
    fn version_0_1_0_migrates_nothing_extra() {
        let p = plan_for("0.1.0");
        assert!(!p.create_v2 && !p.create_v3 && !p.refuse_if_v3);
        assert_eq!(plan_for("garbage"), p);
    }

    #[test]
    fn version_0_1_1_creates_v2_and_refuses_on_v3() {
        let p = plan_for("0.1.1");
        assert!(p.create_v2 && !p.create_v3 && p.refuse_if_v3);
        assert!(gate(p, "0.1.1", false).is_ok());
        let err = gate(p, "0.1.1", true).expect_err("must refuse");
        assert!(err.contains("notes_v3"), "{err}");
    }

    #[test]
    fn version_0_1_2_creates_v2_and_v3_and_boots_on_v3() {
        let p = plan_for("0.1.2");
        assert!(p.create_v2 && p.create_v3 && !p.refuse_if_v3);
        assert!(gate(p, "0.1.2", true).is_ok());
        assert_eq!(plan_for("0.2.0"), p);
        assert_eq!(plan_for("0.1.2-rc1"), p);
    }

    #[test]
    fn version_0_1_0_ignores_v3() {
        assert!(gate(plan_for("0.1.0"), "0.1.0", true).is_ok());
    }

    async fn spawn(cfg: AppConfig) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router(AppState { cfg, db: None })).await;
        });
        (format!("http://127.0.0.1:{port}"), server)
    }

    fn cfg(name: &str, fail: bool) -> AppConfig {
        AppConfig {
            name: name.to_string(),
            version: "0.1.0".to_string(),
            fail,
            uploads: None,
        }
    }

    #[tokio::test]
    async fn root_and_healthz_serve_without_db() {
        let (base, server) = spawn(cfg("test", false)).await;
        let root = reqwest::get(format!("{base}/")).await.expect("GET /");
        assert_eq!(root.status(), StatusCode::OK);
        assert_eq!(root.text().await.expect("body"), "hello from test v0.1.0");
        let health = reqwest::get(format!("{base}/healthz"))
            .await
            .expect("GET /healthz");
        assert_eq!(health.status(), StatusCode::OK);
        server.abort();
    }

    #[tokio::test]
    async fn fail_flag_makes_healthz_500() {
        let (base, server) = spawn(cfg("broken", true)).await;
        let root = reqwest::get(format!("{base}/")).await.expect("GET /");
        assert_eq!(root.text().await.expect("body"), "hello from broken v0.1.0");
        let health = reqwest::get(format!("{base}/healthz"))
            .await
            .expect("GET /healthz");
        assert_eq!(health.status(), StatusCode::INTERNAL_SERVER_ERROR);
        server.abort();
    }

    #[tokio::test]
    async fn uploads_lists_the_directory() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(upload_name(2)), "b").expect("write");
        std::fs::write(dir.path().join(upload_name(1)), "a").expect("write");
        let mut c = cfg("u", false);
        c.uploads = Some(dir.path().to_path_buf());
        let (base, server) = spawn(c).await;
        let names: Vec<String> = reqwest::get(format!("{base}/uploads"))
            .await
            .expect("GET /uploads")
            .json()
            .await
            .expect("json");
        assert_eq!(names, vec!["note-1.txt", "note-2.txt"]);
        server.abort();
    }
}
