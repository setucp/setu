//! `GET/PUT/DELETE /api/v1/sites/{id}/basic-auth` (0.1.7): HTTP basic
//! auth in front of one site, enforced by Caddy. The password is hashed
//! with bcrypt (the format Caddy reads) and only the hash is stored; no
//! response ever carries a password or a hash. A change stores the
//! credentials and queues a `route_update` job that rewrites the site's
//! Caddy route, in one transaction. Every response says whether the
//! stored setting is in force (`route`), from that job: a failed route
//! change shows as `failed`, never as protected.

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

use super::sites::require_no_running_job;
use super::sites::JobOut;

/// bcrypt cost: 12 in release builds; the minimum under `debug_assertions`
/// so the test suite stays fast.
const COST: u32 = if cfg!(debug_assertions) { 4 } else { 12 };

/// Longest password bcrypt reads (bytes).
const MAX_PASSWORD: usize = 72;

/// A site's basic-auth setting: the user name only.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BasicAuthOut {
    pub username: String,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Whether the stored setting is what Caddy enforces, from the site's
/// newest `route_update` job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    /// The route has the stored setting (or no route change was ever made).
    Applied,
    /// The route job is queued or running.
    Applying,
    /// The last route change failed: the route may still have the setting
    /// from before it, whatever `basic_auth` says (an agent restart or a lost
    /// ack can apply the new one anyway).
    Failed,
}

/// The route state behind a basic-auth setting.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BasicAuthRoute {
    pub status: RouteStatus,
    /// The newest `route_update` job, if there is one.
    pub job_id: Option<String>,
    /// Its error when `status` is `failed`.
    pub error: Option<String>,
}

impl BasicAuthRoute {
    fn from_job(job: Option<&setu_db::Job>) -> Self {
        let status = match job.map(|j| j.status.as_str()) {
            None | Some("done") => RouteStatus::Applied,
            Some("failed") => RouteStatus::Failed,
            Some(_) => RouteStatus::Applying,
        };
        Self {
            status,
            job_id: job.map(|j| j.id.clone()),
            error: job
                .filter(|_| status == RouteStatus::Failed)
                .and_then(|j| j.error.clone()),
        }
    }
}

/// `GET /sites/{id}/basic-auth`: the stored setting (`null` when the site
/// is set to be open) and whether the route has it.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BasicAuthGetOut {
    pub basic_auth: Option<BasicAuthOut>,
    pub route: BasicAuthRoute,
}

/// `PUT /sites/{id}/basic-auth` body.
#[derive(Deserialize, ToSchema)]
pub struct BasicAuthPutRequest {
    /// Letters, digits and `._-`, 1 to 64.
    pub username: String,
    /// 8 to 72 bytes, no control characters. Never stored or returned.
    pub password: String,
}

// Holds a password: `Debug` never prints it.
impl std::fmt::Debug for BasicAuthPutRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BasicAuthPutRequest")
            .field("username", &self.username)
            .field("password", &"[redacted]")
            .finish()
    }
}

/// `PUT`/`DELETE` result: the setting after the change, the route state
/// (`applying`) and the route job.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BasicAuthJobResponse {
    pub basic_auth: Option<BasicAuthOut>,
    pub route: BasicAuthRoute,
    pub job: JobOut,
}

fn job_response(basic_auth: Option<BasicAuthOut>, job: &setu_db::Job) -> BasicAuthJobResponse {
    BasicAuthJobResponse {
        basic_auth,
        route: BasicAuthRoute::from_job(Some(job)),
        job: JobOut::from_row(job),
    }
}

async fn latest_route_job(
    state: &AppState,
    site_id: &str,
) -> Result<Option<setu_db::Job>, AppError> {
    state
        .db
        .jobs()
        .latest_for_site(site_id, setu_db::ROUTE_JOB)
        .await
}

fn out(row: &setu_db::SiteBasicAuth) -> BasicAuthOut {
    BasicAuthOut {
        username: row.username.clone(),
        updated_at: row.updated_at,
    }
}

/// The rules for a user name and password; the message never echoes the password.
pub fn validate(username: &str, password: &str) -> Result<(), AppError> {
    let name_ok = (1..=64).contains(&username.len())
        && username
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if !name_ok {
        return Err(AppError::validation(
            "username must be 1 to 64 letters, digits, '.', '_' or '-'",
        ));
    }
    if !(8..=MAX_PASSWORD).contains(&password.len()) {
        return Err(AppError::validation("password must be 8 to 72 bytes long"));
    }
    if password.chars().any(char::is_control) {
        return Err(AppError::validation(
            "password must not contain control characters",
        ));
    }
    Ok(())
}

/// bcrypt hash on the blocking pool (it is CPU-bound).
async fn hash_blocking(password: &str) -> Result<String, AppError> {
    let password = password.to_string();
    tokio::task::spawn_blocking(move || bcrypt::hash_with_result(password, COST))
        .await
        .map_err(|e| {
            tracing::error!("basic-auth hashing task failed: {e}");
            AppError::internal()
        })?
        .map(|h| h.format_for_version(bcrypt::Version::TwoB))
        .map_err(|e| {
            tracing::error!("basic-auth hashing failed: {e}");
            AppError::internal()
        })
}

fn require_settled(site: &setu_db::Site) -> Result<(), AppError> {
    if site.status == "running" || site.status == "stopped" {
        Ok(())
    } else {
        Err(AppError::conflict(format!(
            "site cannot change its basic auth from status {}",
            site.status
        )))
    }
}

/// The site's basic-auth user name (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/basic-auth",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = BasicAuthGetOut),
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
    let site = state.db.sites().get(&id).await?;
    let row = state.db.basic_auths().get(&site.id).await?;
    let job = latest_route_job(&state, &site.id).await?;
    Ok(Json(BasicAuthGetOut {
        basic_auth: row.as_ref().map(out),
        route: BasicAuthRoute::from_job(job.as_ref()),
    }))
}

/// Set or replace the site's basic auth (operator and above); queues a
/// route update. 409 (and nothing stored) when the site has an active job.
#[utoipa::path(
    put,
    path = "/api/v1/sites/{id}/basic-auth",
    params(("id" = String, Path, description = "Site id")),
    request_body = BasicAuthPutRequest,
    responses(
        (status = 202, body = BasicAuthJobResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn put(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
    super::ApiJson(body): super::ApiJson<BasicAuthPutRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    validate(&body.username, &body.password)?;
    require_settled(&site)?;
    require_no_running_job(&state.db, &site.id).await?;
    let hash = hash_blocking(&body.password).await?;
    let (row, job) = state
        .db
        .basic_auths()
        .set_with_route_job(
            &site.id,
            &site.server_id,
            &body.username,
            &hash,
            state.clock.now(),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.basic_auth_set",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(job_response(Some(out(&row)), &job)),
    ))
}

/// Remove the site's basic auth (operator and above); queues a route
/// update. When the site has no credentials stored but its last route
/// change failed, the route update is queued again (the route may still
/// ask for a password); otherwise 404.
#[utoipa::path(
    delete,
    path = "/api/v1/sites/{id}/basic-auth",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = BasicAuthJobResponse),
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
    let site = state.db.sites().get(&id).await?;
    require_settled(&site)?;
    require_no_running_job(&state.db, &site.id).await?;
    let cleared = state
        .db
        .basic_auths()
        .clear_with_route_job(&site.id, &site.server_id)
        .await?;
    let job = if let Some(job) = cleared {
        job
    } else {
        let last_failed = latest_route_job(&state, &site.id)
            .await?
            .is_some_and(|j| j.status == "failed");
        if !last_failed {
            return Err(AppError::not_found("this site has no basic auth"));
        }
        state
            .db
            .jobs()
            .enqueue(
                setu_db::ROUTE_JOB,
                Some(&site.id),
                Some(&site.server_id),
                None,
            )
            .await?
    };
    audit(
        &state.db,
        Some(&caller.id),
        "site.basic_auth_clear",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(job_response(None, &job))))
}
