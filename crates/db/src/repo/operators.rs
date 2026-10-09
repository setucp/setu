//! `operators` repo: panel users (password hashing lives in the api;
//! this repo stores the given hash opaquely). Emails are stored
//! lowercase.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// 64-bit advisory-lock key for first-owner setup: first 8 bytes of
/// `sha256("setu-setup")` as big-endian i64. Serialises the check+insert
/// in `create_first_owner` across concurrently booting control planes.
const SETUP_LOCK_KEY: i64 = -8640242326146119732;

/// One panel user. Holds credential material: `Debug` redacts it.
#[derive(Clone)]
pub struct Operator {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

impl std::fmt::Debug for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operator")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("password_hash", &"[redacted]")
            .field("role", &self.role)
            .field("created_at", &self.created_at)
            .finish()
    }
}

/// Repository accessor; build via [`crate::Db::operators`].
pub struct Operators<'a> {
    pool: &'a PgPool,
}

impl<'a> Operators<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        email: &str,
        password_hash: &str,
        role: &str,
    ) -> Result<Operator, AppError> {
        sqlx::query_as!(
            Operator,
            "INSERT INTO operators (id, email, password_hash, role)
             VALUES ($1, $2, $3, $4)
             RETURNING id, email, password_hash, role, created_at",
            setu_common::new_id(),
            email.to_lowercase(),
            password_hash,
            role,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Create the first operator atomically with the one-time setup
    /// token (spec §7): under a transaction-scoped advisory lock, an
    /// existing operator is 409 `conflict` (whatever the token), a token
    /// whose SHA-256 hex differs from `settings.setup_token_hash` (or no
    /// stored hash) is 401 `unauthorized`; on success the owner is
    /// inserted and the stored hash deleted in the same transaction, so
    /// two concurrent `POST /setup` calls cannot both succeed.
    pub async fn create_first_owner(
        &self,
        email: &str,
        password_hash: &str,
        setup_token_hash: &str,
    ) -> Result<Operator, AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        sqlx::query!("SELECT pg_advisory_xact_lock($1)", SETUP_LOCK_KEY)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        let n: Option<i64> = sqlx::query_scalar!("SELECT COUNT(*) FROM operators")
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        if n.unwrap_or(0) != 0 {
            return Err(AppError::conflict("setup already complete"));
        }
        let stored: Option<serde_json::Value> = sqlx::query_scalar!(
            "SELECT value FROM settings WHERE key = $1",
            crate::repo::settings::SETUP_TOKEN_HASH,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let matches = stored.as_ref().and_then(|v| v.as_str()).is_some_and(|h| {
            crate::repo::constant_time_eq(h.as_bytes(), setup_token_hash.as_bytes())
        });
        if !matches {
            return Err(AppError::unauthorized("invalid setup token"));
        }
        let op = sqlx::query_as!(
            Operator,
            "INSERT INTO operators (id, email, password_hash, role)
             VALUES ($1, $2, $3, 'owner')
             RETURNING id, email, password_hash, role, created_at",
            setu_common::new_id(),
            email.to_lowercase(),
            password_hash,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        sqlx::query!(
            "DELETE FROM settings WHERE key = $1",
            crate::repo::settings::SETUP_TOKEN_HASH,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(op)
    }

    pub async fn by_email(&self, email: &str) -> Result<Option<Operator>, AppError> {
        sqlx::query_as!(
            Operator,
            "SELECT id, email, password_hash, role, created_at
             FROM operators WHERE email = $1",
            email.to_lowercase(),
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn by_id(&self, id: &str) -> Result<Option<Operator>, AppError> {
        sqlx::query_as!(
            Operator,
            "SELECT id, email, password_hash, role, created_at
             FROM operators WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn count(&self) -> Result<i64, AppError> {
        let n: Option<i64> = sqlx::query_scalar!("SELECT COUNT(*) FROM operators")
            .fetch_one(self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(n.unwrap_or(0))
    }

    pub async fn list(&self) -> Result<Vec<Operator>, AppError> {
        sqlx::query_as!(
            Operator,
            "SELECT id, email, password_hash, role, created_at
             FROM operators ORDER BY email, id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Replace the password hash and delete every session of the
    /// operator, in one transaction (spec §3: all sessions die on a
    /// password change).
    pub async fn set_password_hash(&self, id: &str, password_hash: &str) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let n = sqlx::query!(
            "UPDATE operators SET password_hash = $2 WHERE id = $1",
            id,
            password_hash,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            return Err(AppError::not_found("operator not found"));
        }
        sqlx::query!("DELETE FROM sessions WHERE operator_id = $1", id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(())
    }
}
