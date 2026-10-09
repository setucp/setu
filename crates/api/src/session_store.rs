//! Postgres-backed session store for `tower-sessions`.
//!
//! Ruling (Task 7): `tower-sessions-sqlx-store` (all published versions)
//! requires `sqlx 0.8`, but the workspace pins `sqlx 0.9` (Task 5). Rather
//! than downgrade the database layer, this module implements the same
//! [`SessionStore`] contract directly over the existing `sessions` table
//! (`id TEXT PK, data BYTEA, expiry TIMESTAMPTZ`) with `sqlx 0.9`.
//! Encoding matches the upstream store: the full `Record` as MessagePack.
//! Cost if wrong: sessions stop working when the upstream crate releases a
//! `sqlx 0.9` version — swapping the store is a one-line change in
//! [`super::rest::router`] plus deleting this module.

use async_trait::async_trait;
use sqlx::PgPool;
use tower_sessions::session_store::{Error as StoreError, Result as StoreResult};
use tower_sessions::{session::Record, SessionStore};

/// Session store over the `sessions` table. `Debug` is manual: it holds no
/// secrets (record data may hold an operator id only), but the pool's debug
/// could include the connection URL, so redact it.
#[derive(Clone)]
pub struct PgSessionStore {
    pool: PgPool,
}

impl std::fmt::Debug for PgSessionStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PgSessionStore([redacted])")
    }
}

impl PgSessionStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Delete rows past their expiry. Called periodically from `serve` and
    /// from tests setup; loads also enforce expiry eagerly.
    pub async fn delete_expired(&self) -> Result<u64, StoreError> {
        let n = sqlx::query("DELETE FROM sessions WHERE expiry <= now()")
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
            .rows_affected();
        Ok(n)
    }

    fn expiry_timestamp(record: &Record) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::from_timestamp(record.expiry_date.unix_timestamp(), 0)
            .unwrap_or(chrono::DateTime::<chrono::Utc>::MIN_UTC)
    }
}

#[async_trait]
impl SessionStore for PgSessionStore {
    async fn create(&self, record: &mut Record) -> StoreResult<()> {
        // Session-ID collision mitigation (same as the reference stores):
        // retry with a fresh id while the PK rejects the insert.
        for _ in 0..5 {
            let data =
                rmp_serde::to_vec(&*record).map_err(|e| StoreError::Encode(e.to_string()))?;
            let expiry = Self::expiry_timestamp(record);
            let n = sqlx::query(
                "INSERT INTO sessions (id, data, expiry)
                 VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
            )
            .bind(record.id.to_string())
            .bind(data)
            .bind(expiry)
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?
            .rows_affected();
            if n == 1 {
                return Ok(());
            }
            record.id = Default::default();
        }
        Err(StoreError::Backend(
            "could not allocate a session id".to_string(),
        ))
    }

    async fn save(&self, record: &Record) -> StoreResult<()> {
        let data = rmp_serde::to_vec(record).map_err(|e| StoreError::Encode(e.to_string()))?;
        let expiry = Self::expiry_timestamp(record);
        sqlx::query(
            "INSERT INTO sessions (id, data, expiry)
             VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET data = excluded.data, expiry = excluded.expiry",
        )
        .bind(record.id.to_string())
        .bind(data)
        .bind(expiry)
        .execute(&self.pool)
        .await
        .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }

    async fn load(&self, session_id: &tower_sessions::session::Id) -> StoreResult<Option<Record>> {
        let row: Option<(Vec<u8>, chrono::DateTime<chrono::Utc>)> =
            sqlx::query_as("SELECT data, expiry FROM sessions WHERE id = $1")
                .bind(session_id.to_string())
                .fetch_optional(&self.pool)
                .await
                .map_err(|e| StoreError::Backend(e.to_string()))?;
        let Some((data, _)) = row else {
            return Ok(None);
        };
        let record: Record =
            rmp_serde::from_slice(&data).map_err(|e| StoreError::Decode(e.to_string()))?;
        if record.expiry_date <= time::OffsetDateTime::now_utc() {
            self.delete(session_id).await?;
            return Ok(None);
        }
        Ok(Some(record))
    }

    async fn delete(&self, session_id: &tower_sessions::session::Id) -> StoreResult<()> {
        sqlx::query("DELETE FROM sessions WHERE id = $1")
            .bind(session_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|e| StoreError::Backend(e.to_string()))?;
        Ok(())
    }
}
