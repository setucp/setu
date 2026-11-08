//! `site_basic_auth` repo (0.1.7): the HTTP basic-auth user name and
//! bcrypt hash of a site. The password is never stored.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

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

    /// Sets (or replaces) the site's credentials.
    pub async fn set(
        &self,
        site_id: &str,
        username: &str,
        password_hash: &str,
        now: DateTime<Utc>,
    ) -> Result<SiteBasicAuth, AppError> {
        sqlx::query_as!(
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
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Removes the site's credentials; `false` when it had none.
    pub async fn clear(&self, site_id: &str) -> Result<bool, AppError> {
        let done = sqlx::query!("DELETE FROM site_basic_auth WHERE site_id = $1", site_id)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(done.rows_affected() > 0)
    }
}
