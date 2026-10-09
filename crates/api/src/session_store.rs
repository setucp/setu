//! `tower-sessions` store over the spec §3 `sessions` table.
//!
//! Ruling (Task 7): `tower-sessions-sqlx-store` requires `sqlx 0.8`, so
//! this module implements [`SessionStore`] directly. Ruling R78 (WS4):
//! the table now has the spec shape (`id_hash`, `operator_id`,
//! `created_at`, `expires_at`), so the adapter stores the SHA-256 of
//! the tower session id as `id_hash` and the record's only datum, the
//! operator id, as `operator_id`; a record without an operator id (an
//! anonymous session) is not persisted. Password changes delete the
//! operator's rows (`operators().set_password_hash`). WS8 replaces
//! tower-sessions with `db.sessions()` (32-byte cookie, sliding 7 days).

use async_trait::async_trait;
use sqlx::PgPool;
use tower_sessions::session_store::{Error as StoreError, Result as StoreResult};
use tower_sessions::{session::Record, SessionStore};

use crate::auth::SESSION_OPERATOR_ID;

/// Session store over the `sessions` table. `Debug` is manual: the
/// pool's debug could include the connection URL.
#[derive(Clone)]
pub struct PgSessionStore {
    pool: PgPool,
}

impl std::fmt::Debug for PgSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PgSessionStore([redacted])")
    }
}

fn id_hash(id: &tower_sessions::session::Id) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(id.to_string().as_bytes()))
}

fn backend(e: sqlx::Error) -> StoreError {
    StoreError::Backend(e.to_string())
}

fn operator_id(record: &Record) -> Option<String> {
    record
        .data
        .get(SESSION_OPERATOR_ID)
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

impl PgSessionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Delete rows past their expiry. Called periodically from `serve`.
    pub async fn delete_expired(&self) -> Result<u64, StoreError> {
        let n = sqlx::query("DELETE FROM sessions WHERE expires_at <= now()")
            .execute(&self.pool)
            .await
            .map_err(backend)?
            .rows_affected();
        Ok(n)
    }

    fn expiry_timestamp(record: &Record) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(record.expiry_date.unix_timestamp(), 0)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
    }

    /// Insert or refresh; `Ok(false)` when the id is taken by another row
    /// (only on `create`).
    async fn upsert(&self, record: &Record, overwrite: bool) -> StoreResult<bool> {
        let Some(op) = operator_id(record) else {
            return Ok(true);
        };
        let sql = if overwrite {
            "INSERT INTO sessions (id_hash, operator_id, expires_at) VALUES ($1, $2, $3)
             ON CONFLICT (id_hash) DO UPDATE
               SET operator_id = excluded.operator_id, expires_at = excluded.expires_at"
        } else {
            "INSERT INTO sessions (id_hash, operator_id, expires_at) VALUES ($1, $2, $3)
             ON CONFLICT (id_hash) DO NOTHING"
        };
        let res = sqlx::query(sql)
            .bind(id_hash(&record.id))
            .bind(op)
            .bind(Self::expiry_timestamp(record))
            .execute(&self.pool)
            .await;
        match res {
            Ok(r) => Ok(r.rows_affected() == 1),
            // The operator was deleted meanwhile: nothing to persist.
            Err(sqlx::Error::Database(e)) if e.code().as_deref() == Some("23503") => Ok(true),
            Err(e) => Err(backend(e)),
        }
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create(&self, record: &mut Record) -> StoreResult<()> {
        // Session-ID collision mitigation (same as the reference stores).
        for _ in 0..5 {
            if self.upsert(record, false).await? {
                return Ok(());
            }
            record.id = Default::default();
        }
        Err(StoreError::Backend(
            "could not allocate a session id".to_string(),
        ))
    }

    async fn save(&self, record: &Record) -> StoreResult<()> {
        self.upsert(record, true).await.map(|_| ())
    }

    async fn load(&self, session_id: &tower_sessions::session::Id) -> StoreResult<Option<Record>> {
        let row: Option<(String, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
            "SELECT operator_id, expires_at FROM sessions
             WHERE id_hash = $1 AND expires_at > now()",
        )
        .bind(id_hash(session_id))
        .fetch_optional(&self.pool)
        .await
        .map_err(backend)?;
        let Some((operator_id, expires_at)) = row else {
            return Ok(None);
        };
        let expiry_date = time::OffsetDateTime::from_unix_timestamp(expires_at.timestamp())
            .map_err(|e| StoreError::Decode(e.to_string()))?;
        let mut data = std::collections::HashMap::new();
        data.insert(
            SESSION_OPERATOR_ID.to_string(),
            serde_json::Value::String(operator_id),
        );
        Ok(Some(Record {
            id: *session_id,
            data,
            expiry_date,
        }))
    }

    async fn delete(&self, session_id: &tower_sessions::session::Id) -> StoreResult<()> {
        sqlx::query("DELETE FROM sessions WHERE id_hash = $1")
            .bind(id_hash(session_id))
            .execute(&self.pool)
            .await
            .map_err(backend)?;
        Ok(())
    }
}
