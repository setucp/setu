//! setu-db: control-plane Postgres schema and repositories.
//!
//! Schema lives in `migrations/` (forward-only sqlx files, applied under
//! a Postgres advisory lock). Repositories hang off [`Db`]:
//! `db.servers().create_pending(..)`, `db.sites().create(..)`, …
//! Unique violations map to [`AppError`] `conflict`, missing rows to
//! `not_found`; anything else is `internal` (logged, never leaked).

mod repo;

/// The embedded migrations (`crates/db/migrations`).
pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

pub use repo::{
    AgentCommand, Allocation, App, AppVersion, Apps, Audit, AuditEntry, Backup, Backups, Commands,
    CreateError, HeartbeatInput, HeartbeatOutcome, Job, Jobs, JoinError, JoinInput, LatestMetric,
    Metrics, NewPlan, NewSite, Operator, Operators, Plan, Plans, Server, ServerStatus, Servers,
    Session, Sessions, Settings, Site, Sites, WaitError, NOTIFY_CHANNEL, SETUP_TOKEN_HASH,
    TLS_INTERNAL,
};
pub use setu_common::{AppError, ErrorCode};

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// Advisory-lock key for migrations (plan Task 5: `0x5e7a`); the lock
/// serialises `migrate()` across concurrently booting control planes.
const MIGRATE_LOCK_KEY: i64 = 0x5e7a;

/// Control-plane database handle.
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Wrap an existing pool (tests build the pool via `setu-testkit`).
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Connect with a modest pool (control plane + job runner share it).
    pub async fn connect(url: &str) -> Result<Self, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    /// Run pending migrations under a session advisory lock so two
    /// booting replicas never migrate concurrently.
    pub async fn migrate(&self) -> Result<(), AppError> {
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&self.pool)
            .await?;
        let result = MIGRATOR.run(&self.pool).await;
        // Best-effort unlock: the migration result below is authoritative.
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&self.pool)
            .await
            .ok();
        result.map_err(|e| {
            tracing::error!(error = %e, "database migration failed");
            AppError::internal()
        })?;
        Ok(())
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub fn operators(&self) -> Operators<'_> {
        Operators::new(&self.pool)
    }

    pub fn servers(&self) -> Servers<'_> {
        Servers::new(&self.pool)
    }

    pub fn apps(&self) -> Apps<'_> {
        Apps::new(&self.pool)
    }

    pub fn plans(&self) -> Plans<'_> {
        Plans::new(&self.pool)
    }

    pub fn sites(&self) -> Sites<'_> {
        Sites::new(&self.pool)
    }

    pub fn jobs(&self) -> Jobs<'_> {
        Jobs::new(&self.pool)
    }

    pub fn commands(&self) -> Commands<'_> {
        Commands::new(&self.pool)
    }

    pub fn sessions(&self) -> Sessions<'_> {
        Sessions::new(&self.pool)
    }

    pub fn settings(&self) -> Settings<'_> {
        Settings::new(&self.pool)
    }

    pub fn backups(&self) -> Backups<'_> {
        Backups::new(&self.pool)
    }

    pub fn audit(&self) -> Audit<'_> {
        Audit::new(&self.pool)
    }

    pub fn metrics(&self) -> Metrics<'_> {
        Metrics::new(&self.pool)
    }
}

/// Map a sqlx error to the API envelope: unique violations become
/// `conflict` (with a friendly message per constraint), foreign-key
/// violations become `validation`, everything else is `internal`.
pub(crate) fn map_db_error(e: sqlx::Error) -> AppError {
    if let sqlx::Error::Database(db) = &e {
        match db.code().as_deref() {
            Some("23505") => {
                return AppError::conflict(match db.constraint() {
                    Some("sites_domain_key") => "domain already in use",
                    Some("sites_server_id_port_key") => "port already in use on this server",
                    Some("app_versions_app_id_version_key") => "app version already exists",
                    Some("operators_email_key") => "email already in use",
                    Some("plans_name_key") => "plan name already in use",
                    Some("sites_server_id_short_id_key") => "short id already in use",
                    Some("jobs_one_active_per_site_idx") => "site already has an active job",
                    _ => "already exists",
                });
            }
            Some("23503") => {
                return AppError::validation("referenced row does not exist");
            }
            _ => {}
        }
    }
    e.into()
}
