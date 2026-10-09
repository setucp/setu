//! `audit` repo: operator action trail (written by every mutating route).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: String,
    pub operator_id: Option<String>,
    pub action: String,
    pub target_kind: Option<String>,
    pub target_id: Option<String>,
    pub ip: Option<String>,
    pub at: DateTime<Utc>,
}

/// Repository accessor; build via [`crate::Db::audit`].
pub struct Audit<'a> {
    pool: &'a PgPool,
}

impl<'a> Audit<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn record(
        &self,
        operator_id: Option<&str>,
        action: &str,
        target_kind: Option<&str>,
        target_id: Option<&str>,
        ip: Option<&str>,
    ) -> Result<AuditEntry, AppError> {
        sqlx::query_as!(
            AuditEntry,
            "INSERT INTO audit_log (id, operator_id, action, target_kind, target_id, ip)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, operator_id, action, target_kind, target_id, ip, at",
            setu_common::new_id(),
            operator_id,
            action,
            target_kind,
            target_id,
            ip,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<AuditEntry>, AppError> {
        sqlx::query_as!(
            AuditEntry,
            "SELECT id, operator_id, action, target_kind, target_id, ip, at
             FROM audit_log ORDER BY at DESC, id DESC LIMIT $1",
            limit,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }
}
