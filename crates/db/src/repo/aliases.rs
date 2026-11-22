//! `site_aliases` repo (0.1.7, issue #18): the extra hostnames of a site and
//! how each answers (`redirect` to the canonical domain, or `serve`).
//! Runtime-checked queries, so the offline `.sqlx` data does not change.

use sqlx::{PgPool, Row};

use crate::repo::basic_auth::ROUTE_JOB;
use crate::repo::jobs::{self, Job};
use crate::{map_db_error, AppError};

/// Most aliases one site may have.
pub const MAX_ALIASES: usize = 10;

/// Transaction-scoped advisory lock taken by every write that claims a
/// hostname (alias `PUT`, site create, turning `www_redirect` on), so two
/// sites cannot claim one name through checks that each pass alone.
const HOSTS_LOCK: i64 = 0x5E70_0A11_A5E5;

/// Takes [`HOSTS_LOCK`] for the rest of `tx`.
pub(crate) async fn lock_hosts(tx: &mut sqlx::PgConnection) -> Result<(), AppError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(HOSTS_LOCK)
        .execute(tx)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// One alias row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SiteAlias {
    pub host: String,
    /// `redirect` or `serve`.
    pub mode: String,
}

pub struct Aliases<'a> {
    pool: &'a PgPool,
}

impl<'a> Aliases<'a> {
    pub fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// The site's aliases, ordered by host.
    pub async fn list(&self, site_id: &str) -> Result<Vec<SiteAlias>, AppError> {
        let rows =
            sqlx::query("SELECT host, mode FROM site_aliases WHERE site_id = $1 ORDER BY host")
                .bind(site_id)
                .fetch_all(self.pool)
                .await
                .map_err(map_db_error)?;
        Ok(rows
            .iter()
            .map(|r| SiteAlias {
                host: r.get("host"),
                mode: r.get("mode"),
            })
            .collect())
    }

    /// Replaces the site's aliases and queues the [`ROUTE_JOB`] that applies
    /// them, in one transaction. `conflict` when a host is another site's
    /// domain, alias or automatic `www.` name (`www_redirect` on), or the
    /// site has an active job: nothing changes then. The active-job check
    /// is the insert of the job itself: `jobs_one_active_per_site_idx`
    /// makes a second concurrent writer fail with `conflict`, and its
    /// transaction (the delete and inserts above) rolls back.
    pub async fn replace_with_route_job(
        &self,
        site_id: &str,
        server_id: &str,
        aliases: &[SiteAlias],
    ) -> Result<Job, AppError> {
        if aliases.len() > MAX_ALIASES {
            return Err(AppError::validation(format!(
                "at most {MAX_ALIASES} aliases per site"
            )));
        }
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        lock_hosts(&mut tx).await?;
        sqlx::query("DELETE FROM site_aliases WHERE site_id = $1")
            .bind(site_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        for a in aliases {
            let taken: Option<String> =
                sqlx::query_scalar("SELECT id FROM sites WHERE domain = $1")
                    .bind(&a.host)
                    .fetch_optional(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
            if taken.is_some() {
                return Err(AppError::conflict(format!(
                    "{} is already the domain of a site",
                    a.host
                )));
            }
            // `www.<domain>` of a site that redirects its www name is taken.
            let www_of = a.host.strip_prefix("www.").unwrap_or("");
            let www_taken: Option<String> = sqlx::query_scalar(
                "SELECT id FROM sites WHERE domain = $1 AND www_redirect AND id <> $2",
            )
            .bind(www_of)
            .bind(site_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;
            if www_taken.is_some() {
                return Err(AppError::conflict(format!(
                    "{} is the www redirect of another site",
                    a.host
                )));
            }
            sqlx::query("INSERT INTO site_aliases (site_id, host, mode) VALUES ($1, $2, $3)")
                .bind(site_id)
                .bind(&a.host)
                .bind(&a.mode)
                .execute(&mut *tx)
                .await
                .map_err(|e| match &e {
                    sqlx::Error::Database(d) if d.is_unique_violation() => {
                        AppError::conflict(format!("{} is already an alias", a.host))
                    }
                    _ => map_db_error(e),
                })?;
        }
        let job = jobs::insert(&mut *tx, ROUTE_JOB, Some(site_id), Some(server_id), None).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(job)
    }
}
