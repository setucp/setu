//! `GET /api/v1/panel/backup`: the control plane's self-backup status
//! for Settings → Panel (viewer and above, 0.1.1 #9).

use axum::{extract::State, response::IntoResponse, Json};
use serde::Serialize;
use setu_common::AppError;
use utoipa::ToSchema;

use crate::auth::{require_role, Role, Session};
use crate::state::AppState;

#[derive(Debug, Serialize, ToSchema)]
pub struct SelfBackupOut {
    pub last_success_at: Option<chrono::DateTime<chrono::Utc>>,
    /// File name of the newest dump in `dir`.
    pub file: Option<String>,
    pub size_bytes: Option<i64>,
    pub last_attempt_at: Option<chrono::DateTime<chrono::Utc>>,
    /// One line, no secrets (`pg_dump failed: exit status: 1`).
    pub last_error: Option<String>,
    /// Where dumps are written (`<data_dir>/backups/setu`).
    pub dir: String,
}

/// The last control-plane self-backup.
#[utoipa::path(
    get,
    path = "/api/v1/panel/backup",
    responses((status = 200, body = SelfBackupOut), (status = 401, body = super::ErrorBody))
)]
pub async fn backup_status(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Viewer).await?;
    let rec = crate::self_backup::load(&state.db).await?;
    Ok(Json(SelfBackupOut {
        last_success_at: rec.last_success_at,
        file: rec.file,
        size_bytes: rec.size_bytes,
        last_attempt_at: rec.last_attempt_at,
        last_error: rec.last_error,
        dir: crate::self_backup::backup_dir(&state.config.data_dir)
            .display()
            .to_string(),
    }))
}
