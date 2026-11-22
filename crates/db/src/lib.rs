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
    AgentCommand, Allocation, App, AppSource, AppSources, AppVersion, Apps, Audit, AuditEntry,
    Backup, Backups, BasicAuths, Commands, CreateError, HeartbeatInput, HeartbeatOutcome, Job,
    Jobs, JoinError, JoinInput, LatestMetric, Metrics, NewPlan, NewSite, Operator, Operators, Plan,
    Plans, Resets, Server, ServerStatus, Servers, Session, Sessions, Settings, Site, SiteBasicAuth,
    SiteReset, Sites, WaitError, NOTIFY_CHANNEL, ROUTE_JOB, SELF_BACKUP, SETUP_TOKEN_HASH,
    TLS_INTERNAL,
};
pub use setu_common::{AppError, ErrorCode};

use sqlx::postgres::PgPoolOptions;
use sqlx::PgPool;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Advisory-lock key for migrations (plan Task 5: `0x5e7a`); the lock
/// serialises `migrate()` across concurrently booting control planes.
const MIGRATE_LOCK_KEY: i64 = 0x5e7a;

/// How long one acquire waits for a connection before it fails (the
/// sqlx default of 30 s hid a wedged pool for four days, #16).
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(10);
/// A health probe that takes longer than this counts as a failure.
const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the last probes saw (`/readyz` reports it).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DbHealth {
    /// Probes failed in a row; 0 when the last one passed.
    pub consecutive_failures: u32,
    /// The last probe's error, while `consecutive_failures > 0`.
    pub last_error: Option<String>,
}

/// Control-plane database handle.
#[derive(Debug, Clone)]
pub struct Db {
    pool: PgPool,
    health: Arc<Mutex<DbHealth>>,
}

impl Db {
    /// Wrap an existing pool (tests build the pool via `setu-testkit`).
    pub fn new(pool: PgPool) -> Self {
        Self {
            pool,
            health: Arc::default(),
        }
    }

    /// Connect with a modest pool (control plane + job runner share it).
    /// Idle connections are recycled and every acquire checks its
    /// connection and gives up after [`ACQUIRE_TIMEOUT`], so a Postgres
    /// restart cannot leave dead connections in the pool.
    pub async fn connect(url: &str) -> Result<Self, AppError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(ACQUIRE_TIMEOUT)
            .idle_timeout(Duration::from_mins(5))
            .max_lifetime(Duration::from_mins(30))
            .test_before_acquire(true)
            .connect(url)
            .await?;
        Ok(Self::new(pool))
    }

    /// The last probe result.
    pub fn health(&self) -> DbHealth {
        self.health
            .lock()
            .map_or_else(|e| e.into_inner().clone(), |h| h.clone())
    }

    /// `SELECT 1` through the pool, at most [`PROBE_TIMEOUT`]; the result
    /// is kept for [`health`](Self::health). The error names the cause.
    pub async fn probe(&self) -> Result<(), String> {
        let result = match tokio::time::timeout(
            PROBE_TIMEOUT,
            sqlx::query("SELECT 1::int").fetch_one(&self.pool),
        )
        .await
        {
            Ok(Ok(_)) => Ok(()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(_) => Err(format!("no answer within {} s", PROBE_TIMEOUT.as_secs())),
        };
        let mut h = self
            .health
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match &result {
            Ok(()) => *h = DbHealth::default(),
            Err(e) => {
                h.consecutive_failures = h.consecutive_failures.saturating_add(1);
                h.last_error = Some(e.clone());
            }
        }
        result
    }

    /// Probe every `every`. After `limit` failures in a row, check with
    /// a fresh connection (outside the pool): if Postgres answers there
    /// while the pool does not, the pool is wedged and this returns so
    /// the caller can restart the process, which is the only repair the
    /// 0.1.1 test bed ever got (#16). While Postgres itself is down it
    /// keeps probing, so the pool recovers by itself when it comes back.
    ///
    /// The count is the watchdog's own: `/readyz` probes too, and its calls
    /// must not shorten the time before a restart. A probe is
    /// [`PROBE_TIMEOUT`] (shorter than [`ACQUIRE_TIMEOUT`]), so a pool that
    /// stays saturated for `limit` probes while Postgres answers is
    /// restarted as well; that is intended, a leaked pool is repaired the
    /// same way.
    pub async fn watch(&self, url: &str, every: Duration, limit: u32) {
        let mut failures = 0u32;
        loop {
            tokio::time::sleep(every).await;
            let Err(error) = self.probe().await else {
                failures = 0;
                continue;
            };
            failures = failures.saturating_add(1);
            tracing::error!(failures, error = %error, "control-plane database unreachable");
            if failures >= limit && fresh_connection_works(url).await {
                tracing::error!(
                    "postgres answers on a new connection but the pool does not: the pool is wedged"
                );
                return;
            }
        }
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

    pub fn app_sources(&self) -> AppSources<'_> {
        AppSources::new(&self.pool)
    }

    pub fn basic_auths(&self) -> BasicAuths<'_> {
        BasicAuths::new(&self.pool)
    }

    pub fn resets(&self) -> Resets<'_> {
        Resets::new(&self.pool)
    }

    pub fn audit(&self) -> Audit<'_> {
        Audit::new(&self.pool)
    }

    pub fn metrics(&self) -> Metrics<'_> {
        Metrics::new(&self.pool)
    }
}

/// One `SELECT 1` on a connection opened outside the pool.
async fn fresh_connection_works(url: &str) -> bool {
    use sqlx::{Connection as _, PgConnection};
    let attempt = async {
        let mut conn = PgConnection::connect(url).await?;
        sqlx::query("SELECT 1::int").fetch_one(&mut conn).await?;
        Ok::<_, sqlx::Error>(())
    };
    matches!(
        tokio::time::timeout(PROBE_TIMEOUT, attempt).await,
        Ok(Ok(()))
    )
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
