//! `GET/POST /api/v1/setup` and `POST /api/v1/setup/local-server` —
//! the first-run wizard (spec §8).
//!
//! `GET` is public and reports whether an owner exists yet (plus the
//! configured public URL, which the wizard shows on its second step).
//! `POST` creates the owner operator, but only while no operator exists
//! at all — afterwards it is 409 for everyone, including anonymous
//! callers (there is no session to check on a fresh install).
//! `POST /local-server` registers a pending server named after the
//! control-plane host (or an explicit `name`) and returns the one-line
//! `setu agent install` command, so a single-node install is one flow:
//! create owner → log in → register local server → run the command.

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
use super::servers::{install_command, ServerOut};
use super::ApiJson;
use crate::auth::{audit, audit_ip, hash_password, require_role, Role};
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatusOut {
    /// True while no operator exists (the wizard must run).
    pub needed: bool,
    /// The configured `SETU_PUBLIC_URL`, shown by the wizard.
    pub public_url: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SetupRequest {
    pub email: String,
    pub password: String,
}

// Holds a credential: `Debug` redacts the password.
impl std::fmt::Debug for SetupRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupRequest")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct LocalServerRequest {
    /// Explicit server name; defaults to the machine's hostname.
    pub name: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LocalServerResponse {
    pub server: ServerOut,
    /// Single-use join token (valid 1 hour); shown once.
    pub join_token: String,
    /// One-line installer reusing the already installed `setu` binary.
    pub install_command: String,
}

/// The control-plane host's name for the default local-server name.
/// Falls back to `local-server` when the hostname is unavailable
/// (containers without one set, non-UTF-8 names).
pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local-server".to_string())
}

/// Setup status (public, no auth): whether the first owner still needs
/// creating, plus the public URL for the wizard to display.
#[utoipa::path(
    get,
    path = "/api/v1/setup",
    responses((status = 200, body = SetupStatusOut))
)]
pub async fn status(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let needed = state.db.operators().count().await? == 0;
    Ok(Json(SetupStatusOut {
        needed,
        public_url: state.config.public_url.to_string(),
    }))
}

/// Create the owner operator. Public, exactly once: while at least one
/// operator exists this is 409 `conflict` regardless of authentication.
#[utoipa::path(
    post,
    path = "/api/v1/setup",
    request_body = SetupRequest,
    responses(
        (status = 201, body = OperatorOut),
        (status = 400, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn create_owner(
    State(state): State<AppState>,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<SetupRequest>,
) -> Result<impl IntoResponse, AppError> {
    if !body.email.contains('@') || body.email.len() < 3 {
        return Err(AppError::validation("email must contain @"));
    }
    if body.password.len() < 8 {
        return Err(AppError::validation(
            "password must be at least 8 characters",
        ));
    }
    // No operator exists yet, so the audit row has no author. The insert
    // is atomic under an advisory lock: a concurrent second POST (any
    // email) gets 409 instead of creating a second owner.
    let hash = hash_password(&body.password)?;
    let op = state
        .db
        .operators()
        .create_first_owner(&body.email, &hash)
        .await?;
    audit(
        &state.db,
        None,
        "setup.create-owner",
        Some(&op.email),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(OperatorOut::from_row(&op))))
}

/// Register the local server (operator and above): a pending server
/// named after the hostname plus the `setu agent install` command.
#[utoipa::path(
    post,
    path = "/api/v1/setup/local-server",
    request_body = LocalServerRequest,
    responses(
        (status = 201, body = LocalServerResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
    )
)]
pub async fn local_server(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<LocalServerRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(local_hostname);
    let (server, join_token) = state.db.servers().create_pending(&name).await?;
    let command = install_command(&state.config.public_url, &join_token);
    audit(
        &state.db,
        Some(&caller.id),
        "setup.local-server",
        Some(&server.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(LocalServerResponse {
            server: ServerOut::from_row(&server),
            join_token,
            install_command: command,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_fallback_is_never_empty() {
        assert!(!local_hostname().is_empty());
    }
}
