//! `site_options` repo (0.1.7): per-site options kept apart from `sites`.
//! Runtime-checked queries, so the offline `.sqlx` data does not change.

use sqlx::PgPool;

use crate::repo::basic_auth::ROUTE_JOB;
use crate::repo::jobs::{self, Job};
use crate::{map_db_error, AppError};

pub struct Options<'a> {
    pool: &'a PgPool,
}

impl<'a> Options<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// True when the site asks search engines to stay away (false when it
    /// has no row).
    pub async fn noindex(&self, site_id: &str) -> Result<bool, AppError> {
        let v: Option<bool> =
            sqlx::query_scalar("SELECT noindex FROM site_options WHERE site_id = $1")
                .bind(site_id)
                .fetch_optional(self.pool)
                .await
                .map_err(map_db_error)?;
        Ok(v.unwrap_or(false))
    }

    /// Stores the option and queues the [`ROUTE_JOB`] that applies it, in
    /// one transaction.
    pub async fn set_noindex_with_route_job(
        &self,
        site_id: &str,
        server_id: &str,
        enabled: bool,
    ) -> Result<Job, AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        sqlx::query(
            "INSERT INTO site_options (site_id, noindex) VALUES ($1, $2) \
             ON CONFLICT (site_id) DO UPDATE SET noindex = EXCLUDED.noindex",
        )
        .bind(site_id)
        .bind(enabled)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let job = jobs::insert(&mut *tx, ROUTE_JOB, Some(site_id), Some(server_id), None).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(job)
    }
}
