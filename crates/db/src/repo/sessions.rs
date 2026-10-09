//! `sessions` repo: web sessions (spec §3). The cookie value is 32
//! random bytes, base64url without padding; only the SHA-256 (hex) of
//! those bytes is stored. Expiry slides to 7 days, refreshed on load
//! once the last refresh is more than an hour old.

use base64::Engine as _;
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::repo::operators::Operator;
use crate::{map_db_error, AppError};

/// Session lifetime (sliding).
pub const SESSION_TTL_SECS: f64 = 7.0 * 24.0 * 3600.0;
/// A load refreshes `expires_at` when it is older than this.
pub const SESSION_REFRESH_SECS: f64 = 3600.0;

/// One session row. `id_hash` is a hash, not the cookie, but it still
/// identifies a live session: `Debug` redacts it.
#[derive(Clone)]
pub struct Session {
    pub id_hash: String,
    pub operator_id: String,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id_hash", &"[redacted]")
            .field("operator_id", &self.operator_id)
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Repository accessor; build via [`crate::Db::sessions`].
pub struct Sessions<'a> {
    pool: &'a PgPool,
}

/// Hash of a cookie value, or `None` when it is not 32 base64url bytes.
fn cookie_hash(cookie: &str) -> Option<String> {
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(cookie.as_bytes())
        .ok()?;
    (bytes.len() == 32).then(|| crate::repo::sha256_hex(&bytes))
}

impl<'a> Sessions<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Start a session; returns the cookie value (the only time it
    /// exists in plaintext).
    pub async fn create(&self, operator_id: &str) -> Result<String, AppError> {
        let bytes = crate::repo::random_32();
        let cookie = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
        sqlx::query!(
            "INSERT INTO sessions (id_hash, operator_id, expires_at)
             VALUES ($1, $2, now() + make_interval(secs => $3))",
            crate::repo::sha256_hex(&bytes),
            operator_id,
            SESSION_TTL_SECS,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(cookie)
    }

    /// The live session and its operator, sliding the expiry when the
    /// last refresh is more than an hour old. Unknown, malformed or
    /// expired cookies are `None`.
    pub async fn load(&self, cookie: &str) -> Result<Option<(Session, Operator)>, AppError> {
        let Some(id_hash) = cookie_hash(cookie) else {
            return Ok(None);
        };
        // Refresh in place when due (expires_at earlier than now + TTL -
        // 1 h means the last refresh is over an hour old).
        sqlx::query!(
            "UPDATE sessions SET expires_at = now() + make_interval(secs => $2)
             WHERE id_hash = $1
               AND expires_at > now()
               AND expires_at < now() + make_interval(secs => $3)",
            id_hash,
            SESSION_TTL_SECS,
            SESSION_TTL_SECS - SESSION_REFRESH_SECS,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        let row = sqlx::query!(
            "SELECT s.id_hash, s.operator_id, s.created_at, s.expires_at,
                    o.email, o.password_hash, o.role, o.created_at AS op_created_at
             FROM sessions s JOIN operators o ON o.id = s.operator_id
             WHERE s.id_hash = $1 AND s.expires_at > now()",
            id_hash,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(row.map(|r| {
            (
                Session {
                    id_hash: r.id_hash,
                    operator_id: r.operator_id.clone(),
                    created_at: r.created_at,
                    expires_at: r.expires_at,
                },
                Operator {
                    id: r.operator_id,
                    email: r.email,
                    password_hash: r.password_hash,
                    role: r.role,
                    created_at: r.op_created_at,
                },
            )
        }))
    }

    /// End a session (logout). Unknown cookies are a no-op.
    pub async fn destroy(&self, cookie: &str) -> Result<(), AppError> {
        let Some(id_hash) = cookie_hash(cookie) else {
            return Ok(());
        };
        sqlx::query!("DELETE FROM sessions WHERE id_hash = $1", id_hash)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?;
        Ok(())
    }

    /// Delete expired rows; returns how many.
    pub async fn purge_expired(&self) -> Result<u64, AppError> {
        Ok(
            sqlx::query!("DELETE FROM sessions WHERE expires_at <= now()")
                .execute(self.pool)
                .await
                .map_err(map_db_error)?
                .rows_affected(),
        )
    }
}
