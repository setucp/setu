//! `backups` repo: site backup records (files live on the agent host).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Known backup kinds and statuses; anything else is `validation`.
pub const BACKUP_KINDS: &[&str] = &["scheduled", "manual", "pre_update"];
pub const BACKUP_STATUSES: &[&str] = &["pending", "completed", "failed"];

#[derive(Debug, Clone)]
pub struct Backup {
    pub id: String,
    pub site_id: String,
    pub taken_at: DateTime<Utc>,
    pub size_bytes: Option<i64>,
    pub location: String,
    pub kind: String,
    pub status: String,
}

/// Repository accessor; build via [`crate::Db::backups`].
pub struct Backups<'a> {
    pool: &'a PgPool,
}

impl<'a> Backups<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn create(
        &self,
        site_id: &str,
        kind: &str,
        location: &str,
    ) -> Result<Backup, AppError> {
        if !BACKUP_KINDS.contains(&kind) {
            return Err(AppError::validation(format!("unknown backup kind {kind}")));
        }
        sqlx::query_as!(
            BackupRow,
            "INSERT INTO backups (id, site_id, kind, location)
             VALUES ($1, $2, $3, $4)
             RETURNING id, site_id, taken_at, size_bytes, location, kind, status",
            setu_common::new_id(),
            site_id,
            kind,
            location,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_backup())
    }

    pub async fn set_status(
        &self,
        id: &str,
        status: &str,
        size_bytes: Option<i64>,
    ) -> Result<Backup, AppError> {
        if !BACKUP_STATUSES.contains(&status) {
            return Err(AppError::validation(format!(
                "unknown backup status {status}"
            )));
        }
        sqlx::query_as!(
            BackupRow,
            "UPDATE backups SET status = $2, size_bytes = COALESCE($3, size_bytes)
             WHERE id = $1
             RETURNING id, site_id, taken_at, size_bytes, location, kind, status",
            id,
            status,
            size_bytes,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_backup())
        .ok_or_else(|| AppError::not_found("backup not found"))
    }

    pub async fn get(&self, id: &str) -> Result<Backup, AppError> {
        sqlx::query_as!(
            BackupRow,
            "SELECT id, site_id, taken_at, size_bytes, location, kind, status
             FROM backups WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_backup())
        .ok_or_else(|| AppError::not_found("backup not found"))
    }

    pub async fn list_for_site(&self, site_id: &str) -> Result<Vec<Backup>, AppError> {
        sqlx::query_as!(
            BackupRow,
            "SELECT id, site_id, taken_at, size_bytes, location, kind, status
             FROM backups WHERE site_id = $1 ORDER BY taken_at DESC, id DESC",
            site_id,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_backup()).collect())
    }
}

#[derive(sqlx::FromRow)]
struct BackupRow {
    id: String,
    site_id: String,
    taken_at: DateTime<Utc>,
    size_bytes: Option<i64>,
    location: String,
    kind: String,
    status: String,
}

impl BackupRow {
    fn into_backup(self) -> Backup {
        Backup {
            id: self.id,
            site_id: self.site_id,
            taken_at: self.taken_at,
            size_bytes: self.size_bytes,
            location: self.location,
            kind: self.kind,
            status: self.status,
        }
    }
}
