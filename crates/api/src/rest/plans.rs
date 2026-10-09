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
            limits: p.limits.clone(),
            allowed_app_ids: p.allowed_app_ids.clone(),
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

fn checked_limits(limits: &PlanLimitsIn) -> Result<Value, AppError> {
    if limits.memory_mb == 0 || limits.cpu_percent == 0 || limits.disk_mb == 0 {
        return Err(AppError::validation("plan limits must all be positive"));
    }
    Ok(serde_json::json!({
        "memory_mb": limits.memory_mb,
        "cpu_percent": limits.cpu_percent,
        "disk_mb": limits.disk_mb,
    }))
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
    let limits = checked_limits(&body.limits)?;
    let allowed = body
        .allowed_app_ids
        .map(|ids| Value::Array(ids.into_iter().map(Value::String).collect()))
        .unwrap_or(Value::Array(Vec::new()));
    let plan = state
        .db
        .plans()
        .create(body.name.trim(), &limits, &allowed)
        .await?;
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
    let limits = checked_limits(&body.limits)?;
    let allowed = body
        .allowed_app_ids
        .map(|ids| Value::Array(ids.into_iter().map(Value::String).collect()))
        .unwrap_or(Value::Array(Vec::new()));
    let plan = state
        .db
        .plans()
        .update(&id, body.name.trim(), &limits, &allowed)
        .await?;
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
