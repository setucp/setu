//! `plans` repo: resource-limit plans (a seeded "Default" exists:
//! 512 MB, 100 %, 5 GB). `allowed_app_ids` `None` means every app.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::{map_db_error, AppError};

#[derive(Debug, Clone)]
pub struct Plan {
    pub id: String,
    pub name: String,
    pub memory_mb: i32,
    pub cpu_percent: i32,
    pub disk_mb: i32,
    /// App ids allowed on this plan; `None` means all apps.
    pub allowed_app_ids: Option<Vec<String>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Plan {
    /// Whether this plan may run `app_id`.
    pub fn allows(&self, app_id: &str) -> bool {
        self.allowed_app_ids
            .as_ref()
            .is_none_or(|ids| ids.iter().any(|a| a == app_id))
    }
}

/// Fields of a plan to create or replace.
#[derive(Debug, Clone)]
pub struct NewPlan {
    pub name: String,
    pub memory_mb: i32,
    pub cpu_percent: i32,
    pub disk_mb: i32,
    pub allowed_app_ids: Option<Vec<String>>,
}

/// Repository accessor; build via [`crate::Db::plans`].
pub struct Plans<'a> {
    pool: &'a PgPool,
}

impl<'a> Plans<'a> {
    pub(crate) fn new(pool: &'a PgPool) -> Self {
        Self { pool }
    }

    /// Return the "Default" plan, creating it on first call. Idempotent
    /// (unique name + `ON CONFLICT DO NOTHING`).
    pub async fn seed_default(&self) -> Result<Plan, AppError> {
        sqlx::query!(
            "INSERT INTO plans (id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids)
             VALUES ($1, 'Default', 512, 100, 5120, NULL)
             ON CONFLICT (name) DO NOTHING",
            setu_common::new_id(),
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        sqlx::query_as!(
            Plan,
            "SELECT id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids,
                    created_at, updated_at
             FROM plans WHERE name = 'Default'",
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn list(&self) -> Result<Vec<Plan>, AppError> {
        sqlx::query_as!(
            Plan,
            "SELECT id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids,
                    created_at, updated_at
             FROM plans ORDER BY name, id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn get(&self, id: &str) -> Result<Plan, AppError> {
        sqlx::query_as!(
            Plan,
            "SELECT id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids,
                    created_at, updated_at
             FROM plans WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("plan not found"))
    }

    pub async fn create(&self, plan: &NewPlan) -> Result<Plan, AppError> {
        sqlx::query_as!(
            Plan,
            "INSERT INTO plans (id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids)
             VALUES ($1, $2, $3, $4, $5, $6)
             RETURNING id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids,
                       created_at, updated_at",
            setu_common::new_id(),
            plan.name,
            plan.memory_mb,
            plan.cpu_percent,
            plan.disk_mb,
            plan.allowed_app_ids.as_deref(),
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
    }

    pub async fn update(&self, id: &str, plan: &NewPlan) -> Result<Plan, AppError> {
        sqlx::query_as!(
            Plan,
            "UPDATE plans SET name = $2, memory_mb = $3, cpu_percent = $4, disk_mb = $5,
                              allowed_app_ids = $6, updated_at = now()
             WHERE id = $1
             RETURNING id, name, memory_mb, cpu_percent, disk_mb, allowed_app_ids,
                       created_at, updated_at",
            id,
            plan.name,
            plan.memory_mb,
            plan.cpu_percent,
            plan.disk_mb,
            plan.allowed_app_ids.as_deref(),
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| AppError::not_found("plan not found"))
    }
}
