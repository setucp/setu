//! setu-db: control-plane Postgres schema and repositories.
//!
//! Schema lives in `migrations/` (forward-only sqlx files, applied under
//! a Postgres advisory lock). Repositories hang off [`Db`]:
//! `db.servers().create_pending(..)`, `db.sites().next_port(..)`, …
//! Unique violations map to [`AppError`] `conflict`, missing rows to
//! `not_found`; anything else is `internal` (logged, never leaked).

mod repo;

pub use repo::{
    AgentCommand, AgentCommands, App, AppVersion, Apps, Audit, AuditEntry, Backup, Backups, Job,
    Jobs, Metrics, NewSite, Operator, Operators, Plan, Plans, Server, Servers, Site, Sites,
};
pub use setu_common::{AppError, ErrorCode};

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;

/// 64-bit advisory-lock key for migrations: first 8 bytes of
/// `sha256("setu-migrate")` as big-endian i64 (`-4436319754632771127`).
/// A fixed 64-bit key avoids colliding with other lock users; the lock
/// serialises `migrate()` across concurrently booting control planes.
const MIGRATE_LOCK_KEY: i64 = -4436319754632771127;

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
        let result = sqlx::migrate!("./migrations").run(&self.pool).await;
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

    pub fn agent_commands(&self) -> AgentCommands<'_> {
        AgentCommands::new(&self.pool)
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
                    Some("operator_tokens_token_hash_key") => "token already exists",
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
