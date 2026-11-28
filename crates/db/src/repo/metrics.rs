//! `site_metrics` repo: 1-minute samples, upserted from heartbeats,
//! pruned after 30 days.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// The newest sample of a site.
#[derive(Debug, Clone, PartialEq)]
pub struct LatestMetric {
    pub site_id: String,
    pub at: DateTime<Utc>,
    pub cpu_seconds: f64,
    pub memory_bytes: i64,
    pub disk_bytes: i64,
}

/// Repository accessor; build via [`crate::Db::metrics`].
pub struct Metrics<'a> {
    pool: &'a PgPool,
}

impl<'a> Metrics<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Upsert a batch of one server's samples (heartbeats may resend).
    /// Samples name sites by short id; a short id that is not a site on
    /// `server_id` is ignored. Returns the number of rows written.
    pub async fn insert_batch(
        &self,
        server_id: &str,
        metrics: &[setu_proto::SiteMetric],
    ) -> Result<u64, AppError> {
        if metrics.is_empty() {
            return Ok(0);
        }
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let count = insert_metrics_tx(&mut tx, server_id, metrics).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(count)
    }

    /// The site's newest sample, if any.
    pub async fn latest_for_site(&self, site_id: &str) -> Result<Option<LatestMetric>, AppError> {
        sqlx::query_as!(
            LatestMetric,
            "SELECT site_id, at, cpu_seconds, memory_bytes, disk_bytes
             FROM site_metrics WHERE site_id = $1 ORDER BY at DESC LIMIT 1",
            site_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// The newest sample of each of `site_ids` in one query. Sites
    /// without a sample are absent from the map.
    ///
    /// One backward read of the `(site_id, at)` primary key per site (a
    /// `LATERAL` subquery with `LIMIT 1`). `DISTINCT ON (site_id)` reads
    /// every row of every listed site (~43,000 per site after 30 days)
    /// and took 8-10 s in production.
    pub async fn latest_for_sites(
        &self,
        site_ids: &[String],
    ) -> Result<std::collections::HashMap<String, LatestMetric>, AppError> {
        Ok(self
            .latest_for_sites_within(site_ids, None)
            .await?
            .unwrap_or_default())
    }

    /// Like [`Self::latest_for_sites`], but Postgres itself cancels the
    /// query after `limit` (`SET LOCAL statement_timeout`), returning
    /// `Ok(None)`. Dropping a sqlx future does not cancel the running
    /// query (the connection would stay busy until Postgres finishes),
    /// so a caller-side timeout alone is not enough to free the pool.
    pub async fn latest_for_sites_within(
        &self,
        site_ids: &[String],
        limit: Option<Duration>,
    ) -> Result<Option<std::collections::HashMap<String, LatestMetric>>, AppError> {
        if site_ids.is_empty() {
            return Ok(Some(std::collections::HashMap::new()));
        }
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        if let Some(limit) = limit {
            // SET LOCAL takes no bind parameters; the value is an integer.
            let ms = limit.as_millis().max(1);
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "SET LOCAL statement_timeout = {ms}"
            )))
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        let rows = sqlx::query_as!(
            LatestMetric,
            r#"SELECT s.id AS "site_id!", m.at AS "at!", m.cpu_seconds AS "cpu_seconds!",
                      m.memory_bytes AS "memory_bytes!", m.disk_bytes AS "disk_bytes!"
               FROM unnest($1::text[]) AS s(id)
               CROSS JOIN LATERAL (
                   SELECT at, cpu_seconds, memory_bytes, disk_bytes
                   FROM site_metrics WHERE site_id = s.id
                   ORDER BY at DESC LIMIT 1
               ) m"#,
            site_ids,
        )
        .fetch_all(&mut *tx)
        .await;
        match rows {
            Ok(rows) => {
                tx.commit().await.map_err(map_db_error)?;
                Ok(Some(
                    rows.into_iter().map(|m| (m.site_id.clone(), m)).collect(),
                ))
            }
            // 57014 = query_canceled (statement_timeout fired).
            Err(sqlx::Error::Database(db)) if db.code().as_deref() == Some("57014") => {
                let _ = tx.rollback().await;
                Ok(None)
            }
            Err(e) => Err(map_db_error(e)),
        }
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

/// Insert one server's batch inside the caller's transaction (the
/// heartbeat path commits the server update and the metrics together).
/// Short ids resolve against that server's sites only, so an agent can
/// never write another server's metrics.
pub(crate) async fn insert_metrics_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    server_id: &str,
    metrics: &[setu_proto::SiteMetric],
) -> Result<u64, AppError> {
    if metrics.is_empty() {
        return Ok(0);
    }
    let owned = sqlx::query!(
        "SELECT id, short_id FROM sites WHERE server_id = $1",
        server_id
    )
    .fetch_all(&mut **tx)
    .await
    .map_err(map_db_error)?;
    let mut count = 0u64;
    for m in metrics {
        let Some(site) = owned.iter().find(|r| r.short_id == m.short_id) else {
            tracing::debug!(server_id = %server_id, "heartbeat ignored a foreign metric");
            continue;
        };
        sqlx::query!(
            "INSERT INTO site_metrics (site_id, at, cpu_seconds, memory_bytes, disk_bytes)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (site_id, at) DO UPDATE
               SET cpu_seconds = excluded.cpu_seconds,
                   memory_bytes = excluded.memory_bytes,
                   disk_bytes = excluded.disk_bytes",
            site.id,
            m.at,
            m.cpu_seconds,
            i64::try_from(m.memory_bytes).unwrap_or(i64::MAX),
            i64::try_from(m.disk_bytes).unwrap_or(i64::MAX),
        )
        .execute(&mut **tx)
        .await
        .map_err(map_db_error)?;
        count += 1;
    }
    Ok(count)
}
