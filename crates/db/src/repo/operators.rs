//! `operators` repo: panel users (password hashing is Task 7; this
//! repo stores the given hash opaquely).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// One panel user. Holds credential material: `Debug` redacts it.
pub struct Operator {
    pub id: String,
    pub email: String,
    pub password_hash: String,
    pub totp_secret: Option<String>,
    pub role: String,
    pub created_at: DateTime<Utc>,
}

impl std::fmt::Debug for Operator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Operator")
            .field("id", &self.id)
            .field("email", &self.email)
            .field("password_hash", &"[redacted]")
            .field(
                "totp_secret",
                &self.totp_secret.as_ref().map(|_| "[redacted]"),
            )
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
        let id = setu_common::new_id();
        sqlx::query_as!(
            OperatorRow,
            "INSERT INTO operators (id, email, password_hash, role)
             VALUES ($1, $2, $3, $4)
             RETURNING id, email, password_hash, totp_secret, role, created_at",
            id,
            email,
            password_hash,
            role,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_operator())
    }

    pub async fn by_email(&self, email: &str) -> Result<Option<Operator>, AppError> {
        sqlx::query_as!(
            OperatorRow,
            "SELECT id, email, password_hash, totp_secret, role, created_at
             FROM operators WHERE email = $1",
            email,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.map(|r| r.into_operator()))
    }

    pub async fn by_id(&self, id: &str) -> Result<Option<Operator>, AppError> {
        sqlx::query_as!(
            OperatorRow,
            "SELECT id, email, password_hash, totp_secret, role, created_at
             FROM operators WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.map(|r| r.into_operator()))
    }

    pub async fn count(&self) -> Result<i64, AppError> {
        let n: Option<i64> = sqlx::query_scalar!("SELECT COUNT(*) FROM operators")
            .fetch_one(self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(n.unwrap_or(0))
    }
}

#[derive(sqlx::FromRow)]
struct OperatorRow {
    id: String,
    email: String,
    password_hash: String,
    totp_secret: Option<String>,
    role: String,
    created_at: DateTime<Utc>,
}

impl OperatorRow {
    fn into_operator(self) -> Operator {
        Operator {
            id: self.id,
            email: self.email,
            password_hash: self.password_hash,
            totp_secret: self.totp_secret,
            role: self.role,
            created_at: self.created_at,
        }
    }
}
