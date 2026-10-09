//! The one `setu` binary (spec §8): `serve` is the control plane,
//! `agent …` the per-server agent and `helper …` its privileged writer
//! (both from the `setu-agent` library crate), plus operator management,
//! the control plane's own database backup and OpenAPI export.

use std::sync::Arc;

use clap::{Parser, Subcommand};
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
    /// Dump the control plane's database (`pg_dump -Fc`) to `--out`.
    Backup {
        /// Destination file.
        #[arg(long)]
        out: std::path::PathBuf,
    },
    /// Write a control-plane setting (`setting set tls_internal true`).
    Setting {
        #[command(subcommand)]
        cmd: SettingCmd,
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
    /// Privileged helper, run only through `sudo -n` by the agent:
    /// `setu helper <verb> --site <short>` (library crate `setu-agent`).
    Helper(setu_agent::helper::cli::HelperArgs),
}

#[derive(Subcommand)]
enum OperatorCmd {
    /// Create an operator (`--role` defaults to `owner`). The password
    /// comes from `SETU_OPERATOR_PASSWORD`, else the first line of stdin.
    Create {
        #[arg(long)]
        email: String,
        #[arg(long, default_value = "owner")]
        role: String,
    },
}

#[derive(Subcommand)]
enum SettingCmd {
    /// Set one known key (`tls_internal`: `true` | `false`).
    Set { key: String, value: String },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let result = match cli.cmd {
        Cmd::Serve => serve().await,
        Cmd::Migrate => migrate().await,
        Cmd::Operator { cmd } => match cmd {
            OperatorCmd::Create { email, role } => operator_create(&email, &role).await,
        },
        Cmd::Backup { out } => backup(&out),
        Cmd::Setting {
            cmd: SettingCmd::Set { key, value },
        } => setting_set(&key, &value).await,
        Cmd::Openapi { out } => openapi(&out).await,
        Cmd::Agent { cmd } => {
            // The agent's own log (journald captures stderr); stdout
            // stays clean for `doctor`'s table and install output.
            init_tracing(true);
            setu_agent::cli::run_agent(cmd)
                .await
                .map_err(|e| e.to_string())
        }
        Cmd::Helper(args) => std::process::exit(setu_agent::helper::cli::main(args)),
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

/// `RUST_LOG` or `info`; ANSI colours only on a terminal (journald
/// stores the raw bytes). `to_stderr` for the agent roles.
fn init_tracing(to_stderr: bool) {
    use std::io::IsTerminal as _;
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt().with_env_filter(filter);
    if to_stderr {
        builder
            .with_ansi(std::io::stderr().is_terminal())
            .with_writer(std::io::stderr)
            .init();
    } else {
        builder.with_ansi(std::io::stdout().is_terminal()).init();
    }
}

async fn serve() -> Result<(), String> {
    init_tracing(false);
    let config = load_config()?;
    let db = connect_and_migrate(&config).await?;
    db.plans().seed_default().await.map_err(|e| e.message)?;
    // First boot (zero operators): the one-time setup token (spec §8).
    setu_api::rest::setup::ensure_setup_token(&db, &config.data_dir)
        .await
        .map_err(|e| format!("setup token: {}", e.message))?;
    let sealer = Sealer::new(&config.secret_key);
    let agent: Arc<dyn setu_jobs::AgentClient> = Arc::new(DbAgentClient::new(db.clone()));
    let clock: Arc<dyn setu_jobs::Clock> = Arc::new(setu_jobs::SystemClock);
    let state = setu_api::AppState::new(
        db.clone(),
        sealer.clone(),
        config.clone(),
        clock.clone(),
        agent.clone(),
    );
    // Job runner (leases, renewal, two installs per server) and the
    // scheduler (nightly backups, retention, metric/command/session
    // pruning), both spec §6 defaults, on the state's clock.
    let _runner = setu_jobs::Runner::spawn(
        db.clone(),
        agent,
        sealer,
        clock.clone(),
        setu_jobs::RunnerConfig::default(),
    );
    let _scheduler =
        setu_jobs::Scheduler::spawn(db.clone(), clock, setu_jobs::SchedulerConfig::default());
    let app = setu_api::rest::router(state);
    let bound = setu_api::listen::bind(&config.listen)
        .await
        .map_err(|e| format!("cannot listen on {}: {e}", config.listen))?;
    tracing::info!("setu serve listening on {}", bound.describe());
    setu_api::listen::serve(bound, app)
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

async fn operator_create(email: &str, role: &str) -> Result<(), String> {
    if !matches!(role, "owner" | "operator" | "viewer") {
        return Err(format!("unknown role {role}"));
    }
    let password = setu_api::cli::read_operator_password(
        std::env::var(setu_api::cli::OPERATOR_PASSWORD_ENV).ok(),
        &mut std::io::stdin().lock(),
    )?;
    if password.len() < 8 {
        return Err("password must be at least 8 characters".to_string());
    }
    let config = load_config()?;
    let db = connect_and_migrate(&config).await?;
    let hash = setu_api::auth::hash_password(&password).map_err(|e| e.message)?;
    let op = db
        .operators()
        .create(email, &hash, role)
        .await
        .map_err(|e| e.message)?;
    println!("created operator {} ({})", op.email, op.id);
    Ok(())
}

async fn setting_set(key: &str, value: &str) -> Result<(), String> {
    let (key, value) = setu_api::cli::parse_setting(key, value)?;
    let config = load_config()?;
    let db = connect_and_migrate(&config).await?;
    db.settings()
        .set(key, &value)
        .await
        .map_err(|e| e.message)?;
    println!("set {key} = {value}");
    Ok(())
}

fn backup(out: &std::path::Path) -> Result<(), String> {
    let config = load_config()?;
    setu_api::cli::backup(&setu_api::cli::ProcessRunner, &config.database_url, out)?;
    println!("wrote {}", out.display());
    Ok(())
}

async fn openapi(out: &std::path::Path) -> Result<(), String> {
    let doc = setu_api::rest::docs::openapi_json_string();
    std::fs::write(out, doc).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    println!("wrote {}", out.display());
    Ok(())
}
