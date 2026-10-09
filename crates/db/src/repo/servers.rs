//! `servers` repo: agent hosts, join tokens and heartbeats.
//!
//! Tokens are 32 random bytes hex-encoded with a kind prefix
//! (`setu_join_…`, `setu_srv_…`); only their SHA-256 hex is stored. The
//! join is single use (1 h) with an agent nonce: the first use stores
//! the nonce and keeps the issued server token sealed for 60 s, so a
//! retry with the same nonce gets the same token while a different
//! nonce is refused (spec §5).

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// A server counts as `online` when seen within this many seconds.
pub const ONLINE_WINDOW_SECS: i64 = 60;
/// Join tokens are valid this long after `create_pending`.
pub const JOIN_TOKEN_TTL_SECS: f64 = 3600.0;
/// The sealed server token is kept this long after the first join.
pub const JOIN_RETRY_WINDOW_SECS: i64 = 60;

/// Computed status (never stored).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerStatus {
    Pending,
    Online,
    Offline,
}

impl ServerStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Online => "online",
            Self::Offline => "offline",
        }
    }
}

/// One managed server. Holds token material: `Debug` redacts it.
#[derive(Clone)]
pub struct Server {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub hostname: Option<String>,
    pub public_ip: Option<String>,
    pub token_hash: Option<String>,
    pub join_token_hash: Option<String>,
    pub join_expires_at: Option<DateTime<Utc>>,
    pub join_nonce: Option<String>,
    pub join_token_sealed: Option<String>,
    pub joined_at: Option<DateTime<Utc>>,
    pub agent_version: Option<String>,
    pub boot_id: Option<String>,
    pub arch: Option<String>,
    pub os: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_mb: Option<i64>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub cordoned: bool,
    pub is_control_plane: bool,
    pub created_at: DateTime<Utc>,
}

impl Server {
    /// `pending` (never joined), `online` (seen within 60 s), `offline`.
    pub fn status(&self, now: DateTime<Utc>) -> ServerStatus {
        if self.joined_at.is_none() {
            return ServerStatus::Pending;
        }
        match self.last_seen_at {
            Some(seen) if (now - seen).num_seconds() <= ONLINE_WINDOW_SECS => ServerStatus::Online,
            _ => ServerStatus::Offline,
        }
    }
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |o: &Option<String>| o.as_ref().map(|_| "[redacted]");
        f.debug_struct("Server")
            .field("id", &self.id)
            .field("short_id", &self.short_id)
            .field("name", &self.name)
            .field("hostname", &self.hostname)
            .field("token_hash", &redact(&self.token_hash))
            .field("join_token_hash", &redact(&self.join_token_hash))
            .field("join_expires_at", &self.join_expires_at)
            .field("join_nonce", &redact(&self.join_nonce))
            .field("join_token_sealed", &redact(&self.join_token_sealed))
            .field("joined_at", &self.joined_at)
            .field("agent_version", &self.agent_version)
            .field("arch", &self.arch)
            .field("os", &self.os)
            .field("last_seen_at", &self.last_seen_at)
            .field("cordoned", &self.cordoned)
            .field("is_control_plane", &self.is_control_plane)
            .finish()
    }
}

/// What the agent reports when it joins (the db-side input; the API
/// maps proto's `JoinRequest`, nonce included, ledger R76).
#[derive(Clone)]
pub struct JoinInput {
    pub nonce: String,
    pub hostname: String,
    pub public_ip: Option<String>,
    pub arch: String,
    pub os: String,
    pub cpu_cores: i32,
    pub memory_mb: i64,
    pub disk_mb: i64,
}

impl std::fmt::Debug for JoinInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JoinInput")
            .field("nonce", &"[redacted]")
            .field("hostname", &self.hostname)
            .field("arch", &self.arch)
            .field("os", &self.os)
            .field("cpu_cores", &self.cpu_cores)
            .field("memory_mb", &self.memory_mb)
            .field("disk_mb", &self.disk_mb)
            .finish()
    }
}

/// Why a join was refused. The API answers all three with 401 and one
/// message; the distinction is for logs and tests.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    #[error("unknown join token")]
    Unknown,
    #[error("join token expired")]
    Expired,
    #[error("join nonce mismatch")]
    NonceMismatch,
    #[error(transparent)]
    Db(#[from] AppError),
}

impl From<JoinError> for AppError {
    fn from(e: JoinError) -> Self {
        match e {
            JoinError::Db(e) => e,
            _ => AppError::unauthorized("invalid or expired join token"),
        }
    }
}

/// Heartbeat fields stored on the server row; metrics go to
/// `site_metrics` in the same transaction.
#[derive(Debug, Clone, Copy)]
pub struct HeartbeatInput<'h> {
    pub agent_version: &'h str,
    pub boot_id: Option<&'h str>,
    pub metrics: &'h [setu_proto::SiteMetric],
}

/// Result of a heartbeat: `boot_changed` means the agent restarted (or
/// rebooted) since the last heartbeat, so its delivered commands must
/// be requeued (spec §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeartbeatOutcome {
    pub boot_changed: bool,
}

/// Repository accessor; build via [`crate::Db::servers`].
pub struct Servers<'a> {
    pool: &'a PgPool,
}

fn token(prefix: &str) -> String {
    format!("{prefix}{}", hex::encode(crate::repo::random_32()))
}

pub(crate) fn hash_token(plain: &str) -> String {
    crate::repo::sha256_hex(plain.as_bytes())
}

impl<'a> Servers<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Register a pending server and mint its single-use join token
    /// (valid 1 hour). Returns the row plus the plaintext token: the
    /// only time the plaintext exists.
    pub async fn create_pending(
        &self,
        name: &str,
        is_control_plane: bool,
    ) -> Result<(Server, String), AppError> {
        let plain = token("setu_join_");
        for _ in 0..8 {
            let res = sqlx::query_as!(
                Server,
                "INSERT INTO servers
                     (id, short_id, name, join_token_hash, join_expires_at, is_control_plane)
                 VALUES ($1, $2, $3, $4, now() + make_interval(secs => $5), $6)
                 RETURNING id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at",
                setu_common::new_id(),
                crate::repo::random_short_id(),
                name,
                hash_token(&plain),
                JOIN_TOKEN_TTL_SECS,
                is_control_plane,
            )
            .fetch_one(self.pool)
            .await;
            match res {
                Ok(server) => return Ok((server, plain)),
                Err(sqlx::Error::Database(e)) if e.constraint() == Some("servers_short_id_key") => {
                    continue
                }
                Err(e) => return Err(map_db_error(e)),
            }
        }
        Err(AppError::internal())
    }

    /// Redeem a join token (see the module docs for the nonce rules).
    /// Returns the server and the plaintext server token.
    pub async fn join(
        &self,
        join_token_plain: &str,
        input: &JoinInput,
        sealer: &setu_common::Sealer,
    ) -> Result<(Server, String), JoinError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let row = sqlx::query!(
            "SELECT id, join_expires_at, join_nonce, join_token_sealed, joined_at,
                    (joined_at IS NOT NULL
                     AND joined_at > now() - make_interval(secs => $2)) AS \"in_window!\",
                    (join_expires_at IS NOT NULL AND join_expires_at > now()) AS \"valid!\"
             FROM servers WHERE join_token_hash = $1 FOR UPDATE",
            hash_token(join_token_plain),
            JOIN_RETRY_WINDOW_SECS as f64,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or(JoinError::Unknown)?;

        let Some(stored_nonce) = row.join_nonce else {
            // First use.
            if !row.valid {
                return Err(JoinError::Expired);
            }
            let server_token = token("setu_srv_");
            let server = sqlx::query_as!(
                Server,
                "UPDATE servers SET
                     token_hash = $2, join_nonce = $3, join_token_sealed = $4,
                     joined_at = now(), last_seen_at = now(),
                     hostname = $5, public_ip = $6, arch = $7, os = $8,
                     cpu_cores = $9, memory_mb = $10, disk_mb = $11
                 WHERE id = $1
                 RETURNING id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at",
                row.id,
                hash_token(&server_token),
                input.nonce,
                sealer.seal(&server_token),
                input.hostname,
                input.public_ip,
                input.arch,
                input.os,
                input.cpu_cores,
                input.memory_mb,
                input.disk_mb,
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            tx.commit().await.map_err(map_db_error)?;
            return Ok((server, server_token));
        };

        if stored_nonce != input.nonce {
            return Err(JoinError::NonceMismatch);
        }
        let sealed = match (row.in_window, row.join_token_sealed) {
            (true, Some(sealed)) => sealed,
            (_, sealed) => {
                if sealed.is_some() {
                    sqlx::query!(
                        "UPDATE servers SET join_token_sealed = NULL WHERE id = $1",
                        row.id
                    )
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
                    tx.commit().await.map_err(map_db_error)?;
                }
                return Err(JoinError::Expired);
            }
        };
        let server_token = sealer.open(&sealed).map_err(|e| {
            tracing::error!(error = %e, "sealed join token failed to open");
            JoinError::Db(AppError::internal())
        })?;
        let server = sqlx::query_as!(
            Server,
            "SELECT id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at FROM servers WHERE id = $1",
            row.id,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok((server, server_token))
    }

    /// Authenticate an agent request by its server token.
    pub async fn by_token(&self, server_token_plain: &str) -> Result<Option<Server>, AppError> {
        sqlx::query_as!(
            Server,
            "SELECT id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at FROM servers WHERE token_hash = $1",
            hash_token(server_token_plain),
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn get(&self, id: &str) -> Result<Server, AppError> {
        sqlx::query_as!(
            Server,
            "SELECT id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at FROM servers WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("server not found"))
    }

    pub async fn list(&self) -> Result<Vec<Server>, AppError> {
        sqlx::query_as!(
            Server,
            "SELECT id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at FROM servers ORDER BY name, id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// Record a heartbeat: `last_seen_at`, `agent_version`, `boot_id`,
    /// and the metrics batch, in one transaction. Metrics for sites not
    /// on this server are ignored, so a compromised agent cannot inject
    /// another server's metrics. The sealed join token is dropped once
    /// its retry window has passed.
    pub async fn heartbeat(
        &self,
        id: &str,
        hb: &HeartbeatInput<'_>,
    ) -> Result<HeartbeatOutcome, AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let previous = sqlx::query!("SELECT boot_id FROM servers WHERE id = $1 FOR UPDATE", id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| AppError::not_found("server not found"))?;
        sqlx::query!(
            "UPDATE servers SET
                 last_seen_at = now(), agent_version = $2,
                 boot_id = COALESCE($3, boot_id),
                 join_token_sealed = CASE
                     WHEN joined_at < now() - make_interval(secs => $4) THEN NULL
                     ELSE join_token_sealed END
             WHERE id = $1",
            id,
            hb.agent_version,
            hb.boot_id,
            JOIN_RETRY_WINDOW_SECS as f64,
        )
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        crate::repo::metrics::insert_metrics_tx(&mut tx, id, hb.metrics).await?;
        tx.commit().await.map_err(map_db_error)?;
        let boot_changed = match (previous.boot_id.as_deref(), hb.boot_id) {
            (_, None) => false,
            (old, Some(new)) => old != Some(new),
        };
        Ok(HeartbeatOutcome { boot_changed })
    }

    pub async fn set_cordoned(&self, id: &str, cordoned: bool) -> Result<Server, AppError> {
        sqlx::query_as!(
            Server,
            "UPDATE servers SET cordoned = $2 WHERE id = $1 RETURNING id, short_id, name, hostname, public_ip, token_hash, join_token_hash,
                    join_expires_at, join_nonce, join_token_sealed, joined_at, agent_version,
                    boot_id, arch, os, cpu_cores, memory_mb, disk_mb, last_seen_at, cordoned,
                    is_control_plane, created_at",
            id,
            cordoned,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("server not found"))
    }

    /// Delete a server row; `conflict` while any site references it.
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let exists = sqlx::query_scalar!("SELECT id FROM servers WHERE id = $1 FOR UPDATE", id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;
        if exists.is_none() {
            return Err(AppError::not_found("server not found"));
        }
        let sites: Option<i64> =
            sqlx::query_scalar!("SELECT COUNT(*) FROM sites WHERE server_id = $1", id)
                .fetch_one(&mut *tx)
                .await
                .map_err(map_db_error)?;
        if sites.unwrap_or(0) > 0 {
            return Err(AppError::conflict("server still has sites"));
        }
        sqlx::query!("DELETE FROM servers WHERE id = $1", id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(())
    }
}
