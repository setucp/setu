//! `GET/POST /api/v1/operators` — owner only (spec §6, authz matrix).

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use tower_sessions::Session;
use utoipa::ToSchema;

use super::auth::OperatorOut;
use super::ApiJson;
use crate::auth::{audit, audit_ip, hash_password, require_role, Role};
use crate::state::AppState;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateOperatorRequest {
    pub email: String,
    pub password: String,
    pub role: String,
}

// Holds a credential: `Debug` redacts the password.
impl CreateOperatorRequest {
    fn redacted(&self) -> String {
        format!(
            "CreateOperatorRequest {{ email: {:?}, role: {:?} }}",
            self.email, self.role
        )
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OperatorListOut {
    pub operators: Vec<OperatorOut>,
}

/// List operators (owner only).
#[utoipa::path(
    get,
    path = "/api/v1/operators",
    responses(
        (status = 200, body = OperatorListOut),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Owner).await?;
    // No `list` repo method (Task 5 surface is fixed): select directly.
    let rows: Vec<(String, String, String, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as("SELECT id, email, role, created_at FROM operators ORDER BY email, id")
            .fetch_all(state.db.pool())
            .await
            .map_err(|e| {
                tracing::error!(error = %e, "operator list failed");
                AppError::internal()
            })?;
    Ok(Json(OperatorListOut {
        operators: rows
            .into_iter()
            .map(|(id, email, role, created_at)| OperatorOut {
                id,
                email,
                role,
                created_at,
            })
            .collect(),
    }))
}

/// Create an operator (owner only). The password is hashed with argon2id
/// before storage and never logged.
#[utoipa::path(
    post,
    path = "/api/v1/operators",
    request_body = CreateOperatorRequest,
    responses(
        (status = 201, body = OperatorOut),
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
    ApiJson(body): ApiJson<CreateOperatorRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Owner).await?;
    tracing::debug!("operator create {}", body.redacted());
    if !body.email.contains('@') || body.email.len() < 3 {
        return Err(AppError::validation("email must contain @"));
    }
    if body.password.len() < 8 {
        return Err(AppError::validation(
            "password must be at least 8 characters",
        ));
    }
    Role::parse(&body.role)?;
    let hash = hash_password(&body.password)?;
    let op = state
        .db
        .operators()
        .create(&body.email, &hash, &body.role)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "operator.create",
        Some(&op.email),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(OperatorOut::from_row(&op))))
}
