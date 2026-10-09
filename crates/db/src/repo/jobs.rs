//! `jobs` repo: the Postgres work queue (`FOR UPDATE SKIP LOCKED`).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Known job kinds; anything else is rejected with `validation`.
pub const JOB_KINDS: &[&str] = &[
    "provision_site",
    "update_site",
    "backup_site",
    "restore_site",
    "delete_site",
    "restart_site",
    "stop_site",
    "start_site",
    "reconcile",
];

/// One unit of background work. `payload` may carry secrets (prompt
/// answers before they are sealed): `Debug` redacts it.
pub struct Job {
    pub id: String,
    pub kind: String,
    pub site_id: Option<String>,
    pub server_id: Option<String>,
    pub status: String,
    pub attempts: i32,
    pub payload: Option<Value>,
    pub log: String,
    pub error: Option<String>,
    pub worker: Option<String>,
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
            .field("error", &self.error)
            .field("worker", &self.worker)
            .finish()
    }
}

impl Clone for Job {
    fn clone(&self) -> Self {
        Self {
            id: self.id.clone(),
            kind: self.kind.clone(),
            site_id: self.site_id.clone(),
            server_id: self.server_id.clone(),
            status: self.status.clone(),
            attempts: self.attempts,
            payload: self.payload.clone(),
            log: self.log.clone(),
            error: self.error.clone(),
            worker: self.worker.clone(),
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
        }
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
        sqlx::query_as!(
            JobRow,
            "INSERT INTO jobs (id, kind, site_id, server_id, payload)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, kind, site_id, server_id, status, attempts, payload,
                       log, error, worker, created_at, started_at, finished_at",
            setu_common::new_id(),
            kind,
            site_id,
            server_id,
            payload,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_job())
    }

    /// Atomically move the oldest `queued` job to `running`. The
    /// select-and-update is a single statement, so concurrent workers
    /// never receive the same job (`SKIP LOCKED` covers the case where
    /// a sibling transaction holds a row lock).
    pub async fn claim(&self, worker: &str) -> Result<Option<Job>, AppError> {
        sqlx::query_as!(
            JobRow,
            "UPDATE jobs SET status = 'running', started_at = now(),
                             attempts = attempts + 1, worker = $1
             WHERE id = (
                 SELECT id FROM jobs
                 WHERE status = 'queued'
                 ORDER BY created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             RETURNING id, kind, site_id, server_id, status, attempts, payload,
                       log, error, worker, created_at, started_at, finished_at",
            worker,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.map(|r| r.into_job()))
    }

    pub async fn append_log(&self, id: &str, chunk: &str) -> Result<(), AppError> {
        let n = sqlx::query!("UPDATE jobs SET log = log || $2 WHERE id = $1", id, chunk)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?
            .rows_affected();
        if n == 0 {
            return Err(AppError::not_found("job not found"));
        }
        Ok(())
    }

    pub async fn finish(&self, id: &str, ok: bool, error: Option<&str>) -> Result<Job, AppError> {
        let status = if ok { "completed" } else { "failed" };
        sqlx::query_as!(
            JobRow,
            "UPDATE jobs SET status = $2, finished_at = now(), error = $3
             WHERE id = $1
             RETURNING id, kind, site_id, server_id, status, attempts, payload,
                       log, error, worker, created_at, started_at, finished_at",
            id,
            status,
            error,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_job())
        .ok_or_else(|| AppError::not_found("job not found"))
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<Job>, AppError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, kind, site_id, server_id, status, attempts, payload,
                    log, error, worker, created_at, started_at, finished_at
             FROM jobs ORDER BY created_at DESC, id DESC LIMIT $1",
            limit,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_job()).collect())
    }

    pub async fn get(&self, id: &str) -> Result<Job, AppError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, kind, site_id, server_id, status, attempts, payload,
                    log, error, worker, created_at, started_at, finished_at
             FROM jobs WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_job())
        .ok_or_else(|| AppError::not_found("job not found"))
    }

    /// Any live (`queued` or `running`) job for the site, newest first.
    /// Mutating site routes use this to reject overlapping work.
    pub async fn running_for_site(&self, site_id: &str) -> Result<Option<Job>, AppError> {
        sqlx::query_as!(
            JobRow,
            "SELECT id, kind, site_id, server_id, status, attempts, payload,
                    log, error, worker, created_at, started_at, finished_at
             FROM jobs
             WHERE site_id = $1 AND status IN ('queued', 'running')
             ORDER BY created_at DESC, id DESC LIMIT 1",
            site_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.map(|r| r.into_job()))
    }
}

#[derive(sqlx::FromRow)]
struct JobRow {
    id: String,
    kind: String,
    site_id: Option<String>,
    server_id: Option<String>,
    status: String,
    attempts: i32,
    payload: Option<Value>,
    log: String,
    error: Option<String>,
    worker: Option<String>,
    created_at: DateTime<Utc>,
    started_at: Option<DateTime<Utc>>,
    finished_at: Option<DateTime<Utc>>,
}

impl JobRow {
    fn into_job(self) -> Job {
        Job {
            id: self.id,
            kind: self.kind,
            site_id: self.site_id,
            server_id: self.server_id,
            status: self.status,
            attempts: self.attempts,
            payload: self.payload,
            log: self.log,
            error: self.error,
            worker: self.worker,
            created_at: self.created_at,
            started_at: self.started_at,
            finished_at: self.finished_at,
        }
    }
}
