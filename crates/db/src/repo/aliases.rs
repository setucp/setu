//! `site_aliases` repo (0.1.7, issue #18): the extra hostnames of a site and
//! how each answers (`redirect` to the canonical domain, or `serve`).
//! Runtime-checked queries, so the offline `.sqlx` data does not change.

use sqlx::{PgPool, Row};

use crate::repo::basic_auth::ROUTE_JOB;
use crate::repo::jobs::{self, Job};
use crate::{map_db_error, AppError};

/// Most aliases one site may have.
pub const MAX_ALIASES: usize = 10;

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
    /// domain or alias, or the site has an active job: nothing changes then.
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
