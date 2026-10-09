//! The one `setu` binary (spec §8): `serve` is the control plane,
//! `agent …` the per-server agent and `helper …` its privileged writer
//! (both from the `setu-agent` library crate), plus operator management
//! and OpenAPI export.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use clap::{Parser, Subcommand};
use setu_api::session_store::PgSessionStore;
use setu_common::{Config, Sealer};
use setu_db::Db;
use setu_jobs::DbAgentClient;

#[derive(Parser)]
#[command(
    name = "setu",
    version,
    about = "Control plane and agent for single-binary web apps"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the control plane (REST API, job runner, embedded UI).
    Serve,
    /// Apply pending database migrations and exit.
    Migrate,
    /// Manage panel operators.
    Operator {
        #[command(subcommand)]
        cmd: OperatorCmd,
    },
    /// Write the OpenAPI document to `openapi.json` (default) or `--out`.
    Openapi {
        /// Destination file.
        #[arg(long, default_value = "openapi.json")]
        out: std::path::PathBuf,
    },
    /// Per-server agent (library crate `setu-agent`).
    Agent {
        #[command(subcommand)]
        cmd: setu_agent::cli::AgentCmd,
    },
    /// Privileged file writer (library crate `setu-agent`).
    Helper {
        #[command(subcommand)]
        cmd: setu_agent::cli::HelperCmd,
    },
}

#[derive(Subcommand)]
enum OperatorCmd {
    /// Create an operator (`--role` defaults to `owner`).
    Create {
        #[arg(long)]
        email: String,
        #[arg(long)]
        password: String,
        #[arg(long, default_value = "owner")]
        role: String,
    },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Serve => serve().await,
        Cmd::Migrate => migrate().await,
        Cmd::Operator { cmd } => match cmd {
            OperatorCmd::Create {
                email,
                password,
                role,
            } => operator_create(&email, &password, &role).await,
        },
        Cmd::Openapi { out } => openapi(&out).await,
        Cmd::Agent { cmd } => setu_agent::cli::run_agent(cmd)
            .await
            .map_err(|e| e.to_string()),
        Cmd::Helper { cmd } => setu_agent::cli::run_helper(cmd)
            .await
            .map_err(|e| e.to_string()),
    };
    if let Err(msg) = result {
        eprintln!("setu: {msg}");
        std::process::exit(1);
    }
}

fn load_config() -> Result<Config, String> {
    Config::load().map_err(|e| e.to_string())
}

async fn connect_and_migrate(config: &Config) -> Result<Db, String> {
    let db = Db::connect(&config.database_url)
        .await
        .map_err(|e| format!("cannot connect to database: {}", e.message))?;
    db.migrate()
        .await
        .map_err(|e| format!("migration failed: {}", e.message))?;
    Ok(db)
}

async fn serve() -> Result<(), String> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();
    let config = load_config()?;
    let db = connect_and_migrate(&config).await?;
    db.plans().seed_default().await.map_err(|e| e.message)?;
    let sealer = Sealer::new(&config.secret_key);
    let agent: Arc<dyn setu_jobs::AgentClient> =
        Arc::new(DbAgentClient::new(db.clone(), Duration::from_secs(120)));
    let state = setu_api::AppState::new(db.clone(), sealer.clone(), config.clone(), agent.clone());
    // Job runner with concurrency 4 (plan Task 9).
    let _runner = setu_jobs::Runner::spawn(db.clone(), agent, sealer, 4);
    // Periodic session-expiry cleanup.
    let store = PgSessionStore::new(db.pool().clone());
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(15 * 60));
        loop {
            timer.tick().await;
            if let Err(e) = store.delete_expired().await {
                tracing::error!(error = %e, "session cleanup failed");
            }
        }
    });
    let app = setu_api::rest::router(state);
    let addr: SocketAddr = config.bind_addr;
    tracing::info!("setu serve listening on {addr}");
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|e| format!("cannot bind {addr}: {e}"))?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .map_err(|e| format!("server error: {e}"))?;
    Ok(())
}

async fn migrate() -> Result<(), String> {
    let config = load_config()?;
    connect_and_migrate(&config).await?;
    println!("migrations applied");
    Ok(())
}

async fn operator_create(email: &str, password: &str, role: &str) -> Result<(), String> {
    if !matches!(role, "owner" | "operator" | "viewer") {
        return Err(format!("unknown role {role}"));
    }
    if password.len() < 8 {
        return Err("password must be at least 8 characters".to_string());
    }
    let config = load_config()?;
    let db = connect_and_migrate(&config).await?;
    let hash = setu_api::auth::hash_password(password).map_err(|e| e.message)?;
    let op = db
        .operators()
        .create(email, &hash, role)
        .await
        .map_err(|e| e.message)?;
    println!("created operator {} ({})", op.email, op.id);
    Ok(())
}

async fn openapi(out: &std::path::Path) -> Result<(), String> {
    let doc = setu_api::rest::docs::openapi_json_string();
    std::fs::write(out, doc).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}
