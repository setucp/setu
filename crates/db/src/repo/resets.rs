//! `site_resets` repo (0.1.4 §2): one schedule per site that restores a
//! chosen backup every `every_minutes`.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// A site's reset schedule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteReset {
    pub site_id: String,
    pub backup_id: String,
    pub every_minutes: i32,
    pub last_reset_at: DateTime<Utc>,
}

impl SiteReset {
    /// When the next reset is due.
    #[must_use]
    pub fn next_at(&self) -> DateTime<Utc> {
        self.last_reset_at + chrono::Duration::minutes(i64::from(self.every_minutes))
    }
}

pub struct Resets<'a> {
    pool: &'a PgPool,
}

impl<'a> Resets<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// The site's schedule, if it has one.
    pub async fn get(&self, site_id: &str) -> Result<Option<SiteReset>, AppError> {
        sqlx::query_as!(
            SiteReset,
            "SELECT site_id, backup_id, every_minutes, last_reset_at FROM site_resets
             WHERE site_id = $1",
            site_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Every schedule (the scheduler filters what is due).
    pub async fn list(&self) -> Result<Vec<SiteReset>, AppError> {
        sqlx::query_as!(
            SiteReset,
            "SELECT site_id, backup_id, every_minutes, last_reset_at FROM site_resets
             ORDER BY site_id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Sets (or replaces) the site's schedule; the first reset comes one
    /// interval after `now`.
    pub async fn set(
        &self,
        site_id: &str,
        backup_id: &str,
        every_minutes: i32,
        now: DateTime<Utc>,
    ) -> Result<SiteReset, AppError> {
        sqlx::query_as!(
            SiteReset,
            "INSERT INTO site_resets (site_id, backup_id, every_minutes, last_reset_at)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (site_id) DO UPDATE
                SET backup_id = EXCLUDED.backup_id,
                    every_minutes = EXCLUDED.every_minutes,
                    last_reset_at = EXCLUDED.last_reset_at
             RETURNING site_id, backup_id, every_minutes, last_reset_at",
            site_id,
            backup_id,
            every_minutes,
            now,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Removes the site's schedule; `false` when it had none.
    pub async fn clear(&self, site_id: &str) -> Result<bool, AppError> {
        let done = sqlx::query!("DELETE FROM site_resets WHERE site_id = $1", site_id)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(done.rows_affected() > 0)
    }

    /// Records that a reset was enqueued at `now`.
    pub async fn touch(&self, site_id: &str, now: DateTime<Utc>) -> Result<(), AppError> {
        sqlx::query!(
            "UPDATE site_resets SET last_reset_at = $2 WHERE site_id = $1",
            site_id,
            now,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(())
    }

    /// Whether `backup_id` is any site's reset point.
    pub async fn is_reset_point(&self, backup_id: &str) -> Result<bool, AppError> {
        sqlx::query_scalar!(
            "SELECT EXISTS (SELECT 1 FROM site_resets WHERE backup_id = $1) AS \"exists!\"",
            backup_id,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }
}
