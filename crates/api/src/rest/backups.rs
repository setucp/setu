//! Site backups: list, take (202 `backup_site` job), restore (202
//! `restore_site` job) and delete (202 `delete_backup` job). Mutations are
//! operator and above and write `audit_log`; the backup rows themselves
//! are written by the runner.

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

use super::sites::{require_no_running_job, JobOut};

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BackupOut {
    pub id: String,
    /// `null` once the site is deleted (the backup outlives it).
    pub site_id: Option<String>,
    pub site_name: String,
    pub domain: String,
    pub taken_at: chrono::DateTime<chrono::Utc>,
    pub size_bytes: Option<i64>,
    pub kind: String,
    pub status: String,
}

impl BackupOut {
    fn from_row(b: &setu_db::Backup) -> Self {
        Self {
            id: b.id.clone(),
            site_id: b.site_id.clone(),
            site_name: b.site_name.clone(),
            domain: b.domain.clone(),
            taken_at: b.taken_at,
            size_bytes: b.size_bytes,
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
    require_role(&session, Role::Viewer).await?;
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
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
        ip.as_deref(),
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
    ip: ClientIp,
    Path((id, bid)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    let backup = state.db.backups().get(&bid).await?;
    if backup.site_id.as_deref() != Some(site.id.as_str()) {
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
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(BackupJobResponse {
            job: JobOut::from_row(&job),
        }),
    ))
}

/// Delete a backup's files on its server (operator and above): 202 with a
/// `delete_backup` job on the site (it serialises with the site's other
/// jobs); the row ends `deleted` and leaves the list. 409 when the backup
/// is already deleted, a delete is already queued, or the site has an
/// active job.
#[utoipa::path(
    delete,
    path = "/api/v1/sites/{id}/backups/{bid}",
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
pub async fn delete(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path((id, bid)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    let backup = state.db.backups().get(&bid).await?;
    if backup.site_id.as_deref() != Some(site.id.as_str()) {
        return Err(AppError::validation("backup does not belong to this site"));
    }
    enqueue_delete(&state, &caller.id, ip.as_deref(), &backup).await
}

/// All backups, newest first, including those of deleted sites
/// (`site_id` null; `site_name` and `domain` name them). Viewer and above.
#[utoipa::path(
    get,
    path = "/api/v1/backups",
    responses(
        (status = 200, body = BackupListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn list_all(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Viewer).await?;
    let rows = state.db.backups().list_all().await?;
    Ok(Json(BackupListOut {
        backups: rows.iter().map(BackupOut::from_row).collect(),
    }))
}

/// Delete any backup by id (operator and above), the only way to delete
/// a `pre_delete` backup once its site is gone: 202 with the same
/// `delete_backup` job as `DELETE /sites/{id}/backups/{bid}` (on the
/// site while it exists, else server-level); 409 when already deleted or
/// queued, or the site has an active job.
#[utoipa::path(
    delete,
    path = "/api/v1/backups/{bid}",
    params(("bid" = String, Path, description = "Backup id")),
    responses(
        (status = 202, body = BackupJobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn delete_any(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(bid): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let backup = state.db.backups().get(&bid).await?;
    enqueue_delete(&state, &caller.id, ip.as_deref(), &backup).await
}

async fn enqueue_delete(
    state: &AppState,
    caller_id: &str,
    ip: Option<&str>,
    backup: &setu_db::Backup,
) -> Result<(StatusCode, Json<BackupJobResponse>), AppError> {
    if backup.status == "deleted" {
        return Err(AppError::conflict("backup already deleted"));
    }
    if state.db.resets().is_reset_point(&backup.id).await? {
        return Err(AppError::conflict(
            "backup is the site's reset point; stop the scheduled reset first",
        ));
    }
    if state
        .db
        .jobs()
        .active_delete_backup(&backup.id)
        .await?
        .is_some()
    {
        return Err(AppError::conflict("backup delete already queued"));
    }
    // A live site's backup is deleted by a job on that site: the
    // one-active-job-per-site index then keeps it from racing an update
    // or restore that reads the backup (409 while one runs; M6, R136).
    // A deleted site's backup (`site_id` null) gets a server-level job.
    let job = state
        .db
        .jobs()
        .enqueue(
            "delete_backup",
            backup.site_id.as_deref(),
            Some(&backup.server_id),
            Some(&serde_json::json!({"backup_id": backup.id})),
        )
        .await?;
    audit(
        &state.db,
        Some(caller_id),
        "backup.delete",
        Some(&backup.id),
        ip,
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(BackupJobResponse {
            job: JobOut::from_row(&job),
        }),
    ))
}

/// A site's scheduled reset (0.1.4 §2).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ResetOut {
    pub backup_id: String,
    pub every_minutes: i32,
    pub last_reset_at: chrono::DateTime<chrono::Utc>,
    pub next_reset_at: chrono::DateTime<chrono::Utc>,
}

impl ResetOut {
    fn from_row(r: &setu_db::SiteReset) -> Self {
        Self {
            backup_id: r.backup_id.clone(),
            every_minutes: r.every_minutes,
            last_reset_at: r.last_reset_at,
            next_reset_at: r.next_at(),
        }
    }
}

/// `GET /sites/{id}/reset`: the schedule, or `null`.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResetGetOut {
    pub reset: Option<ResetOut>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ResetPutRequest {
    /// A completed backup of this site.
    pub backup_id: String,
    /// Minutes between resets, 15 to 10080 (a week).
    pub every_minutes: i32,
}

/// The site's scheduled reset (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/reset",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = ResetGetOut),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn reset_get(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Viewer).await?;
    let site = state.db.sites().get(&id).await?;
    let reset = state.db.resets().get(&site.id).await?;
    Ok(Json(ResetGetOut {
        reset: reset.as_ref().map(ResetOut::from_row),
    }))
}

/// Set the site's scheduled reset (operator and above): every
/// `every_minutes` the scheduler restores `backup_id`, skipping the
/// pre-restore backup. The first reset is one interval from now.
#[utoipa::path(
    put,
    path = "/api/v1/sites/{id}/reset",
    params(("id" = String, Path, description = "Site id")),
    request_body = ResetPutRequest,
    responses(
        (status = 200, body = ResetGetOut),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn reset_put(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
    super::ApiJson(body): super::ApiJson<ResetPutRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    if !(15..=10_080).contains(&body.every_minutes) {
        return Err(AppError::validation(
            "every_minutes must be between 15 and 10080",
        ));
    }
    let backup = state.db.backups().get(&body.backup_id).await?;
    if backup.site_id.as_deref() != Some(site.id.as_str()) {
        return Err(AppError::validation("backup does not belong to this site"));
    }
    if backup.status != "completed" {
        return Err(AppError::validation(format!(
            "backup {} is {}",
            backup.id, backup.status
        )));
    }
    let row = state
        .db
        .resets()
        .set(&site.id, &backup.id, body.every_minutes, state.clock.now())
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.reset_set",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok(Json(ResetGetOut {
        reset: Some(ResetOut::from_row(&row)),
    }))
}

/// Stop the site's scheduled reset (operator and above); 404 when none.
#[utoipa::path(
    delete,
    path = "/api/v1/sites/{id}/reset",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 204),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn reset_delete(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    if !state.db.resets().clear(&site.id).await? {
        return Err(AppError::not_found("this site has no scheduled reset"));
    }
    audit(
        &state.db,
        Some(&caller.id),
        "site.reset_clear",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
}
