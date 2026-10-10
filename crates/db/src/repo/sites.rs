//! `sites` repo: provisioned app instances. `create` allocates the
//! server, port and `short_id` in one transaction (spec §3/§6).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

/// Lifecycle states; anything else is rejected with `validation`.
pub const SITE_STATUSES: &[&str] = &[
    "provisioning",
    "running",
    "stopped",
    "failed",
    "updating",
    "deleting",
];

/// Site ports: 20000–29999 inclusive.
pub const PORT_MIN: i32 = 20000;
pub const PORT_MAX: i32 = 29999;
/// Capacity overheads (MB): per site, plus the control plane's own.
pub const SITE_OVERHEAD_MB: i64 = 256;
pub const CONTROL_PLANE_OVERHEAD_MB: i64 = 512;
/// Plan memory may fill at most this percentage of a server's memory.
pub const CAPACITY_PERCENT: i64 = 85;

/// One site. `env` holds sealed values: `Debug` redacts it.
#[derive(Clone)]
pub struct Site {
    pub id: String,
    pub short_id: String,
    pub name: String,
    pub app_id: String,
    pub app_version: String,
    pub server_id: String,
    pub plan_id: String,
    pub domain: String,
    pub status: String,
    pub failed_step: Option<String>,
    pub port: i32,
    pub db_name: String,
    pub db_role: String,
    pub env: Value,
    pub owner_email: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[allow(clippy::missing_fields_in_debug)] // reviewed: kept as-is (R150)
impl std::fmt::Debug for Site {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Site")
            .field("id", &self.id)
            .field("short_id", &self.short_id)
            .field("name", &self.name)
            .field("app_id", &self.app_id)
            .field("app_version", &self.app_version)
            .field("server_id", &self.server_id)
            .field("plan_id", &self.plan_id)
            .field("domain", &self.domain)
            .field("status", &self.status)
            .field("failed_step", &self.failed_step)
            .field("port", &self.port)
            .field("db_name", &self.db_name)
            .field("env", &"[redacted]")
            .field("owner_email", &self.owner_email)
            .finish()
    }
}

/// A site to create (status starts `provisioning`). `env_sealed` must
/// already be sealed with the panel `Sealer`. `server_id: None` picks
/// the online, uncordoned server with the most free memory.
pub struct NewSite {
    pub name: String,
    pub app_id: String,
    pub app_version: String,
    pub server_id: Option<String>,
    pub plan_id: String,
    pub domain: String,
    pub owner_email: String,
    pub env_sealed: Value,
    /// The plan's memory (MB) counted for this site's admission.
    pub plan_memory_mb: i64,
}

impl std::fmt::Debug for NewSite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NewSite")
            .field("name", &self.name)
            .field("app_id", &self.app_id)
            .field("app_version", &self.app_version)
            .field("server_id", &self.server_id)
            .field("plan_id", &self.plan_id)
            .field("domain", &self.domain)
            .field("env_sealed", &"[redacted]")
            .field("owner_email", &self.owner_email)
            .field("plan_memory_mb", &self.plan_memory_mb)
            .finish()
    }
}

/// What `create` allocated, handed to the env builder of
/// [`Sites::create_with`] before the row is inserted (spec §3: the env
/// is generated exactly once, at creation, from the real allocation).
#[derive(Debug, Clone)]
pub struct Allocation {
    pub server_id: String,
    pub short_id: String,
    pub port: i32,
    pub db_name: String,
    pub db_role: String,
}

/// Why `create` refused.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    #[error("server has insufficient capacity")]
    NoCapacity,
    #[error("server is offline")]
    ServerOffline,
    #[error("server is cordoned")]
    Cordoned,
    #[error("domain already in use")]
    DomainTaken,
    #[error("no online server available")]
    NoServer,
    #[error(transparent)]
    Db(#[from] AppError),
}

impl From<CreateError> for AppError {
    fn from(e: CreateError) -> Self {
        match e {
            CreateError::NoCapacity => AppError::conflict("server has insufficient capacity"),
            CreateError::ServerOffline => AppError::validation("server is offline"),
            CreateError::Cordoned => AppError::validation("server is cordoned"),
            CreateError::DomainTaken => AppError::conflict("domain already in use"),
            CreateError::NoServer => AppError::conflict("no online server available"),
            CreateError::Db(e) => e,
        }
    }
}

fn db_err(e: sqlx::Error) -> CreateError {
    CreateError::Db(map_db_error(e))
}

/// Capacity rule: other sites' plan memory + this site + 256 MB
/// (+ 512 MB on the control plane's server) ≤ 85 % of memory.
fn fits(memory_mb: Option<i64>, used_mb: i64, need_mb: i64, is_control_plane: bool) -> bool {
    let Some(mem) = memory_mb.filter(|m| *m > 0) else {
        return false;
    };
    let cp = if is_control_plane {
        CONTROL_PLANE_OVERHEAD_MB
    } else {
        0
    };
    (used_mb + need_mb + SITE_OVERHEAD_MB + cp) * 100 <= CAPACITY_PERCENT * mem
}

/// Memory left under the 85 % line (overheads excluded), for ranking.
fn free_mb(memory_mb: Option<i64>, used_mb: i64, is_control_plane: bool) -> i64 {
    let cp = if is_control_plane {
        CONTROL_PLANE_OVERHEAD_MB
    } else {
        0
    };
    memory_mb.unwrap_or(0) * CAPACITY_PERCENT / 100 - used_mb - cp
}

/// Repository accessor; build via [`crate::Db::sites`].
pub struct Sites<'a> {
    pool: &'a PgPool,
}

struct LockedServer {
    id: String,
    memory_mb: Option<i64>,
    cordoned: bool,
    is_control_plane: bool,
    online: bool,
}

impl<'a> Sites<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Create a site in one transaction: pick the server (given, or the
    /// online uncordoned one with the most free memory), lock its row,
    /// apply the capacity rule, take the lowest free port and a fresh
    /// `short_id` unique on that server, insert. `db_name` and
    /// `db_role` are `site_<short_id>`.
    pub async fn create(&self, new: &NewSite) -> Result<Site, CreateError> {
        self.create_with(new, |_| Ok(new.env_sealed.clone())).await
    }

    /// [`create`](Self::create) whose stored env is built by `env` from
    /// the allocation (short id, port, database names) inside the same
    /// transaction; `new.env_sealed` is ignored. The builder must return
    /// the SEALED env; its error aborts the create.
    #[allow(
        clippy::cast_precision_loss,
        clippy::single_match_else,
        clippy::too_many_lines
    )] // reviewed: kept as-is (R150)
    pub async fn create_with<F>(&self, new: &NewSite, env: F) -> Result<Site, CreateError>
    where
        F: FnOnce(&Allocation) -> Result<Value, AppError>,
    {
        let mut tx = self.pool.begin().await.map_err(db_err)?;

        let taken: Option<String> =
            sqlx::query_scalar!("SELECT id FROM sites WHERE domain = $1", new.domain)
                .fetch_optional(&mut *tx)
                .await
                .map_err(db_err)?;
        if taken.is_some() {
            return Err(CreateError::DomainTaken);
        }

        let server_id = match &new.server_id {
            Some(id) => id.clone(),
            None => {
                // Candidates by free memory, unlocked; the chosen row is
                // locked and re-checked below.
                let candidates = sqlx::query!(
                    "SELECT s.id, s.memory_mb, s.is_control_plane,
                            COALESCE((SELECT SUM(p.memory_mb) FROM sites x
                                      JOIN plans p ON p.id = x.plan_id
                                      WHERE x.server_id = s.id), 0)::BIGINT AS \"used!\"
                     FROM servers s
                     WHERE s.joined_at IS NOT NULL AND NOT s.cordoned
                       AND s.last_seen_at > now() - make_interval(secs => $1)
                     ORDER BY s.id",
                    crate::repo::servers::ONLINE_WINDOW_SECS as f64,
                )
                .fetch_all(&mut *tx)
                .await
                .map_err(db_err)?;
                if candidates.is_empty() {
                    return Err(CreateError::NoServer);
                }
                candidates
                    .into_iter()
                    .filter(|c| fits(c.memory_mb, c.used, new.plan_memory_mb, c.is_control_plane))
                    .max_by_key(|c| free_mb(c.memory_mb, c.used, c.is_control_plane))
                    .map(|c| c.id)
                    .ok_or(CreateError::NoCapacity)?
            }
        };

        let server = sqlx::query_as!(
            LockedServer,
            "SELECT id, memory_mb, cordoned, is_control_plane,
                    (joined_at IS NOT NULL AND last_seen_at IS NOT NULL
                     AND last_seen_at > now() - make_interval(secs => $2)) AS \"online!\"
             FROM servers WHERE id = $1 FOR UPDATE",
            server_id,
            crate::repo::servers::ONLINE_WINDOW_SECS as f64,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(db_err)?
        .ok_or(CreateError::NoServer)?;
        if server.cordoned {
            return Err(CreateError::Cordoned);
        }
        if !server.online {
            return Err(CreateError::ServerOffline);
        }
        let used: i64 = sqlx::query_scalar!(
            "SELECT COALESCE(SUM(p.memory_mb), 0)::BIGINT AS \"used!\"
             FROM sites s JOIN plans p ON p.id = s.plan_id
             WHERE s.server_id = $1",
            server.id,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(db_err)?;
        if !fits(
            server.memory_mb,
            used,
            new.plan_memory_mb,
            server.is_control_plane,
        ) {
            return Err(CreateError::NoCapacity);
        }

        let rows = sqlx::query!(
            "SELECT port, short_id FROM sites WHERE server_id = $1",
            server.id,
        )
        .fetch_all(&mut *tx)
        .await
        .map_err(db_err)?;
        let port = (PORT_MIN..=PORT_MAX)
            .find(|p| !rows.iter().any(|r| r.port == *p))
            .ok_or(CreateError::NoCapacity)?;
        // The server row is locked, so no concurrent create picks for
        // this server; UNIQUE(server_id, short_id) is the backstop.
        let short_id = loop {
            let candidate = crate::repo::random_short_id();
            if !rows.iter().any(|r| r.short_id == candidate) {
                break candidate;
            }
        };
        let db_name = format!("site_{short_id}");
        let env_sealed = env(&Allocation {
            server_id: server.id.clone(),
            short_id: short_id.clone(),
            port,
            db_name: db_name.clone(),
            db_role: db_name.clone(),
        })?;

        let site = sqlx::query_as!(
            Site,
            "INSERT INTO sites
                 (id, short_id, name, app_id, app_version, server_id, plan_id, domain,
                  status, port, db_name, db_role, env, owner_email)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'provisioning', $9, $10, $10, $11, $12)
             RETURNING id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at",
            setu_common::new_id(),
            short_id,
            new.name,
            new.app_id,
            new.app_version,
            server.id,
            new.plan_id,
            new.domain,
            port,
            db_name,
            env_sealed,
            new.owner_email,
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("sites_domain_key") => {
                CreateError::DomainTaken
            }
            _ => db_err(e),
        })?;
        tx.commit().await.map_err(db_err)?;
        Ok(site)
    }

    pub async fn get(&self, id: &str) -> Result<Site, AppError> {
        sqlx::query_as!(Site, "SELECT id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at FROM sites WHERE id = $1", id)
            .fetch_optional(self.pool)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| AppError::not_found("site not found"))
    }

    pub async fn list(&self) -> Result<Vec<Site>, AppError> {
        sqlx::query_as!(
            Site,
            "SELECT id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at FROM sites ORDER BY created_at DESC, id DESC"
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn list_by_server(&self, server_id: &str) -> Result<Vec<Site>, AppError> {
        sqlx::query_as!(
            Site,
            "SELECT id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at FROM sites WHERE server_id = $1 ORDER BY created_at, id",
            server_id,
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    /// The site holding `domain`, other than `site_id` (pass `""` to
    /// exclude nothing).
    pub async fn by_domain_excluding(
        &self,
        domain: &str,
        site_id: &str,
    ) -> Result<Option<Site>, AppError> {
        sqlx::query_as!(
            Site,
            "SELECT id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at FROM sites WHERE domain = $1 AND id <> $2",
            domain,
            site_id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn set_status(
        &self,
        id: &str,
        status: &str,
        failed_step: Option<&str>,
    ) -> Result<Site, AppError> {
        if !SITE_STATUSES.contains(&status) {
            return Err(AppError::validation(format!(
                "unknown site status {status}"
            )));
        }
        sqlx::query_as!(
            Site,
            "UPDATE sites SET status = $2, failed_step = $3, updated_at = now()
             WHERE id = $1 RETURNING id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at",
            id,
            status,
            failed_step,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    pub async fn set_version(&self, id: &str, version: &str) -> Result<Site, AppError> {
        sqlx::query_as!(
            Site,
            "UPDATE sites SET app_version = $2, updated_at = now()
             WHERE id = $1 RETURNING id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at",
            id,
            version,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    /// Replace the sealed env.
    pub async fn set_env(&self, id: &str, env: &Value) -> Result<Site, AppError> {
        sqlx::query_as!(
            Site,
            "UPDATE sites SET env = $2, updated_at = now()
             WHERE id = $1 RETURNING id, short_id, name, app_id, app_version, server_id, plan_id, domain, status,
                       failed_step, port, db_name, db_role, env, owner_email, created_at, updated_at",
            id,
            env,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("site not found"))
    }

    /// Delete the row; metrics cascade, jobs and backups keep their rows
    /// with `site_id` nulled (0002, R142).
    pub async fn delete(&self, id: &str) -> Result<(), AppError> {
        let n = sqlx::query!("DELETE FROM sites WHERE id = $1", id)
            .execute(self.pool)
            .await
            .map_err(map_db_error)?
            .rows_affected();
        if n == 0 {
            return Err(AppError::not_found("site not found"));
        }
        Ok(())
    }
}
