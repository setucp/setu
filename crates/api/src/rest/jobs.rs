//! `GET /api/v1/jobs`, `GET /jobs/{id}`, `GET /jobs/{id}/log` (SSE tail)
//! and `GET /api/v1/audit` (spec §6, plan Task 9). Reads are viewer and
//! above; nothing here mutates, so no audit rows.

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use futures::channel::mpsc;
use futures::SinkExt;
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use tower_sessions::Session;
use utoipa::ToSchema;

use crate::auth::{require_role, Role};
use crate::state::AppState;

use super::sites::JobOut;

#[derive(Debug, Deserialize, ToSchema)]
pub struct JobsQuery {
    pub limit: Option<i64>,
}

/// Recent jobs, newest first (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/jobs",
    params(("limit" = Option<i64>, Query, description = "Max rows, default 50, cap 200")),
    responses(
        (status = 200, body = JobListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<JobsQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let jobs = state.db.jobs().list_recent(limit).await?;
    Ok(Json(JobListOut {
        jobs: jobs.iter().map(JobOut::from_row).collect(),
    }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct JobListOut {
    pub jobs: Vec<JobOut>,
}

/// One job (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/jobs/{id}",
    params(("id" = String, Path, description = "Job id")),
    responses(
        (status = 200, body = JobOut),
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
    let job = state.db.jobs().get(&id).await?;
    Ok(Json(JobOut::from_row(&job)))
}

/// Tail a job's log as SSE (viewer and above): the accumulated `jobs.log`
/// lines first, then — while the job is still `queued`/`running` — new
/// deltas every 500 ms until it finishes or 30 s pass. Finished jobs
/// close the stream after the backlog.
#[utoipa::path(
    get,
    path = "/api/v1/jobs/{id}/log",
    params(("id" = String, Path, description = "Job id")),
    responses(
        (status = 200, description = "SSE log tail (`data: <line>` events)"),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn log(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let job = state.db.jobs().get(&id).await?;

    let (mut tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(64);
    let db = state.db.clone();
    tokio::spawn(async move {
        let mut sent_len: usize;
        // Backlog first (the whole log for a finished job).
        for line in job.log.lines() {
            let chunk = format!("data: {line}\n\n");
            if tx.send(Ok(axum::body::Bytes::from(chunk))).await.is_err() {
                return;
            }
        }
        sent_len = job.log.len();
        if job.status != "queued" && job.status != "running" {
            return;
        }
        // Live tail: poll for appended log text.
        for _ in 0..60 {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let Ok(current) = db.jobs().get(&id).await else {
                return;
            };
            if current.log.len() > sent_len {
                for line in current.log[sent_len..].lines() {
                    let chunk = format!("data: {line}\n\n");
                    if tx.send(Ok(axum::body::Bytes::from(chunk))).await.is_err() {
                        return;
                    }
                }
                sent_len = current.log.len();
            }
            if current.status != "queued" && current.status != "running" {
                return;
            }
        }
    });

    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::CONNECTION, "keep-alive"),
        ],
        Body::from_stream(rx),
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct AuditQuery {
    pub limit: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEntryOut {
    pub id: String,
    pub operator_id: Option<String>,
    pub action: String,
    pub target: Option<String>,
    pub ip: Option<String>,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditListOut {
    pub audit: Vec<AuditEntryOut>,
}

/// Recent audit rows, newest first (viewer and above — the authz matrix
/// grants viewers every GET).
#[utoipa::path(
    get,
    path = "/api/v1/audit",
    params(("limit" = Option<i64>, Query, description = "Max rows, default 100, cap 500")),
    responses(
        (status = 200, body = AuditListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn audit_list(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<AuditQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let limit = query.limit.unwrap_or(100).clamp(1, 500);
    let rows = state.db.audit().list_recent(limit).await?;
    Ok(Json(AuditListOut {
        audit: rows
            .into_iter()
            .map(|e| AuditEntryOut {
                id: e.id,
                operator_id: e.operator_id,
                action: e.action,
                target: e.target_id,
                ip: e.ip,
                at: e.at,
            })
            .collect(),
    }))
}
