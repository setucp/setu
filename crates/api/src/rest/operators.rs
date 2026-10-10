//! `GET/POST /api/v1/operators` — owner only (spec §7, authz matrix) —
//! and `PUT /api/v1/operators/{id}/password` (yourself with your current
//! password, or the owner for anyone). A password change deletes every
//! session of that operator (spec §3).

use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use utoipa::ToSchema;

use super::auth::OperatorOut;
use super::ApiJson;
use crate::auth::session::{secure_cookies, set_cookie};
use crate::auth::{
    audit, current_operator, hash_password_blocking, require_role, verify_password_blocking,
    ClientIp, Role, Session,
};
use crate::state::AppState;

#[derive(Deserialize, ToSchema)]
pub struct CreateOperatorRequest {
    pub email: String,
    pub password: String,
    pub role: String,
}

// Holds a credential: `Debug` redacts the password.
impl std::fmt::Debug for CreateOperatorRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateOperatorRequest")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .field("role", &self.role)
            .finish()
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
    require_role(&session, Role::Owner).await?;
    let rows = state.db.operators().list().await?;
    Ok(Json(OperatorListOut {
        operators: rows.iter().map(OperatorOut::from_row).collect(),
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
    ip: ClientIp,
    ApiJson(body): ApiJson<CreateOperatorRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Owner).await?;
    tracing::debug!("operator create {body:?}");
    if !body.email.contains('@') || body.email.len() < 3 {
        return Err(AppError::validation("email must contain @"));
    }
    if body.password.len() < 8 {
        return Err(AppError::validation(
            "password must be at least 8 characters",
        ));
    }
    Role::parse(&body.role)?;
    let hash = hash_password_blocking(&body.password).await?;
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
        ip.as_deref(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(OperatorOut::from_row(&op))))
}

#[derive(Deserialize, ToSchema)]
pub struct SetPasswordRequest {
    /// Required when changing your own password.
    #[serde(default)]
    pub current_password: Option<String>,
    pub password: String,
}

// Holds credentials: `Debug` redacts both.
impl std::fmt::Debug for SetPasswordRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SetPasswordRequest([redacted])")
    }
}

/// Change a password: your own (with `current_password`), or anyone's
/// as the owner. Every session of that operator ends (spec §3); changing
/// your own starts a fresh session for this browser.
#[utoipa::path(
    put,
    path = "/api/v1/operators/{id}/password",
    params(("id" = String, Path, description = "Operator id")),
    request_body = SetPasswordRequest,
    responses(
        (status = 204),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn set_password(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<SetPasswordRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = current_operator(&session).await?;
    let own = caller.id == id;
    if own {
        let current = body.current_password.as_deref().unwrap_or_default();
        // Same per-IP limiter as login, counted before the verify (M12).
        let key = ip.limiter_key();
        state.begin_login_attempt(key).await?;
        verify_password_blocking(&caller.password_hash, current)
            .await
            .map_err(|_| AppError::forbidden("current password is incorrect"))?;
        state.forgive_login_attempt(key).await;
    } else {
        require_role(&session, Role::Owner).await?;
        if state.db.operators().by_id(&id).await?.is_none() {
            return Err(AppError::not_found("operator not found"));
        }
    }
    if body.password.len() < 8 {
        return Err(AppError::validation(
            "password must be at least 8 characters",
        ));
    }
    let hash = hash_password_blocking(&body.password).await?;
    state.db.operators().set_password_hash(&id, &hash).await?;
    audit(
        &state.db,
        Some(&caller.id),
        "operator.password",
        Some(&id),
        ip.as_deref(),
    )
    .await;
    if own {
        let cookie = state.db.sessions().create(&id).await?;
        let header = set_cookie(&cookie, secure_cookies(&state.config.public_url));
        return Ok((StatusCode::NO_CONTENT, [(header::SET_COOKIE, header)]).into_response());
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}
