//! `app_sources` repo (0.1.5 §1): the manifest URL an app follows and the
//! outcome of its last check.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// The URL an app follows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppSource {
    pub app_id: String,
    pub url: String,
    pub checked_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
}

pub struct AppSources<'a> {
    pool: &'a PgPool,
}

impl<'a> AppSources<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// The app's source, if it follows one.
    pub async fn get(&self, app_id: &str) -> Result<Option<AppSource>, AppError> {
        sqlx::query_as!(
            AppSource,
            "SELECT app_id, url, checked_at, last_error FROM app_sources WHERE app_id = $1",
            app_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Every source.
    pub async fn list(&self) -> Result<Vec<AppSource>, AppError> {
        sqlx::query_as!(
            AppSource,
            "SELECT app_id, url, checked_at, last_error FROM app_sources ORDER BY app_id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Sets (or replaces) the app's URL; a new URL starts unchecked.
    pub async fn set(&self, app_id: &str, url: &str) -> Result<AppSource, AppError> {
        sqlx::query_as!(
            AppSource,
            "INSERT INTO app_sources (app_id, url) VALUES ($1, $2)
             ON CONFLICT (app_id) DO UPDATE
                SET url = EXCLUDED.url,
                    checked_at = CASE WHEN app_sources.url = EXCLUDED.url
                                      THEN app_sources.checked_at END,
                    last_error = CASE WHEN app_sources.url = EXCLUDED.url
                                      THEN app_sources.last_error END
             RETURNING app_id, url, checked_at, last_error",
            app_id,
            url,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Records a finished check: `error` is `None` on success.
    pub async fn record_check(
        &self,
        app_id: &str,
        now: DateTime<Utc>,
        error: Option<&str>,
    ) -> Result<(), AppError> {
        sqlx::query!(
            "UPDATE app_sources SET checked_at = $2, last_error = $3 WHERE app_id = $1",
            app_id,
            now,
            error,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(())
    }

    /// Stops following; `false` when the app followed nothing.
    pub async fn clear(&self, app_id: &str) -> Result<bool, AppError> {
        let done = sqlx::query!("DELETE FROM app_sources WHERE app_id = $1", app_id)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(done.rows_affected() > 0)
    }
}
