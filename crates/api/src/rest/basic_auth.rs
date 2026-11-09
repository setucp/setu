//! `GET/PUT/DELETE /api/v1/sites/{id}/basic-auth` (0.1.7): HTTP basic
//! auth in front of one site, enforced by Caddy. The password is hashed
//! with bcrypt (the format Caddy reads) and only the hash is stored; no
//! response ever carries a password or a hash. A change enqueues a
//! `route_update` job that rewrites the site's Caddy route.

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

/// `GET /sites/{id}/basic-auth`: the setting, or `null` when the site is open.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BasicAuthGetOut {
    pub basic_auth: Option<BasicAuthOut>,
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

/// `PUT`/`DELETE` result: the setting after the change and the route job.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BasicAuthJobResponse {
    pub basic_auth: Option<BasicAuthOut>,
    pub job: JobOut,
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

async fn enqueue_route(state: &AppState, site: &setu_db::Site) -> Result<JobOut, AppError> {
    let job = state
        .db
        .jobs()
        .enqueue("route_update", Some(&site.id), Some(&site.server_id), None)
        .await?;
    Ok(JobOut::from_row(&job))
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
    Ok(Json(BasicAuthGetOut {
        basic_auth: row.as_ref().map(out),
    }))
}

/// Set or replace the site's basic auth (operator and above); enqueues a
/// route update.
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
    let row = state
        .db
        .basic_auths()
        .set(&site.id, &body.username, &hash, state.clock.now())
        .await?;
    let job = enqueue_route(&state, &site).await?;
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
        Json(BasicAuthJobResponse {
            basic_auth: Some(out(&row)),
            job,
        }),
    ))
}

/// Remove the site's basic auth (operator and above); 404 when none.
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
    if !state.db.basic_auths().clear(&site.id).await? {
        return Err(AppError::not_found("this site has no basic auth"));
    }
    let job = enqueue_route(&state, &site).await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.basic_auth_clear",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(BasicAuthJobResponse {
            basic_auth: None,
            job,
        }),
    ))
}
