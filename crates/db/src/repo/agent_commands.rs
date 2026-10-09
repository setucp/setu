//! `agent_commands` repo: the long-poll command queue (spec §4).
//!
//! Lifecycle per command: `enqueue` → `next_for` (marks `delivered_at`,
//! bumps `attempts`) → `complete` (sets `completed_at` + `result`).
//! Commands delivered but never completed (agent restarted mid-step)
//! become deliverable again via `requeue_stale`; agents are idempotent,
//! so redelivery is safe. `wait` lets `DbAgentClient` (Task 6) block
//! for a result with 500 ms polling.

use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Stored command row. `command`/`result` embed env values and secrets:
/// `Debug` redacts both.
pub struct AgentCommand {
    pub id: String,
    pub server_id: String,
    pub command: Value,
    pub created_at: DateTime<Utc>,
    pub delivered_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub result: Option<Value>,
    pub attempts: i32,
}

impl std::fmt::Debug for AgentCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentCommand")
            .field("id", &self.id)
            .field("server_id", &self.server_id)
            .field("command", &"[redacted]")
            .field("created_at", &self.created_at)
            .field("delivered_at", &self.delivered_at)
            .field("completed_at", &self.completed_at)
            .field("result", &"[redacted]")
            .field("attempts", &self.attempts)
            .finish()
    }
}

/// Repository accessor; build via [`crate::Db::agent_commands`].
pub struct AgentCommands<'a> {
    pool: &'a PgPool,
}

impl<'a> AgentCommands<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    pub async fn enqueue(
        &self,
        server_id: &str,
        command: &setu_proto::Command,
    ) -> Result<setu_proto::CommandEnvelope, AppError> {
        let id = setu_common::new_id();
        let created_at = Utc::now();
        let value = serde_json::to_value(command).map_err(|e| {
            tracing::error!(error = %e, "agent command is not serializable");
            AppError::internal()
        })?;
        sqlx::query!(
            "INSERT INTO agent_commands (id, server_id, command, created_at)
             VALUES ($1, $2, $3, $4)",
            id,
            server_id,
            value,
            created_at,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(setu_proto::CommandEnvelope {
            id,
            created_at,
            command: command.clone(),
            attempts: 0,
        })
    }

    /// Oldest command for the server that was never delivered (stale
    /// deliveries are reset to undelivered by `requeue_stale`, so this
    /// only looks at `delivered_at IS NULL`). Marks the row delivered and
    /// counts the delivery in `attempts`.
    pub async fn next_for(
        &self,
        server_id: &str,
    ) -> Result<Option<setu_proto::CommandEnvelope>, AppError> {
        sqlx::query_as!(
            DeliveredRow,
            "UPDATE agent_commands
             SET delivered_at = now(), attempts = attempts + 1
             WHERE id = (
                 SELECT id FROM agent_commands
                 WHERE server_id = $1
                   AND delivered_at IS NULL
                   AND completed_at IS NULL
                 ORDER BY created_at, id
                 FOR UPDATE SKIP LOCKED
                 LIMIT 1
             )
              RETURNING id, command, created_at, attempts",
            server_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_envelope())
        .transpose()
    }

    pub async fn complete(
        &self,
        id: &str,
        result: &setu_proto::CommandResult,
    ) -> Result<(), AppError> {
        let value = serde_json::to_value(result).map_err(|e| {
            tracing::error!(error = %e, "command result is not serializable");
            AppError::internal()
        })?;
        // Idempotence ruling: `AND completed_at IS NULL` makes the first
        // writer win; a repeat post for an already-completed command is an
        // ok no-op (no overwrite), while a truly unknown id is 404. The
        // follow-up SELECT distinguishes the two.
        let n = sqlx::query!(
            "UPDATE agent_commands SET completed_at = now(), result = $2
              WHERE id = $1 AND completed_at IS NULL",
            id,
            value,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            let exists = sqlx::query!("SELECT id FROM agent_commands WHERE id = $1", id)
                .fetch_optional(self.pool)
                .await
                .map_err(map_db_error)?;
            if exists.is_some() {
                return Ok(());
            }
            return Err(AppError::not_found("command not found"));
        }
        Ok(())
    }

    /// Scoped completion for the agent protocol: only the server the
    /// command was enqueued for may complete it (`WHERE id = $1 AND
    /// server_id = $2`). A command owned by another server — or an unknown
    /// id — is 404, so server A cannot complete server B's commands.
    /// Idempotence matches [`Self::complete`]: the first writer wins via
    /// `AND completed_at IS NULL`; a repeat post is an ok no-op.
    pub async fn complete_for(
        &self,
        server_id: &str,
        id: &str,
        result: &setu_proto::CommandResult,
    ) -> Result<(), AppError> {
        let value = serde_json::to_value(result).map_err(|e| {
            tracing::error!(error = %e, "command result is not serializable");
            AppError::internal()
        })?;
        let n = sqlx::query!(
            "UPDATE agent_commands SET completed_at = now(), result = $2
              WHERE id = $1 AND server_id = $3 AND completed_at IS NULL",
            id,
            value,
            server_id,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n == 0 {
            let exists = sqlx::query!(
                "SELECT id FROM agent_commands WHERE id = $1 AND server_id = $2",
                id,
                server_id,
            )
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)?;
            if exists.is_some() {
                return Ok(());
            }
            return Err(AppError::not_found("command not found"));
        }
        Ok(())
    }

    /// Poll every 500 ms until the command completes. Times out as
    /// `agent_unreachable` — the caller (Task 6 runner) then lets
    /// `requeue_stale` redeliver it.
    pub async fn wait(
        &self,
        id: &str,
        timeout: Duration,
    ) -> Result<setu_proto::CommandResult, AppError> {
        let deadline = Instant::now() + timeout;
        loop {
            let row = sqlx::query!(
                "SELECT completed_at, result FROM agent_commands WHERE id = $1",
                id,
            )
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| AppError::not_found("command not found"))?;
            if row.completed_at.is_some() {
                let value = row.result.ok_or_else(AppError::internal)?;
                return serde_json::from_value(value).map_err(|e| {
                    tracing::error!(error = %e, "stored command result is corrupt");
                    AppError::internal()
                });
            }
            if Instant::now() >= deadline {
                return Err(AppError::agent_unreachable(format!(
                    "timed out waiting for command {id}"
                )));
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            tokio::time::sleep(remaining.min(Duration::from_millis(500))).await;
        }
    }

    /// Reset deliveries older than `older_than` that never completed so
    /// `next_for` picks them up again. Only `delivered_at` is cleared:
    /// `attempts` already counts deliveries (bumped by `next_for`), so a
    /// command delivered once then requeued reads `attempts = 1` until its
    /// redelivery bumps it to 2. Returns the number reset.
    pub async fn requeue_stale(&self, older_than: Duration) -> Result<u64, AppError> {
        let cutoff = Utc::now()
            - chrono::Duration::from_std(older_than)
                .map_err(|_| AppError::validation("requeue window out of range"))?;
        let n = sqlx::query!(
            "UPDATE agent_commands
             SET delivered_at = NULL
             WHERE delivered_at IS NOT NULL
               AND completed_at IS NULL
               AND delivered_at < $1",
            cutoff,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n)
    }
}

#[derive(sqlx::FromRow)]
struct DeliveredRow {
    id: String,
    command: Value,
    created_at: DateTime<Utc>,
    attempts: i32,
}

impl DeliveredRow {
    fn into_envelope(self) -> Result<setu_proto::CommandEnvelope, AppError> {
        let command = serde_json::from_value(self.command).map_err(|e| {
            tracing::error!(error = %e, "stored agent command is corrupt");
            AppError::internal()
        })?;
        Ok(setu_proto::CommandEnvelope {
            id: self.id,
            created_at: self.created_at,
            command,
            attempts: self.attempts,
        })
    }
}
