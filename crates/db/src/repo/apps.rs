//! `apps` repo: catalogue entries and their validated manifests.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

#[derive(Debug, Clone)]
pub struct App {
    pub id: String,
    pub name: String,
    pub homepage: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct AppVersion {
    pub id: String,
    pub app_id: String,
    pub version: String,
    pub manifest: Value,
    pub channel: String,
    pub published_at: DateTime<Utc>,
}

/// Repository accessor; build via [`crate::Db::apps`].
pub struct Apps<'a> {
    pool: &'a PgPool,
}

impl<'a> Apps<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Insert (or refresh) an app and one of its versions from a
    /// validated manifest JSON object (`{ app: { id, name, version,
    /// homepage? }, … }`). Re-publishing the same version replaces the
    /// stored manifest; the original `published_at` is kept.
    pub async fn upsert_version(&self, manifest: &Value) -> Result<AppVersion, AppError> {
        let app = manifest
            .get("app")
            .ok_or_else(|| AppError::validation("manifest.app missing"))?;
        let field = |key: &str| {
            app.get(key)
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::validation(format!("manifest.app.{key} missing")))
                .map(str::to_string)
        };
        let app_id = field("id")?;
        let version = field("version")?;
        let name = field("name")?;
        let homepage = app
            .get("homepage")
            .and_then(|v| v.as_str())
            .map(str::to_string);

        sqlx::query!(
            "INSERT INTO apps (id, name, homepage, updated_at)
             VALUES ($1, $2, $3, now())
             ON CONFLICT (id) DO UPDATE
               SET name = excluded.name,
                   homepage = excluded.homepage,
                   updated_at = now()",
            app_id,
            name,
            homepage,
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;

        sqlx::query_as!(
            AppVersionRow,
            "INSERT INTO app_versions (id, app_id, version, manifest)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (app_id, version) DO UPDATE
               SET manifest = excluded.manifest
             RETURNING id, app_id, version, manifest, channel, published_at",
            setu_common::new_id(),
            app_id,
            version,
            manifest,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_version())
    }

    pub async fn list(&self) -> Result<Vec<App>, AppError> {
        sqlx::query_as!(
            AppRow,
            "SELECT id, name, homepage, created_at, updated_at
             FROM apps ORDER BY id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| {
            rows.into_iter()
                .map(|r| App {
                    id: r.id,
                    name: r.name,
                    homepage: r.homepage,
                    created_at: r.created_at,
                    updated_at: r.updated_at,
                })
                .collect()
        })
    }

    pub async fn version(&self, app_id: &str, version: &str) -> Result<AppVersion, AppError> {
        sqlx::query_as!(
            AppVersionRow,
            "SELECT id, app_id, version, manifest, channel, published_at
             FROM app_versions WHERE app_id = $1 AND version = $2",
            app_id,
            version,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_version())
        .ok_or_else(|| AppError::not_found("app version not found"))
    }
}

#[derive(sqlx::FromRow)]
struct AppRow {
    id: String,
    name: String,
    homepage: Option<String>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct AppVersionRow {
    id: String,
    app_id: String,
    version: String,
    manifest: Value,
    channel: String,
    published_at: DateTime<Utc>,
}

impl AppVersionRow {
    fn into_version(self) -> AppVersion {
        AppVersion {
            id: self.id,
            app_id: self.app_id,
            version: self.version,
            manifest: self.manifest,
            channel: self.channel,
            published_at: self.published_at,
        }
    }
}
