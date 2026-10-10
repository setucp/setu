//! `backups` repo: backup records. Files live on the site's server at
//! `/var/lib/setu/backups/<site_short_id>/<id>/{db.dump,dirs.tar.zst}`;
//! the row outlives its site (`site_id` is nulled on site delete).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::repo::sites::Site;
use crate::{map_db_error, AppError};

/// Backup kinds and statuses; anything else is `validation`.
pub const BACKUP_KINDS: &[&str] = &[
    "scheduled",
    "manual",
    "pre_update",
    "pre_restore",
    "pre_delete",
];
pub const BACKUP_STATUSES: &[&str] = &["pending", "completed", "failed", "deleted"];

#[derive(Debug, Clone)]
pub struct Backup {
    pub id: String,
    pub site_id: Option<String>,
    pub server_id: String,
    pub site_short_id: String,
    pub site_name: String,
    pub domain: String,
    pub kind: String,
    pub taken_at: DateTime<Utc>,
    pub size_bytes: Option<i64>,
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

    /// A `pending` backup of `site` (its server, short id, name and
    /// domain are copied so the row survives the site).
    pub async fn create(&self, site: &Site, kind: &str) -> Result<Backup, AppError> {
        if !BACKUP_KINDS.contains(&kind) {
            return Err(AppError::validation(format!("unknown backup kind {kind}")));
        }
        sqlx::query_as!(
            Backup,
            "INSERT INTO backups (id, site_id, server_id, site_short_id, site_name, domain, kind)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             RETURNING id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status",
            setu_common::new_id(),
            site.id,
            site.server_id,
            site.short_id,
            site.name,
            site.domain,
            kind,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
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
            Backup,
            "UPDATE backups SET status = $2, size_bytes = COALESCE($3, size_bytes)
             WHERE id = $1
             RETURNING id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status",
            id,
            status,
            size_bytes,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("backup not found"))
    }

    /// The files were removed from the server.
    pub async fn mark_deleted(&self, id: &str) -> Result<Backup, AppError> {
        self.set_status(id, "deleted", None).await
    }

    pub async fn get(&self, id: &str) -> Result<Backup, AppError> {
        sqlx::query_as!(
            Backup,
            "SELECT id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status FROM backups WHERE id = $1",
            id
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("backup not found"))
    }

    /// The site's backups, newest first (deleted ones excluded).
    pub async fn list_for_site(&self, site_id: &str) -> Result<Vec<Backup>, AppError> {
        sqlx::query_as!(
            Backup,
            "SELECT id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status FROM backups
             WHERE site_id = $1 AND status <> 'deleted'
             ORDER BY taken_at DESC, id DESC",
            site_id,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Every backup, newest first (deleted ones excluded), including
    /// those whose site is gone (`site_id` NULL: `pre_delete` backups
    /// outlive their site).
    pub async fn list_all(&self) -> Result<Vec<Backup>, AppError> {
        sqlx::query_as!(
            Backup,
            "SELECT id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status FROM backups
             WHERE status <> 'deleted'
             ORDER BY taken_at DESC, id DESC",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// The site's `scheduled` backups beyond the newest `keep` (the
    /// scheduler deletes these).
    pub async fn scheduled_beyond(
        &self,
        site_id: &str,
        keep: i64,
    ) -> Result<Vec<Backup>, AppError> {
        sqlx::query_as!(
            Backup,
            "SELECT id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status FROM backups
             WHERE site_id = $1 AND kind = 'scheduled' AND status <> 'deleted'
             ORDER BY taken_at DESC, id DESC
             OFFSET $2",
            site_id,
            keep,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// `pre_*` backups older than `days` (the scheduler deletes these).
    pub async fn pre_older_than(&self, days: i32) -> Result<Vec<Backup>, AppError> {
        sqlx::query_as!(
            Backup,
            "SELECT id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status FROM backups
             WHERE kind IN ('pre_update', 'pre_restore', 'pre_delete')
               AND status <> 'deleted'
               AND taken_at < now() - make_interval(days => $1)
             ORDER BY taken_at, id",
            days,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// `pre_*` backups taken before `cutoff` (the scheduler passes its
    /// clock's now minus 30 days).
    pub async fn pre_taken_before(&self, cutoff: DateTime<Utc>) -> Result<Vec<Backup>, AppError> {
        sqlx::query_as!(
            Backup,
            "SELECT id, site_id, server_id, site_short_id, site_name, domain, kind, taken_at,
                    size_bytes, status FROM backups
             WHERE kind IN ('pre_update', 'pre_restore', 'pre_delete')
               AND status <> 'deleted'
               AND taken_at < $1
             ORDER BY taken_at, id",
            cutoff,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// When the site's newest `scheduled` backup (any status) was taken.
    pub async fn latest_scheduled_at(
        &self,
        site_id: &str,
    ) -> Result<Option<DateTime<Utc>>, AppError> {
        sqlx::query_scalar!(
            "SELECT max(taken_at) FROM backups WHERE site_id = $1 AND kind = 'scheduled'",
            site_id,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }
}
