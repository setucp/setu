//! `site_metrics` repo: 1-minute samples, upserted from heartbeats,
//! pruned after 30 days.

use std::time::Duration;

use chrono::Utc;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Repository accessor; build via [`crate::Db::metrics`].
pub struct Metrics<'a> {
    pool: &'a PgPool,
}

impl<'a> Metrics<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Upsert a batch of samples (heartbeats may resend). Returns the
    /// number of rows processed.
    pub async fn insert_batch(&self, metrics: &[setu_proto::SiteMetric]) -> Result<u64, AppError> {
        insert_metrics(self.pool, metrics).await
    }

    /// Delete samples older than `older_than`. Returns rows removed.
    pub async fn prune(&self, older_than: Duration) -> Result<u64, AppError> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(older_than)
                .map_err(|_| AppError::validation("prune window out of range"))?;
        let n = sqlx::query!("DELETE FROM site_metrics WHERE at < $1", cutoff)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?
            .rows_affected();
        Ok(n)
    }
}

/// Shared by [`Metrics::insert_batch`] and the servers heartbeat path.
pub(crate) async fn insert_metrics(
    pool: &PgPool,
    metrics: &[setu_proto::SiteMetric],
) -> Result<u64, AppError> {
    if metrics.is_empty() {
        return Ok(0);
    }
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let mut count = 0u64;
    for m in metrics {
        sqlx::query!(
            "INSERT INTO site_metrics (site_id, at, cpu_seconds, memory_bytes, disk_bytes)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (site_id, at) DO UPDATE
               SET cpu_seconds = excluded.cpu_seconds,
                   memory_bytes = excluded.memory_bytes,
                   disk_bytes = excluded.disk_bytes",
            m.site_id,
            m.at,
            m.cpu_seconds,
            m.memory_bytes as i64,
            m.disk_bytes as i64,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        count += 1;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(count)
}
