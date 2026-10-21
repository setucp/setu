//! Followed manifest URLs (0.1.5 §1): an app published from a URL is
//! fetched again every [`CHECK_EVERY`] and any version not yet published
//! is published. A check never replaces a published version: a manifest
//! names binaries and their checksums, and an unattended re-publish must
//! not swap what a running version installs.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use setu_common::AppError;
use setu_db::{AppSource, Db};
use setu_jobs::Clock;

use crate::auth::audit;

/// How often every followed app is checked.
pub const CHECK_EVERY: Duration = Duration::from_hours(6);

/// Fetches a manifest by URL; the real one applies the publish guards.
#[async_trait]
pub trait ManifestFetcher: Send + Sync + std::fmt::Debug {
    async fn fetch(&self, url: &str) -> Result<String, AppError>;
}

/// The guarded HTTP fetch `POST /api/v1/apps` uses.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpFetcher;

#[async_trait]
impl ManifestFetcher for HttpFetcher {
    async fn fetch(&self, url: &str) -> Result<String, AppError> {
        crate::rest::apps::fetch_manifest_url(url).await
    }
}

/// Fetch `source.url` once and publish its version when it is new.
/// Returns the version published, or `None` when it was already
/// published. The outcome is recorded on the source either way.
pub async fn check(
    db: &Db,
    fetcher: &dyn ManifestFetcher,
    clock: &dyn Clock,
    source: &AppSource,
) -> Result<Option<String>, AppError> {
    let outcome = check_inner(db, fetcher, source).await;
    let error = outcome.as_ref().err().map(ToString::to_string);
    db.app_sources()
        .record_check(&source.app_id, clock.now(), error.as_deref())
        .await?;
    outcome
}

async fn check_inner(
    db: &Db,
    fetcher: &dyn ManifestFetcher,
    source: &AppSource,
) -> Result<Option<String>, AppError> {
    let src = fetcher.fetch(&source.url).await?;
    let manifest = crate::rest::apps::parse_or_validation(&src)?;
    if manifest.app.id != source.app_id {
        return Err(AppError::validation(format!(
            "manifest is for app {}, not {}",
            manifest.app.id, source.app_id
        )));
    }
    match db
        .apps()
        .version(&source.app_id, &manifest.app.version)
        .await
    {
        Ok(_) => return Ok(None),
        Err(e) if e.code == setu_common::ErrorCode::NotFound => {}
        Err(e) => return Err(e),
    }
    let json = serde_json::to_value(&manifest).map_err(|e| {
        tracing::error!(error = %e, "manifest re-serialisation failed");
        AppError::internal()
    })?;
    let row = db.apps().upsert_version(&json).await?;
    audit(
        db,
        None,
        "app.publish",
        Some(&format!("{}/{} (followed)", row.app_id, row.version)),
        None,
    )
    .await;
    tracing::info!(app = %row.app_id, version = %row.version, "followed URL published a new version");
    Ok(Some(row.version))
}

/// Check every followed app now, one after another.
pub async fn check_all(db: &Db, fetcher: &dyn ManifestFetcher, clock: &dyn Clock) {
    let sources = match db.app_sources().list().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "app sources unreadable");
            return;
        }
    };
    for source in &sources {
        if let Err(e) = check(db, fetcher, clock, source).await {
            tracing::warn!(app = %source.app_id, error = %e, "followed URL check failed");
        }
    }
}

/// The background loop: a round every [`CHECK_EVERY`], the first one
/// `first_after` after start.
pub fn spawn(
    db: Db,
    fetcher: Arc<dyn ManifestFetcher>,
    clock: Arc<dyn Clock>,
    first_after: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut every =
            tokio::time::interval_at(tokio::time::Instant::now() + first_after, CHECK_EVERY);
        every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            every.tick().await;
            check_all(&db, fetcher.as_ref(), clock.as_ref()).await;
        }
    })
}
