//! `settings` repo: key/value JSON (setup token hash, trusted proxies,
//! backup defaults).

use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Repository accessor; build via [`crate::Db::settings`].
pub struct Settings<'a> {
    pool: &'a PgPool,
}

impl<'a> Settings<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn get(&self, key: &str) -> Result<Option<Value>, AppError> {
        sqlx::query_scalar!("SELECT value FROM settings WHERE key = $1", key)
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)
    }

    pub async fn set(&self, key: &str, value: &Value) -> Result<(), AppError> {
        sqlx::query!(
            "INSERT INTO settings (key, value) VALUES ($1, $2)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value",
            key,
            value,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(())
    }
}
