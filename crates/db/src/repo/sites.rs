//! `sites` repo: provisioned app instances and port allocation.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Lifecycle states; anything else is rejected with `validation`.
pub const SITE_STATUSES: &[&str] = &[
    "provisioning",
    "running",
    "stopped",
    "failed",
    "updating",
    "deleting",
];

/// One site. `env` holds sealed secrets: `Debug` redacts it.
pub struct Site {
    pub id: String,
    pub name: String,
    pub app_id: String,
    pub app_version: String,
    pub server_id: String,
    pub plan_id: String,
    pub domain: String,
    pub status: String,
    pub failed_step: Option<String>,
    pub port: i32,
    pub data_dir: String,
    pub db_name: String,
    pub env: Value,
    pub owner_email: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Site")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("app_id", &self.app_id)
            .field("app_version", &self.app_version)
            .field("server_id", &self.server_id)
            .field("plan_id", &self.plan_id)
            .field("domain", &self.domain)
            .field("status", &self.status)
            .field("failed_step", &self.failed_step)
            .field("port", &self.port)
            .field("env", &"[redacted]")
            .field("owner_email", &self.owner_email)
            .finish()
    }
}

impl Clone for Site {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            name: self.name.clone(),
            app_id: self.app_id.clone(),
            app_version: self.app_version.clone(),
            server_id: self.server_id.clone(),
            plan_id: self.plan_id.clone(),
            domain: self.domain.clone(),
            status: self.status.clone(),
            failed_step: self.failed_step.clone(),
            port: self.port,
            data_dir: self.data_dir.clone(),
            db_name: self.db_name.clone(),
            env: self.env.clone(),
            owner_email: self.owner_email.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}

/// Parameters for a new site row (status starts `provisioning`).
/// `env` must already be sealed with the panel `Sealer`.
pub struct NewSite {
    pub name: String,
    pub app_id: String,
    pub app_version: String,
    pub server_id: String,
    pub plan_id: String,
    pub domain: String,
    pub port: i32,
    pub data_dir: String,
    pub db_name: String,
    pub env: Value,
    pub owner_email: String,
}

impl std::fmt::Debug for NewSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewSite")
            .field("name", &self.name)
            .field("app_id", &self.app_id)
            .field("app_version", &self.app_version)
            .field("server_id", &self.server_id)
            .field("plan_id", &self.plan_id)
            .field("domain", &self.domain)
            .field("port", &self.port)
            .field("env", &"[redacted]")
            .field("owner_email", &self.owner_email)
            .finish()
    }
}

/// Repository accessor; build via [`crate::Db::sites`].
pub struct Sites<'a> {
    pool: &'a PgPool,
}

impl<'a> Sites<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(&self, site: &NewSite) -> Result<Site, AppError> {
        sqlx::query_as!(
            SiteRow,
            "INSERT INTO sites
                 (id, name, app_id, app_version, server_id, plan_id, domain,
                  status, port, data_dir, db_name, env, owner_email)
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'provisioning', $8, $9, $10, $11, $12)
             RETURNING id, name, app_id, app_version, server_id, plan_id, domain,
                       status, failed_step, port, data_dir, db_name, env, owner_email,
                       created_at, updated_at",
            setu_common::new_id(),
            site.name,
            site.app_id,
            site.app_version,
            site.server_id,
            site.plan_id,
            site.domain,
            site.port,
            site.data_dir,
            site.db_name,
            site.env,
            site.owner_email,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_site())
    }

    pub async fn get(&self, id: &str) -> Result<Site, AppError> {
        sqlx::query_as!(
            SiteRow,
            "SELECT id, name, app_id, app_version, server_id, plan_id, domain,
                    status, failed_step, port, data_dir, db_name, env, owner_email,
                    created_at, updated_at
             FROM sites WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_site())
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    pub async fn by_domain(&self, domain: &str) -> Result<Option<Site>, AppError> {
        sqlx::query_as!(
            SiteRow,
            "SELECT id, name, app_id, app_version, server_id, plan_id, domain,
                    status, failed_step, port, data_dir, db_name, env, owner_email,
                    created_at, updated_at
             FROM sites WHERE domain = $1",
            domain,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.map(|r| r.into_site()))
    }

    pub async fn list(&self) -> Result<Vec<Site>, AppError> {
        sqlx::query_as!(
            SiteRow,
            "SELECT id, name, app_id, app_version, server_id, plan_id, domain,
                    status, failed_step, port, data_dir, db_name, env, owner_email,
                    created_at, updated_at
             FROM sites ORDER BY created_at DESC, id DESC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_site()).collect())
    }

    pub async fn set_status(
        &self,
        id: &str,
        status: &str,
        failed_step: Option<&str>,
    ) -> Result<Site, AppError> {
        if !SITE_STATUSES.contains(&status) {
            return Err(AppError::validation(format!(
                "unknown site status {status}"
            )));
        }
        sqlx::query_as!(
            SiteRow,
            "UPDATE sites SET status = $2, failed_step = $3, updated_at = now()
             WHERE id = $1
             RETURNING id, name, app_id, app_version, server_id, plan_id, domain,
                       status, failed_step, port, data_dir, db_name, env, owner_email,
                       created_at, updated_at",
            id,
            status,
            failed_step,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_site())
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    pub async fn set_version(&self, id: &str, version: &str) -> Result<Site, AppError> {
        sqlx::query_as!(
            SiteRow,
            "UPDATE sites SET app_version = $2, updated_at = now()
             WHERE id = $1
             RETURNING id, name, app_id, app_version, server_id, plan_id, domain,
                       status, failed_step, port, data_dir, db_name, env, owner_email,
                       created_at, updated_at",
            id,
            version,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_site())
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    pub async fn set_env(&self, id: &str, env: &Value) -> Result<Site, AppError> {
        sqlx::query_as!(
            SiteRow,
            "UPDATE sites SET env = $2, updated_at = now()
             WHERE id = $1
             RETURNING id, name, app_id, app_version, server_id, plan_id, domain,
                       status, failed_step, port, data_dir, db_name, env, owner_email,
                       created_at, updated_at",
            id,
            env,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_site())
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    /// Lowest free port for the server in 20000–29999. Runs in a
    /// transaction that locks the server row (`FOR UPDATE`), so
    /// concurrent provisioners serialise on the pick; the
    /// `UNIQUE(server_id, port)` constraint is the backstop — a loser
    /// retries with a fresh pick and duplicate ports never commit.
    pub async fn next_port(&self, server_id: &str) -> Result<i32, AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let locked: Option<String> =
            sqlx::query_scalar!("SELECT id FROM servers WHERE id = $1 FOR UPDATE", server_id,)
                .fetch_optional(&mut *tx)
                .await
                .map_err(map_db_error)?;
        if locked.is_none() {
            return Err(AppError::not_found("server not found"));
        }
        let used: Vec<i32> =
            sqlx::query_scalar!("SELECT port FROM sites WHERE server_id = $1", server_id,)
                .fetch_all(&mut *tx)
                .await
                .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        (20000..30000)
            .find(|p| !used.contains(p))
            .ok_or_else(|| AppError::conflict("no free ports on server"))
    }

    /// Delete the row; jobs, backups and metrics cascade.
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        let n = sqlx::query!("DELETE FROM sites WHERE id = $1", id)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?
            .rows_affected();
        if n == 0 {
            return Err(AppError::not_found("site not found"));
        }
        Ok(())
    }
}

#[derive(sqlx::FromRow)]
struct SiteRow {
    id: String,
    name: String,
    app_id: String,
    app_version: String,
    server_id: String,
    plan_id: String,
    domain: String,
    status: String,
    failed_step: Option<String>,
    port: i32,
    data_dir: String,
    db_name: String,
    env: Value,
    owner_email: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl SiteRow {
    fn into_site(self) -> Site {
        Site {
            id: self.id,
            name: self.name,
            app_id: self.app_id,
            app_version: self.app_version,
            server_id: self.server_id,
            plan_id: self.plan_id,
            domain: self.domain,
            status: self.status,
            failed_step: self.failed_step,
            port: self.port,
            data_dir: self.data_dir,
            db_name: self.db_name,
            env: self.env,
            owner_email: self.owner_email,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
