//! `plans` repo: resource-limit plans (a seeded "Default" exists).

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;

use crate::{map_db_error, AppError};

#[derive(Debug, Clone)]
pub struct Plan {
    pub id: String,
    pub name: String,
    /// `{ memory_mb, cpu_percent, disk_mb }`.
    pub limits: Value,
    /// App ids allowed on this plan; `[]` means all apps.
    pub allowed_app_ids: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
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
    /// (unique name + `ON CONFLICT DO NOTHING`), so concurrent boots
    /// converge on one row.
    pub async fn seed_default(&self) -> Result<Plan, AppError> {
        sqlx::query!(
            "INSERT INTO plans (id, name, limits, allowed_app_ids)
             VALUES ($1, 'Default', $2, $3)
             ON CONFLICT (name) DO NOTHING",
            setu_common::new_id(),
            serde_json::json!({"memory_mb": 512, "cpu_percent": 100, "disk_mb": 5120}),
            serde_json::json!([]),
        )
        .execute(self.pool)
        .await
        .map_err(map_db_error)?;
        self.get_by_name("Default").await
    }

    async fn get_by_name(&self, name: &str) -> Result<Plan, AppError> {
        sqlx::query_as!(
            PlanRow,
            "SELECT id, name, limits, allowed_app_ids, created_at, updated_at
             FROM plans WHERE name = $1",
            name,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_plan())
        .ok_or_else(AppError::internal)
    }

    pub async fn list(&self) -> Result<Vec<Plan>, AppError> {
        sqlx::query_as!(
            PlanRow,
            "SELECT id, name, limits, allowed_app_ids, created_at, updated_at
             FROM plans ORDER BY name, id",
        )
        .fetch_all(self.pool)
        .await
        .map_err(map_db_error)
        .map(|rows| rows.into_iter().map(|r| r.into_plan()).collect())
    }

    pub async fn get(&self, id: &str) -> Result<Plan, AppError> {
        sqlx::query_as!(
            PlanRow,
            "SELECT id, name, limits, allowed_app_ids, created_at, updated_at
             FROM plans WHERE id = $1",
            id,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_plan())
        .ok_or_else(|| AppError::not_found("plan not found"))
    }

    pub async fn create(
        &self,
        name: &str,
        limits: &Value,
        allowed_app_ids: &Value,
    ) -> Result<Plan, AppError> {
        sqlx::query_as!(
            PlanRow,
            "INSERT INTO plans (id, name, limits, allowed_app_ids)
             VALUES ($1, $2, $3, $4)
             RETURNING id, name, limits, allowed_app_ids, created_at, updated_at",
            setu_common::new_id(),
            name,
            limits,
            allowed_app_ids,
        )
        .fetch_one(self.pool)
        .await
        .map_err(map_db_error)
        .map(|r| r.into_plan())
    }

    pub async fn update(
        &self,
        id: &str,
        name: &str,
        limits: &Value,
        allowed_app_ids: &Value,
    ) -> Result<Plan, AppError> {
        sqlx::query_as!(
            PlanRow,
            "UPDATE plans SET name = $2, limits = $3, allowed_app_ids = $4, updated_at = now()
             WHERE id = $1
             RETURNING id, name, limits, allowed_app_ids, created_at, updated_at",
            id,
            name,
            limits,
            allowed_app_ids,
        )
        .fetch_optional(self.pool)
        .await
        .map_err(map_db_error)?
        .map(|r| r.into_plan())
        .ok_or_else(|| AppError::not_found("plan not found"))
    }
}

#[derive(sqlx::FromRow)]
struct PlanRow {
    id: String,
    name: String,
    limits: Value,
    allowed_app_ids: Value,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl PlanRow {
    fn into_plan(self) -> Plan {
        Plan {
            id: self.id,
            name: self.name,
            limits: self.limits,
            allowed_app_ids: self.allowed_app_ids,
            created_at: self.created_at,
            updated_at: self.updated_at,
        }
    }
}
