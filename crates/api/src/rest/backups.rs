//! Site backups: list, take (202 `backup_site` job) and restore a backup
//! (202 `restore_site` job). Mutations are operator and above and write
//! `audit_log`; the backup rows themselves are written by the runner.

use std::net::SocketAddr;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::Serialize;
use setu_common::AppError;
use tower_sessions::Session;
use utoipa::ToSchema;

use crate::auth::{audit, audit_ip, require_role, Role};
use crate::state::AppState;

use super::sites::{require_no_running_job, JobOut};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackupOut {
    pub id: String,
    pub site_id: String,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    pub size_bytes: Option<i64>,
    pub location: String,
    pub kind: String,
    pub status: String,
}

impl BackupOut {
    fn from_row(b: &setu_db::Backup) -> Self {
        Self {
            id: b.id.clone(),
            site_id: b.site_id.clone(),
            taken_at: b.taken_at,
            size_bytes: b.size_bytes,
            location: b.location.clone(),
            kind: b.kind.clone(),
            status: b.status.clone(),
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BackupListOut {
    pub backups: Vec<BackupOut>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BackupJobResponse {
    pub job: JobOut,
}

/// A site's backups, newest first (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/backups",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = BackupListOut),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    state.db.sites().get(&id).await?;
    let rows = state.db.backups().list_for_site(&id).await?;
    Ok(Json(BackupListOut {
        backups: rows.iter().map(BackupOut::from_row).collect(),
    }))
}

/// Take a manual backup (operator and above): 202 with a `backup_site`
/// job; the row lands `completed` when the job finishes. 409 while
/// another job is live for the site.
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/backups",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = BackupJobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    require_no_running_job(&state.db, &id).await?;
    let job = state
        .db
        .jobs()
        .enqueue("backup_site", Some(&site.id), Some(&site.server_id), None)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "backup.create",
        Some(&site.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(BackupJobResponse {
            job: JobOut::from_row(&job),
        }),
    ))
}

/// Restore a backup (operator and above): 202 with a `restore_site` job
/// carrying `payload.backup_id`. The backup must belong to the site.
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/backups/{bid}/restore",
    params(
        ("id" = String, Path, description = "Site id"),
        ("bid" = String, Path, description = "Backup id"),
    ),
    responses(
        (status = 202, body = BackupJobResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn restore(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path((id, bid)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    let backup = state.db.backups().get(&bid).await?;
    if backup.site_id != site.id {
        return Err(AppError::validation("backup does not belong to this site"));
    }
    require_no_running_job(&state.db, &id).await?;
    let job = state
        .db
        .jobs()
        .enqueue(
            "restore_site",
            Some(&site.id),
            Some(&site.server_id),
            Some(&serde_json::json!({"backup_id": backup.id})),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "backup.restore",
        Some(&backup.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(BackupJobResponse {
            job: JobOut::from_row(&job),
        }),
    ))
}
