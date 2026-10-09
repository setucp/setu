//! `GET/POST /api/v1/servers`, `GET/DELETE /api/v1/servers/{id}`,
//! `POST /api/v1/servers/{id}/cordon`.
//!
//! `POST` returns the one-time join token and, separately, the install
//! command for the single `setu` binary,
//! `setu agent install --control-plane <public_url>`: the token never
//! sits inside the command string; the operator passes it in
//! `SETU_JOIN_TOKEN` or on stdin (spec §7).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use utoipa::ToSchema;

use crate::auth::{audit, require_role, ClientIp, Role, Session};
use crate::state::AppState;

use super::ApiJson;

#[derive(Debug, Serialize, ToSchema)]
pub struct ServerOut {
    pub id: String,
    pub name: String,
    pub host: Option<String>,
    /// Computed on read (spec §3): `pending` until the first join, then
    /// `online` while `last_seen_at` is within 60 s, else `offline`.
    pub status: String,
    /// No new sites are placed on a cordoned server.
    pub cordoned: bool,
    /// The control plane's own box (`POST /setup/local-server`).
    pub is_control_plane: bool,
    pub last_seen_at: Option<chrono::DateTime<chrono::Utc>>,
    pub agent_version: Option<String>,
    pub arch: Option<String>,
    pub os: Option<String>,
    pub cpu_cores: Option<i32>,
    pub memory_mb: Option<i64>,
    pub disk_mb: Option<i64>,
    /// From the latest heartbeat (`null` before the first).
    pub load1: Option<f64>,
    pub mem_free_mb: Option<i64>,
    pub disk_free_mb: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ServerOut {
    /// The row as of `now` (the state's clock).
    pub fn from_row(s: &setu_db::Server, now: chrono::DateTime<chrono::Utc>) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            host: s.hostname.clone(),
            status: s.status(now).as_str().to_string(),
            cordoned: s.cordoned,
            is_control_plane: s.is_control_plane,
            last_seen_at: s.last_seen_at,
            agent_version: s.agent_version.clone(),
            arch: s.arch.clone(),
            os: s.os.clone(),
            cpu_cores: s.cpu_cores,
            memory_mb: s.memory_mb,
            disk_mb: s.disk_mb,
            load1: s.load1,
            mem_free_mb: s.mem_free_mb,
            disk_free_mb: s.disk_free_mb,
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
    /// Single-use join token (valid 1 hour); shown once. Pass it to the
    /// install command as `SETU_JOIN_TOKEN` or on stdin.
    pub join_token: String,
    /// `setu agent install --control-plane <public_url>` (no token).
    pub install_command: String,
}

/// `public_url` without a trailing slash (`https://panel.example`).
pub fn control_plane_url(public_url: &url::Url) -> String {
    public_url.as_str().trim_end_matches('/').to_string()
}

/// The install command for a remote server. The join token is not part
/// of it: it travels in `SETU_JOIN_TOKEN` or stdin, never on argv.
pub fn install_command(public_url: &url::Url) -> String {
    format!(
        "setu agent install --control-plane {}",
        control_plane_url(public_url)
    )
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CordonRequest {
    pub cordoned: bool,
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
    require_role(&session, Role::Viewer).await?;
    let servers = state.db.servers().list().await?;
    Ok(Json(ServerListOut {
        servers: servers
            .iter()
            .map(|s| ServerOut::from_row(s, state.clock.now()))
            .collect(),
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
    ip: ClientIp,
    ApiJson(body): ApiJson<CreateServerRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    if body.name.trim().is_empty() {
        return Err(AppError::validation("server name must not be empty"));
    }
    let (server, join_token) = state
        .db
        .servers()
        .create_pending(body.name.trim(), false)
        .await?;
    let command = install_command(&state.config.public_url);
    audit(
        &state.db,
        Some(&caller.id),
        "server.create",
        Some(&server.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(CreateServerResponse {
            server: ServerOut::from_row(&server, state.clock.now()),
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
    require_role(&session, Role::Viewer).await?;
    let server = state.db.servers().get(&id).await?;
    Ok(Json(ServerOut::from_row(&server, state.clock.now())))
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
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
        ip.as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}

/// Cordon or uncordon a server (operator and above): a cordoned server
/// takes no new sites (provision `validate` and placement refuse it);
/// its running sites are untouched.
#[utoipa::path(
    post,
    path = "/api/v1/servers/{id}/cordon",
    params(("id" = String, Path, description = "Server id")),
    request_body = CordonRequest,
    responses(
        (status = 200, body = ServerOut),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn cordon(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<CordonRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let server = state.db.servers().set_cordoned(&id, body.cordoned).await?;
    audit(
        &state.db,
        Some(&caller.id),
        if body.cordoned {
            "server.cordon"
        } else {
            "server.uncordon"
        },
        Some(&server.id),
        ip.as_deref(),
    )
    .await;
    Ok(Json(ServerOut::from_row(&server, state.clock.now())))
}
