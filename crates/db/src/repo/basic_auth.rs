//! `site_basic_auth` repo (0.1.7): the HTTP basic-auth user name and
//! bcrypt hash of a site. The password is never stored.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::repo::jobs::{self, Job};
use crate::{map_db_error, AppError};

/// The job that pushes a site's credentials to its Caddy route.
pub const ROUTE_JOB: &str = "route_update";

/// A site's basic-auth credentials (the hash, not the password).
#[derive(Clone, PartialEq, Eq)]
pub struct SiteBasicAuth {
    pub site_id: String,
    pub username: String,
    pub password_hash: String,
    pub updated_at: DateTime<Utc>,
}

// `Debug` never prints the hash.
impl std::fmt::Debug for SiteBasicAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SiteBasicAuth")
            .field("site_id", &self.site_id)
            .field("username", &self.username)
            .field("password_hash", &"[redacted]")
            .field("updated_at", &self.updated_at)
            .finish()
    }
}

pub struct BasicAuths<'a> {
    pool: &'a PgPool,
}

impl<'a> BasicAuths<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// The site's credentials, if it has any.
    pub async fn get(&self, site_id: &str) -> Result<Option<SiteBasicAuth>, AppError> {
        sqlx::query_as!(
            SiteBasicAuth,
            "SELECT site_id, username, password_hash, updated_at FROM site_basic_auth
             WHERE site_id = $1",
            site_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Sets (or replaces) the site's credentials and queues the
    /// [`ROUTE_JOB`] that applies them, in one transaction: when the job
    /// cannot be queued (`conflict`: the site has an active job) the
    /// credentials stay as they were.
    pub async fn set_with_route_job(
        &self,
        site_id: &str,
        server_id: &str,
        username: &str,
        password_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<(SiteBasicAuth, Job), AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let row = sqlx::query_as!(
            SiteBasicAuth,
            "INSERT INTO site_basic_auth (site_id, username, password_hash, updated_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (site_id) DO UPDATE
                SET username = EXCLUDED.username,
                    password_hash = EXCLUDED.password_hash,
                    updated_at = EXCLUDED.updated_at
             RETURNING site_id, username, password_hash, updated_at",
            site_id,
            username,
            password_hash,
            now,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let job = jobs::insert(&mut *tx, ROUTE_JOB, Some(site_id), Some(server_id), None).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok((row, job))
    }

    /// Removes the site's credentials and queues the [`ROUTE_JOB`] that
    /// opens the route, in one transaction (as in
    /// [`set_with_route_job`](Self::set_with_route_job)). `None` when the
    /// site had no credentials: nothing is removed or queued.
    pub async fn clear_with_route_job(
        &self,
        site_id: &str,
        server_id: &str,
    ) -> Result<Option<Job>, AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let done = sqlx::query!("DELETE FROM site_basic_auth WHERE site_id = $1", site_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        if done.rows_affected() == 0 {
            return Ok(None);
        }
        let job = jobs::insert(&mut *tx, ROUTE_JOB, Some(site_id), Some(server_id), None).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(Some(job))
    }
}
