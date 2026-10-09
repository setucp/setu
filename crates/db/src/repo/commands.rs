//! `agent_commands` repo: the long-poll command queue (spec §5).
//!
//! `queued` → `delivered` (`next_for`) → `done`/`failed` (`complete`),
//! or `cancelled` (`cancel`, a job step that timed out) or `expired`
//! (its `expires_at` passed before delivery). Secret fields (the
//! `create_database` password, `write_env` values) are nulled by the SQL
//! function `setu_redact_command` whenever a row leaves
//! `queued`/`delivered`, and by `prune` as a safety net. A new agent
//! `boot_id` requeues that server's `delivered` rows
//! (`requeue_delivered`); nothing else redelivers.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// `NOTIFY` channel; the payload is the server id.
pub const NOTIFY_CHANNEL: &str = "setu_commands";
/// `wait` polls this often.
pub const WAIT_POLL: Duration = Duration::from_millis(300);
/// `prune` redacts (and expires) any non-terminal row older than this.
pub const STALE_SECS: f64 = 24.0 * 3600.0;

/// Stored command row. `command`/`result` may embed secrets: `Debug`
/// redacts both.
#[derive(Clone)]
pub struct AgentCommand {
    pub id: String,
    pub server_id: String,
    pub site_short_id: Option<String>,
    pub command: Value,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<Value>,
    pub status: String,
    pub attempts: i32,
}

impl std::fmt::Debug for AgentCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCommand")
            .field("id", &self.id)
            .field("server_id", &self.server_id)
            .field("site_short_id", &self.site_short_id)
            .field("command", &"[redacted]")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("status", &self.status)
            .field("result", &"[redacted]")
            .field("attempts", &self.attempts)
            .finish()
    }
}

/// Why `wait` returned without a result.
#[derive(Debug, thiserror::Error)]
pub enum WaitError {
    #[error("timed out waiting for the agent")]
    Timeout,
    #[error("command was cancelled")]
    Cancelled,
    #[error("command expired before delivery")]
    Expired,
    #[error(transparent)]
    Db(#[from] AppError),
}

/// Repository accessor; build via [`crate::Db::commands`].
pub struct Commands<'a> {
    pool: &'a PgPool,
}

fn to_value<T: serde::Serialize>(v: &T) -> Result<Value, AppError> {
    serde_json::to_value(v).map_err(|e| {
        tracing::error!(error = %e, "agent command is not serializable");
        AppError::internal()
    })
}

impl<'a> Commands<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Queue a command that expires after `ttl`, and `NOTIFY
    /// setu_commands, '<server_id>'` (delivered on commit).
    pub async fn enqueue(
        &self,
        server_id: &str,
        site_short_id: Option<&str>,
        command: &setu_proto::Command,
        ttl: Duration,
    ) -> Result<setu_proto::CommandEnvelope, AppError> {
        let value = to_value(command)?;
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let row = sqlx::query!(
            "INSERT INTO agent_commands (id, server_id, site_short_id, command, expires_at)
             VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5))
             RETURNING id, created_at",
            setu_common::new_id(),
            server_id,
            site_short_id,
            value,
            crate::repo::secs(ttl),
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        sqlx::query!("SELECT pg_notify($1, $2)", NOTIFY_CHANNEL, server_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(setu_proto::CommandEnvelope {
            id: row.id,
            created_at: row.created_at,
            command: command.clone(),
            attempts: 0,
        })
    }

    /// Deliver the server's oldest live `queued` command: rows whose
    /// `expires_at` passed are first marked `expired` (and redacted).
    pub async fn next_for(
        &self,
        server_id: &str,
    ) -> Result<Option<setu_proto::CommandEnvelope>, AppError> {
        sqlx::query!(
            "UPDATE agent_commands
             SET status = 'expired', command = setu_redact_command(command)
             WHERE server_id = $1 AND status = 'queued' AND expires_at <= now()",
            server_id,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        let row = sqlx::query!(
            "UPDATE agent_commands
             SET status = 'delivered', delivered_at = now(), attempts = attempts + 1
             WHERE id = (
                 SELECT id FROM agent_commands
                 WHERE server_id = $1 AND status = 'queued' AND expires_at > now()
                 ORDER BY created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
             RETURNING id, command, created_at, attempts",
            server_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?;
        row.map(|r| {
            let command = serde_json::from_value(r.command).map_err(|e| {
                tracing::error!(error = %e, "stored agent command is corrupt");
                AppError::internal()
            })?;
            Ok(setu_proto::CommandEnvelope {
                id: r.id,
                created_at: r.created_at,
                command,
                attempts: r.attempts,
            })
        })
        .transpose()
    }

    /// Store the result (`done` when `ok`, else `failed`) and redact.
    /// Only a `queued`/`delivered` row changes; a repeat post, or a post
    /// for a cancelled/expired row, is an ok no-op; an unknown id is 404.
    pub async fn complete(
        &self,
        id: &str,
        result: &setu_proto::CommandResult,
    ) -> Result<(), AppError> {
        self.complete_scoped(None, id, result).await
    }

    /// [`Self::complete`] for the agent protocol: only the server the
    /// command was queued for may complete it (another server's id is
    /// 404).
    pub async fn complete_for(
        &self,
        server_id: &str,
        id: &str,
        result: &setu_proto::CommandResult,
    ) -> Result<(), AppError> {
        self.complete_scoped(Some(server_id), id, result).await
    }

    async fn complete_scoped(
        &self,
        server_id: Option<&str>,
        id: &str,
        result: &setu_proto::CommandResult,
    ) -> Result<(), AppError> {
        let value = to_value(result)?;
        let status = if result.ok { "done" } else { "failed" };
        let n = sqlx::query!(
            "UPDATE agent_commands
             SET status = $2, completed_at = now(), result = $3,
                 command = setu_redact_command(command)
             WHERE id = $1 AND ($4::text IS NULL OR server_id = $4)
               AND status IN ('queued', 'delivered')",
            id,
            status,
            value,
            server_id,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            let exists = sqlx::query_scalar!(
                "SELECT id FROM agent_commands
                 WHERE id = $1 AND ($2::text IS NULL OR server_id = $2)",
                id,
                server_id,
            )
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)?;
            if exists.is_none() {
                return Err(AppError::not_found("command not found"));
            }
        }
        Ok(())
    }

    /// Cancel a `queued`/`delivered` command and redact it; the control
    /// plane never re-delivers it. Returns whether the row changed.
    pub async fn cancel(&self, id: &str) -> Result<bool, AppError> {
        let n = sqlx::query!(
            "UPDATE agent_commands
             SET status = 'cancelled', command = setu_redact_command(command)
             WHERE id = $1 AND status IN ('queued', 'delivered')",
            id,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n == 1)
    }

    /// Poll every 300 ms until the command is `done`/`failed` (the
    /// stored result), `cancelled`, `expired` (also when it is still
    /// `queued` past `expires_at`: it is marked expired here), or
    /// `timeout` passes.
    pub async fn wait(
        &self,
        id: &str,
        timeout: Duration,
    ) -> Result<setu_proto::CommandResult, WaitError> {
        let deadline = Instant::now() + timeout;
        loop {
            let row = sqlx::query!(
                "SELECT status, result, (expires_at <= now()) AS \"expired!\"
                 FROM agent_commands WHERE id = $1",
                id,
            )
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| AppError::not_found("command not found"))?;
            match row.status.as_str() {
                "done" | "failed" => {
                    let value = row.result.ok_or_else(AppError::internal)?;
                    return serde_json::from_value(value).map_err(|e| {
                        tracing::error!(error = %e, "stored command result is corrupt");
                        WaitError::Db(AppError::internal())
                    });
                }
                "cancelled" => return Err(WaitError::Cancelled),
                "expired" => return Err(WaitError::Expired),
                "queued" if row.expired => {
                    let n = sqlx::query!(
                        "UPDATE agent_commands
                         SET status = 'expired', command = setu_redact_command(command)
                         WHERE id = $1 AND status = 'queued'",
                        id,
                    )
                    .execute(self.pool)
                    .await
                    .map_err(map_db_error)?
                    .rows_affected();
                    if n == 1 {
                        return Err(WaitError::Expired);
                    }
                    continue; // delivered meanwhile: look again
                }
                _ => {}
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(WaitError::Timeout);
            }
            tokio::time::sleep((deadline - now).min(WAIT_POLL)).await;
        }
    }

    /// Requeue every `delivered` command of the server (its agent
    /// restarted: a new `boot_id`). Returns how many.
    pub async fn requeue_delivered(&self, server_id: &str) -> Result<u64, AppError> {
        Ok(sqlx::query!(
            "UPDATE agent_commands SET status = 'queued', delivered_at = NULL
             WHERE server_id = $1 AND status = 'delivered'",
            server_id,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected())
    }

    /// Delete terminal rows created more than `older_than` ago; as a
    /// safety net, any non-terminal row older than 24 h (far past every
    /// TTL) is marked `expired` and redacted. Returns rows deleted.
    pub async fn prune(&self, older_than: Duration) -> Result<u64, AppError> {
        sqlx::query!(
            "UPDATE agent_commands
             SET status = 'expired', command = setu_redact_command(command)
             WHERE status IN ('queued', 'delivered')
               AND created_at < now() - make_interval(secs => $1)",
            STALE_SECS,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(sqlx::query!(
            "DELETE FROM agent_commands
             WHERE status IN ('done', 'failed', 'cancelled', 'expired')
               AND created_at < now() - make_interval(secs => $1)",
            crate::repo::secs(older_than),
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected())
    }

    /// One stored row (tests and diagnostics).
    pub async fn get(&self, id: &str) -> Result<AgentCommand, AppError> {
        sqlx::query_as!(
            AgentCommand,
            "SELECT id, server_id, site_short_id, command, created_at, expires_at,
                    delivered_at, completed_at, result, status, attempts
             FROM agent_commands WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("command not found"))
    }
}
