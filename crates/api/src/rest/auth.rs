//! `POST /api/v1/auth/login`, `POST /auth/logout`, `GET /auth/me`.

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

use crate::auth::{audit, audit_ip, current_operator, verify_password, SESSION_OPERATOR_ID};
use crate::state::AppState;

use super::ApiJson;

/// Baked-in argon2id hash (default params, same cost as real verifies) used
/// when the email is unknown, so unknown-email timing matches a known-user
/// wrong-password attempt. The password that produced it is discarded; the
/// result is ignored — both paths return the identical 401 message below.
const DUMMY_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$c2V0dS1kdW1teS1zYWx0LWZpeGVk$f6zqtg+k125brfyKsldH5k7Q795A1TRva0BDSRu//Tg";

#[derive(Deserialize, ToSchema)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

// Holds a credential: `Debug` redacts the password.
impl std::fmt::Debug for LoginRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRequest")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct OperatorOut {
    pub id: String,
    pub email: String,
    pub role: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl OperatorOut {
    pub fn from_row(op: &setu_db::Operator) -> Self {
        Self {
            id: op.id.clone(),
            email: op.email.clone(),
            role: op.role.clone(),
            created_at: op.created_at,
        }
    }
}

/// Log in: verify the argon2id hash, rotate the session id (fixation
/// guard), store the operator id, and return the operator. Failures count
/// per source IP: 10 within 15 minutes → 429 on subsequent attempts.
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    request_body = LoginRequest,
    responses(
        (status = 200, body = OperatorOut),
        (status = 401, body = super::ErrorBody),
        (status = 429, body = super::ErrorBody),
    )
)]
pub async fn login(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<LoginRequest>,
) -> Result<impl IntoResponse, AppError> {
    let ip = peer.0.ip();
    state.check_login_rate(ip).await?;
    tracing::debug!("login attempt for {:?}", body.email);

    let fail = || AppError::unauthorized("invalid email or password");

    let Some(op) = state.db.operators().by_email(&body.email).await? else {
        // Same-cost dummy verify so unknown-email timing matches the
        // known-user wrong-password path; the message stays identical.
        let _ = verify_password(DUMMY_HASH, &body.password);
        state.record_login_failure(ip).await;
        return Err(fail());
    };
    if verify_password(&op.password_hash, &body.password).is_err() {
        state.record_login_failure(ip).await;
        return Err(fail());
    }

    state.clear_login_failures(ip).await;
    session.cycle_id().await.map_err(|e| {
        tracing::error!("session cycle failed: {e}");
        AppError::internal()
    })?;
    session
        .insert(SESSION_OPERATOR_ID, op.id.clone())
        .await
        .map_err(|e| {
            tracing::error!("session store failed: {e}");
            AppError::internal()
        })?;
    audit(
        &state.db,
        Some(&op.id),
        "auth.login",
        Some(&op.email),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((StatusCode::OK, Json(OperatorOut::from_row(&op))))
}

/// Log out: destroy the session. Anonymous callers get 401 (checked first,
//  so logout never leaks whether a session existed).
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    responses((status = 204), (status = 401, body = super::ErrorBody))
)]
pub async fn logout(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
) -> Result<impl IntoResponse, AppError> {
    let op = current_operator(&session, &state.db).await?;
    session.delete().await.map_err(|e| {
        tracing::error!("session delete failed: {e}");
        AppError::internal()
    })?;
    audit(
        &state.db,
        Some(&op.id),
        "auth.logout",
        Some(&op.email),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// The current operator, or 401 when anonymous.
#[utoipa::path(
    get,
    path = "/api/v1/auth/me",
    responses(
        (status = 200, body = OperatorOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn me(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    let op = current_operator(&session, &state.db).await?;
    Ok(Json(OperatorOut::from_row(&op)))
}
