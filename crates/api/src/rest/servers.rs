//! `GET/POST /api/v1/servers`, `GET/DELETE /api/v1/servers/{id}`.
//!
//! `POST` returns the one-time join token plus the one-line install command
//! for the single `setu` binary:
//! `setu agent install --control-plane <PUBLIC_URL> --join-token <tok>`.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use tower_sessions::Session;
use utoipa::ToSchema;

use crate::auth::{audit, audit_ip, require_role, Role};
use crate::state::AppState;

use super::ApiJson;

#[derive(Debug, Serialize, ToSchema)]
pub struct ServerOut {
    pub id: String,
    pub name: String,
    pub host: Option<String>,
    /// Effective status: `pending` until first join, then `online` while
    /// `last_seen_at` is within 60 s, else `offline` (computed at read
    /// time, spec §3/Task 8).
    pub status: String,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub agent_version: Option<String>,
    pub arch: Option<String>,
    pub os: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_mb: Option<i64>,
    pub load1: Option<f64>,
    pub mem_free_mb: Option<i64>,
    pub disk_free_mb: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Status computed on read (spec §3): `pending`, `online`, `offline`.
pub fn effective_status(server: &setu_db::Server) -> String {
    server.status(chrono::Utc::now()).as_str().to_string()
}

impl ServerOut {
    pub fn from_row(s: &setu_db::Server) -> Self {
        let status = effective_status(s);
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            host: s.hostname.clone(),
            status,
            last_seen_at: s.last_seen_at,
            agent_version: s.agent_version.clone(),
            arch: s.arch.clone(),
            os: s.os.clone(),
            cpu_cores: s.cpu_cores,
            memory_mb: s.memory_mb,
            disk_mb: s.disk_mb,
            // Spec §3 stores no load figures; WS8 reshapes `ServerOut`
            // (ledger R79).
            load1: None,
            mem_free_mb: None,
            disk_free_mb: None,
            created_at: s.created_at,
            updated_at: s.last_seen_at.unwrap_or(s.created_at),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ServerListOut {
    pub servers: Vec<ServerOut>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateServerRequest {
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateServerResponse {
    pub server: ServerOut,
    /// Single-use join token (valid 1 hour); shown once.
    pub join_token: String,
    /// One-line installer using the single binary.
    pub install_command: String,
}

/// Build the installer command for the single `setu` binary.
pub fn install_command(public_url: &url::Url, join_token: &str) -> String {
    format!("setu agent install --control-plane {public_url} --join-token {join_token}")
}

/// List servers (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/servers",
    responses(
        (status = 200, body = ServerListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let servers = state.db.servers().list().await?;
    Ok(Json(ServerListOut {
        servers: servers.iter().map(ServerOut::from_row).collect(),
    }))
}

/// Register a pending server and mint its join token (operator and above).
#[utoipa::path(
    post,
    path = "/api/v1/servers",
    request_body = CreateServerRequest,
    responses(
        (status = 201, body = CreateServerResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<CreateServerRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    if body.name.trim().is_empty() {
        return Err(AppError::validation("server name must not be empty"));
    }
    let (server, join_token) = state
        .db
        .servers()
        .create_pending(body.name.trim(), false)
        .await?;
    let command = install_command(&state.config.public_url, &join_token);
    audit(
        &state.db,
        Some(&caller.id),
        "server.create",
        Some(&server.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(CreateServerResponse {
            server: ServerOut::from_row(&server),
            join_token,
            install_command: command,
        }),
    ))
}

/// One server (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/servers/{id}",
    params(("id" = String, Path, description = "Server id")),
    responses(
        (status = 200, body = ServerOut),
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
    let server = state.db.servers().get(&id).await?;
    Ok(Json(ServerOut::from_row(&server)))
}

/// Delete a server (operator and above); 409 while sites reference it.
#[utoipa::path(
    delete,
    path = "/api/v1/servers/{id}",
    params(("id" = String, Path, description = "Server id")),
    responses(
        (status = 204),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    // Existence first (404 before the sites check).
    state.db.servers().get(&id).await?;
    let sites = state.db.sites().list().await?;
    if sites.iter().any(|s| s.server_id == id) {
        return Err(AppError::conflict("server still has sites"));
    }
    state.db.servers().delete(&id).await?;
    audit(
        &state.db,
        Some(&caller.id),
        "server.delete",
        Some(&id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
