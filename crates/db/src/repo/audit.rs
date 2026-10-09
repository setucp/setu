//! `audit` repo: operator action trail (written by every mutating route).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

#[derive(Debug, Clone)]
pub struct AuditEntry {
    pub id: String,
    pub operator_id: Option<String>,
    pub action: String,
    pub target: Option<String>,
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
        target: Option<&str>,
        ip: Option<&str>,
    ) -> Result<AuditEntry, AppError> {
        sqlx::query_as!(
            AuditRow,
            "INSERT INTO audit_log (id, operator_id, action, target, ip)
             VALUES ($1, $2, $3, $4, $5)
             RETURNING id, operator_id, action, target, ip, at",
            setu_common::new_id(),
            operator_id,
            action,
            target,
            ip,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_entry())
    }

    pub async fn list_recent(&self, limit: i64) -> Result<Vec<AuditEntry>, AppError> {
        sqlx::query_as!(
            AuditRow,
            "SELECT id, operator_id, action, target, ip, at
             FROM audit_log ORDER BY at DESC, id DESC LIMIT $1",
            limit,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_entry()).collect())
    }
}

#[derive(sqlx::FromRow)]
struct AuditRow {
    id: String,
    operator_id: Option<String>,
    action: String,
    target: Option<String>,
    ip: Option<String>,
    at: DateTime<Utc>,
}

impl AuditRow {
    fn into_entry(self) -> AuditEntry {
        AuditEntry {
            id: self.id,
            operator_id: self.operator_id,
            action: self.action,
            target: self.target,
            ip: self.ip,
            at: self.at,
        }
    }
}
