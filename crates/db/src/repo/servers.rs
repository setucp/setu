//! `servers` repo: agent hosts, join tokens and heartbeats.
//!
//! Tokens are 32 random bytes hex-encoded with a kind prefix
//! (`setu_join_…`, `setu_srv_…`); only their sha256 hex is stored.

use chrono::{DateTime, Utc};
use rand::RngCore;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// One managed server. Holds token hashes: `Debug` redacts them.
pub struct Server {
    pub id: String,
    pub name: String,
    pub host: Option<String>,
    pub token_hash: Option<String>,
    pub join_token_hash: Option<String>,
    pub join_token_expires_at: Option<DateTime<Utc>>,
    pub join_token_used: bool,
    pub agent_version: Option<String>,
    pub arch: Option<String>,
    pub os: Option<String>,
    pub caddy_admin: bool,
    pub postgres_version: Option<String>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_mb: Option<i64>,
    pub load1: Option<f64>,
    pub mem_free_mb: Option<i64>,
    pub disk_free_mb: Option<i64>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field(
                "token_hash",
                &self.token_hash.as_ref().map(|_| "[redacted]"),
            )
            .field(
                "join_token_hash",
                &self.join_token_hash.as_ref().map(|_| "[redacted]"),
            )
            .field("join_token_expires_at", &self.join_token_expires_at)
            .field("join_token_used", &self.join_token_used)
            .field("agent_version", &self.agent_version)
            .field("arch", &self.arch)
            .field("os", &self.os)
            .field("status", &self.status)
            .field("last_seen_at", &self.last_seen_at)
            .finish()
    }
}

/// Repository accessor; build via [`crate::Db::servers`].
pub struct Servers<'a> {
    pool: &'a PgPool,
}

impl<'a> Servers<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Register a pending server and mint its single-use join token
    /// (valid 1 hour). Returns the row plus the plaintext token — the
    /// only time the plaintext exists.
    pub async fn create_pending(&self, name: &str) -> Result<(Server, String), AppError> {
        let plain = format!("setu_join_{}", rand_hex_32());
        let row = sqlx::query_as!(
            ServerRow,
            "INSERT INTO servers
                 (id, name, join_token_hash, join_token_expires_at, join_token_used, status)
             VALUES ($1, $2, $3, $4, FALSE, 'pending')
             RETURNING id, name, host, token_hash, join_token_hash,
                       join_token_expires_at, join_token_used, agent_version, arch, os,
                       caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                       disk_mb, load1, mem_free_mb, disk_free_mb, status,
                       created_at, updated_at",
            setu_common::new_id(),
            name,
            hash_token(&plain),
            Utc::now() + chrono::Duration::hours(1),
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)?;
        Ok((row.into_server(), plain))
    }

    /// Redeem a join token: single-use and expiry-enforced (the second
    /// UPDATE re-checks `join_token_used`, so two racing joins cannot
    /// both succeed). Marks the server live and returns it with the
    /// long-lived server token plaintext.
    pub async fn join(
        &self,
        join_token_plain: &str,
        req: &setu_proto::JoinRequest,
    ) -> Result<(Server, String), AppError> {
        let candidate = sqlx::query_as!(
            ServerRow,
            "SELECT id, name, host, token_hash, join_token_hash,
                    join_token_expires_at, join_token_used, agent_version, arch, os,
                    caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                    disk_mb, load1, mem_free_mb, disk_free_mb, status,
                    created_at, updated_at
             FROM servers
             WHERE join_token_hash = $1
               AND join_token_used = FALSE
               AND join_token_expires_at > now()",
            hash_token(join_token_plain),
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::unauthorized("invalid or expired join token"))?;

        let server_token = format!("setu_srv_{}", rand_hex_32());
        let row = sqlx::query_as!(
            ServerRow,
            "UPDATE servers SET
                 join_token_used = TRUE,
                 token_hash = $2,
                 host = $3, arch = $4, os = $5,
                 cpu_cores = $6, memory_mb = $7, disk_mb = $8,
                 status = 'online', last_seen_at = now(), updated_at = now()
             WHERE id = $1 AND join_token_used = FALSE
             RETURNING id, name, host, token_hash, join_token_hash,
                       join_token_expires_at, join_token_used, agent_version, arch, os,
                       caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                       disk_mb, load1, mem_free_mb, disk_free_mb, status,
                       created_at, updated_at",
            candidate.id,
            hash_token(&server_token),
            req.hostname,
            req.arch,
            req.os,
            req.cpu_cores as i32,
            req.memory_mb as i64,
            req.disk_mb as i64,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::unauthorized("invalid or expired join token"))?;
        Ok((row.into_server(), server_token))
    }

    /// Authenticate an agent request by its server token.
    pub async fn by_token(&self, server_token_plain: &str) -> Result<Option<Server>, AppError> {
        sqlx::query_as!(
            ServerRow,
            "SELECT id, name, host, token_hash, join_token_hash,
                    join_token_expires_at, join_token_used, agent_version, arch, os,
                    caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                    disk_mb, load1, mem_free_mb, disk_free_mb, status,
                    created_at, updated_at
             FROM servers WHERE token_hash = $1",
            hash_token(server_token_plain),
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.map(|r| r.into_server()))
    }

    pub async fn get(&self, id: &str) -> Result<Server, AppError> {
        sqlx::query_as!(
            ServerRow,
            "SELECT id, name, host, token_hash, join_token_hash,
                    join_token_expires_at, join_token_used, agent_version, arch, os,
                    caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                    disk_mb, load1, mem_free_mb, disk_free_mb, status,
                    created_at, updated_at
             FROM servers WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_server())
        .ok_or_else(|| AppError::not_found("server not found"))
    }

    /// Record a heartbeat: the server is `online`, capacity-adjacent
    /// fields refresh, and the metrics batch is upserted.
    pub async fn heartbeat(
        &self,
        id: &str,
        hb: &setu_proto::Heartbeat,
    ) -> Result<Server, AppError> {
        let row = sqlx::query_as!(
            ServerRow,
            "UPDATE servers SET
                 last_seen_at = now(), status = 'online',
                 agent_version = $2, load1 = $3,
                 mem_free_mb = $4, disk_free_mb = $5, updated_at = now()
             WHERE id = $1
             RETURNING id, name, host, token_hash, join_token_hash,
                       join_token_expires_at, join_token_used, agent_version, arch, os,
                       caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                       disk_mb, load1, mem_free_mb, disk_free_mb, status,
                       created_at, updated_at",
            id,
            hb.agent_version,
            hb.load1,
            hb.mem_free_mb as i64,
            hb.disk_free_mb as i64,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_server())
        .ok_or_else(|| AppError::not_found("server not found"))?;
        crate::repo::metrics::insert_metrics(self.pool, &hb.metrics).await?;
        Ok(row)
    }

    pub async fn list(&self) -> Result<Vec<Server>, AppError> {
        sqlx::query_as!(
            ServerRow,
            "SELECT id, name, host, token_hash, join_token_hash,
                    join_token_expires_at, join_token_used, agent_version, arch, os,
                    caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                    disk_mb, load1, mem_free_mb, disk_free_mb, status,
                    created_at, updated_at
             FROM servers ORDER BY name, id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_server()).collect())
    }

    /// Servers seen within the last 60 s (the API computes `offline`
    /// from `last_seen_at` at read time on top of this).
    pub async fn online(&self) -> Result<Vec<Server>, AppError> {
        sqlx::query_as!(
            ServerRow,
            "SELECT id, name, host, token_hash, join_token_hash,
                    join_token_expires_at, join_token_used, agent_version, arch, os,
                    caddy_admin, postgres_version, last_seen_at, cpu_cores, memory_mb,
                    disk_mb, load1, mem_free_mb, disk_free_mb, status,
                    created_at, updated_at
             FROM servers
             WHERE last_seen_at > now() - INTERVAL '60 seconds'
             ORDER BY name, id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_server()).collect())
    }
    /// Delete a server row. Callers check for dependent sites first and
    /// return `conflict` (the API refuses 409 while sites reference it).
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        let n = sqlx::query!("DELETE FROM servers WHERE id = $1", id)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?
            .rows_affected();
        if n == 0 {
            return Err(AppError::not_found("server not found"));
        }
        Ok(())
    }
}

pub(crate) fn hash_token(plain: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(plain.as_bytes()))
}

fn rand_hex_32() -> String {
    let mut bytes = [0u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    hex::encode(bytes)
}

#[derive(sqlx::FromRow)]
struct ServerRow {
    id: String,
    name: String,
    host: Option<String>,
    token_hash: Option<String>,
    join_token_hash: Option<String>,
    join_token_expires_at: Option<DateTime<Utc>>,
    join_token_used: bool,
    agent_version: Option<String>,
    arch: Option<String>,
    os: Option<String>,
    caddy_admin: bool,
    postgres_version: Option<String>,
    last_seen_at: Option<DateTime<Utc>>,
    cpu_cores: Option<i32>,
    memory_mb: Option<i64>,
    disk_mb: Option<i64>,
    load1: Option<f64>,
    mem_free_mb: Option<i64>,
    disk_free_mb: Option<i64>,
    status: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl ServerRow {
    fn into_server(self) -> Server {
        Server {
            id: self.id,
            name: self.name,
            host: self.host,
            token_hash: self.token_hash,
            join_token_hash: self.join_token_hash,
            join_token_expires_at: self.join_token_expires_at,
            join_token_used: self.join_token_used,
            agent_version: self.agent_version,
            arch: self.arch,
            os: self.os,
            caddy_admin: self.caddy_admin,
            postgres_version: self.postgres_version,
            last_seen_at: self.last_seen_at,
            cpu_cores: self.cpu_cores,
            memory_mb: self.memory_mb,
            disk_mb: self.disk_mb,
            load1: self.load1,
            mem_free_mb: self.mem_free_mb,
            disk_free_mb: self.disk_free_mb,
            status: self.status,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
