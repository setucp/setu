//! `GET/POST /api/v1/plans`, `GET/PUT /api/v1/plans/{id}`.
//!
//! Reads are viewer-and-above; creation and updates are owner-only
//! (operators may not touch plans, per the authz matrix).

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use setu_common::AppError;
use tower_sessions::Session;
use utoipa::ToSchema;

use crate::auth::{audit, audit_ip, require_role, Role};
use crate::state::AppState;

use super::ApiJson;

#[derive(Debug, Serialize, ToSchema)]
pub struct PlanOut {
    pub id: String,
    pub name: String,
    pub limits: Value,
    pub allowed_app_ids: Value,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl PlanOut {
    fn from_row(p: &setu_db::Plan) -> Self {
        Self {
            id: p.id.clone(),
            name: p.name.clone(),
            limits: serde_json::json!({
                "memory_mb": p.memory_mb,
                "cpu_percent": p.cpu_percent,
                "disk_mb": p.disk_mb,
            }),
            allowed_app_ids: serde_json::json!(p.allowed_app_ids.clone().unwrap_or_default()),
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PlanListOut {
    pub plans: Vec<PlanOut>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PlanLimitsIn {
    pub memory_mb: u32,
    pub cpu_percent: u32,
    pub disk_mb: u32,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreatePlanRequest {
    pub name: String,
    pub limits: PlanLimitsIn,
    pub allowed_app_ids: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdatePlanRequest {
    pub name: String,
    pub limits: PlanLimitsIn,
    pub allowed_app_ids: Option<Vec<String>>,
}

/// The stored plan; `allowed_app_ids` absent or empty means every app.
fn new_plan(
    name: &str,
    limits: &PlanLimitsIn,
    allowed_app_ids: Option<Vec<String>>,
) -> Result<setu_db::NewPlan, AppError> {
    let positive = |v: u32| i32::try_from(v).ok().filter(|v| *v > 0);
    let (Some(memory_mb), Some(cpu_percent), Some(disk_mb)) = (
        positive(limits.memory_mb),
        positive(limits.cpu_percent),
        positive(limits.disk_mb),
    ) else {
        return Err(AppError::validation("plan limits must all be positive"));
    };
    Ok(setu_db::NewPlan {
        name: name.to_string(),
        memory_mb,
        cpu_percent,
        disk_mb,
        allowed_app_ids: allowed_app_ids.filter(|ids| !ids.is_empty()),
    })
}

/// List plans (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/plans",
    responses(
        (status = 200, body = PlanListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let plans = state.db.plans().list().await?;
    Ok(Json(PlanListOut {
        plans: plans.iter().map(PlanOut::from_row).collect(),
    }))
}

/// Create a plan (owner only).
#[utoipa::path(
    post,
    path = "/api/v1/plans",
    request_body = CreatePlanRequest,
    responses(
        (status = 201, body = PlanOut),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<CreatePlanRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Owner).await?;
    if body.name.trim().is_empty() {
        return Err(AppError::validation("plan name must not be empty"));
    }
    let plan = new_plan(body.name.trim(), &body.limits, body.allowed_app_ids)?;
    let plan = state.db.plans().create(&plan).await?;
    audit(
        &state.db,
        Some(&caller.id),
        "plan.create",
        Some(&plan.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(PlanOut::from_row(&plan))))
}

/// One plan (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/plans/{id}",
    params(("id" = String, Path, description = "Plan id")),
    responses(
        (status = 200, body = PlanOut),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn get(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let plan = state.db.plans().get(&id).await?;
    Ok(Json(PlanOut::from_row(&plan)))
}

/// Update a plan (owner only).
#[utoipa::path(
    put,
    path = "/api/v1/plans/{id}",
    params(("id" = String, Path, description = "Plan id")),
    request_body = UpdatePlanRequest,
    responses(
        (status = 200, body = PlanOut),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn update(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdatePlanRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Owner).await?;
    if body.name.trim().is_empty() {
        return Err(AppError::validation("plan name must not be empty"));
    }
    let plan = new_plan(body.name.trim(), &body.limits, body.allowed_app_ids)?;
    let plan = state.db.plans().update(&id, &plan).await?;
    audit(
        &state.db,
        Some(&caller.id),
        "plan.update",
        Some(&plan.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok(Json(PlanOut::from_row(&plan)))
}
