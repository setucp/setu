//! `jobs` repo: the Postgres work queue with leases. A claim takes a
//! `queued` job, or a `running` one whose lease (`locked_until`) has
//! expired, with `FOR UPDATE SKIP LOCKED`; the runner renews the lease
//! while it works. One active (`queued`/`running`) job per site is
//! enforced by a partial unique index.

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Job kinds (spec §3); anything else is rejected with `validation`.
pub const JOB_KINDS: &[&str] = &[
    "provision_site",
    "update_site",
    "backup_site",
    "restore_site",
    "delete_site",
    "delete_backup",
    "env_update",
    "restart_site",
    "stop_site",
    "start_site",
];

/// One unit of background work. `payload` may carry sealed values and
/// the step cursor: `Debug` redacts it.
#[derive(Clone)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub site_id: Option<String>,
    pub server_id: Option<String>,
    pub status: String,
    pub payload: Value,
    pub attempts: i32,
    pub locked_by: Option<String>,
    pub locked_until: Option<DateTime<Utc>>,
    pub log: String,
    pub error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    pub finished_at: Option<DateTime<Utc>>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("site_id", &self.site_id)
            .field("server_id", &self.server_id)
            .field("status", &self.status)
            .field("attempts", &self.attempts)
            .field("payload", &"[redacted]")
            .field("locked_by", &self.locked_by)
            .field("locked_until", &self.locked_until)
            .field("error", &self.error)
            .finish()
    }
}

/// Repository accessor; build via [`crate::Db::jobs`].
pub struct Jobs<'a> {
    pool: &'a PgPool,
}

impl<'a> Jobs<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Queue a job. `conflict` when the site already has an active job.
    pub async fn enqueue(
        &self,
        kind: &str,
        site_id: Option<&str>,
        server_id: Option<&str>,
        payload: Option<&Value>,
    ) -> Result<Job, AppError> {
        if !JOB_KINDS.contains(&kind) {
            return Err(AppError::validation(format!("unknown job kind {kind}")));
        }
        let payload = payload.cloned().unwrap_or_else(|| serde_json::json!({}));
        sqlx::query_as!(
            Job,
            "INSERT INTO jobs (id, kind, site_id, server_id, payload)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at",
            setu_common::new_id(),
            kind,
            site_id,
            server_id,
            payload,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Claim the oldest `queued` job, or a `running` job whose lease
    /// expired (its runner died), and lease it to `worker`.
    pub async fn claim(&self, worker: &str, lease: Duration) -> Result<Option<Job>, AppError> {
        sqlx::query_as!(
            Job,
            "UPDATE jobs SET status = 'running', locked_by = $1,
                             locked_until = now() + make_interval(secs => $2),
                             attempts = attempts + 1,
                             started_at = COALESCE(started_at, now())
             WHERE id = (
                 SELECT id FROM jobs
                 WHERE status = 'queued'
                    OR (status = 'running' AND locked_until < now())
                 ORDER BY created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             RETURNING id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at",
            worker,
            crate::repo::secs(lease),
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Extend the lease; `false` when `worker` no longer holds the job
    /// (it was reclaimed or finished).
    pub async fn renew(&self, id: &str, worker: &str, lease: Duration) -> Result<bool, AppError> {
        let n = sqlx::query!(
            "UPDATE jobs SET locked_until = now() + make_interval(secs => $3)
             WHERE id = $1 AND locked_by = $2 AND status = 'running'",
            id,
            worker,
            crate::repo::secs(lease),
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n == 1)
    }

    /// Store the step cursor (`payload.step`). Only the lease holder
    /// may: `conflict` when `worker` lost the lease (M2).
    pub async fn set_step(&self, id: &str, worker: &str, step: &str) -> Result<(), AppError> {
        let n = sqlx::query!(
            "UPDATE jobs SET payload = jsonb_set(payload, '{step}', to_jsonb($3::text))
             WHERE id = $1 AND locked_by = $2 AND status = 'running'",
            id,
            worker,
            step,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            return Err(self.not_held(id).await);
        }
        Ok(())
    }

    /// The step cursor, if any.
    pub async fn get_step(&self, id: &str) -> Result<Option<String>, AppError> {
        sqlx::query_scalar!(
            "SELECT payload->>'step' AS step FROM jobs WHERE id = $1",
            id
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("job not found"))
    }

    /// Replace the payload (lease holder only, as [`Self::set_step`]).
    pub async fn set_payload(
        &self,
        id: &str,
        worker: &str,
        payload: &Value,
    ) -> Result<(), AppError> {
        let n = sqlx::query!(
            "UPDATE jobs SET payload = $3
             WHERE id = $1 AND locked_by = $2 AND status = 'running'",
            id,
            worker,
            payload
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            return Err(self.not_held(id).await);
        }
        Ok(())
    }

    /// Append to the log while `worker` holds the lease; `false` (and
    /// nothing written) when it does not, or the job is gone.
    pub async fn append_log(&self, id: &str, worker: &str, chunk: &str) -> Result<bool, AppError> {
        let n = sqlx::query!(
            "UPDATE jobs SET log = log || $3
             WHERE id = $1 AND locked_by = $2 AND status = 'running'",
            id,
            worker,
            chunk
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n == 1)
    }

    /// End the job `done` or `failed` and release the lease. Only the
    /// lease holder may: `conflict` when `worker` lost it, `not_found`
    /// when the row is gone.
    pub async fn finish(
        &self,
        id: &str,
        worker: &str,
        ok: bool,
        error: Option<&str>,
    ) -> Result<Job, AppError> {
        let status = if ok { "done" } else { "failed" };
        let row = sqlx::query_as!(
            Job,
            "UPDATE jobs SET status = $3, finished_at = now(), error = $4,
                             locked_by = NULL, locked_until = NULL
             WHERE id = $1 AND locked_by = $2 AND status = 'running'
             RETURNING id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at",
            id,
            worker,
            status,
            error,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?;
        match row {
            Some(job) => Ok(job),
            None => Err(self.not_held(id).await),
        }
    }

    /// Why a lease-holder write matched no row: the job is gone
    /// (`not_found`) or `worker` does not hold it (`conflict`).
    async fn not_held(&self, id: &str) -> AppError {
        match self.get(id).await {
            Ok(_) => AppError::conflict("job lease lost: another runner owns it"),
            Err(e) => e,
        }
    }

    /// Put `running` jobs whose lease expired back to `queued` (boot
    /// recovery). Returns how many.
    pub async fn reset_expired_running(&self) -> Result<u64, AppError> {
        Ok(sqlx::query!(
            "UPDATE jobs SET status = 'queued', locked_by = NULL, locked_until = NULL
             WHERE status = 'running' AND locked_until < now()"
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected())
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<Job>, AppError> {
        sqlx::query_as!(
            Job,
            "SELECT id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at FROM jobs ORDER BY created_at DESC, id DESC LIMIT $1",
            limit,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn get(&self, id: &str) -> Result<Job, AppError> {
        sqlx::query_as!(Job, "SELECT id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at FROM jobs WHERE id = $1", id)
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| AppError::not_found("job not found"))
    }

    /// The site's active (`queued`/`running`) job, if any.
    pub async fn active_for_site(&self, site_id: &str) -> Result<Option<Job>, AppError> {
        sqlx::query_as!(
            Job,
            "SELECT id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at FROM jobs
             WHERE site_id = $1 AND status IN ('queued', 'running')",
            site_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// An active (`queued`/`running`) `delete_backup` job for this backup.
    pub async fn active_delete_backup(&self, backup_id: &str) -> Result<Option<Job>, AppError> {
        sqlx::query_as!(
            Job,
            "SELECT id, kind, site_id, server_id, status, payload, attempts, locked_by,
                       locked_until, log, error, created_at, started_at, finished_at FROM jobs
             WHERE kind = 'delete_backup' AND status IN ('queued', 'running')
               AND payload->>'backup_id' = $1
             LIMIT 1",
            backup_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }
}
