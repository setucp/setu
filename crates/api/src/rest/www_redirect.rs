//! `PUT /api/v1/sites/{id}/www-redirect` (0.1.7): turn the `www.` → apex
//! redirect of a site on or off. The setting is stored with a
//! `route_update` job that rewrites the site's Caddy routes, in one
//! transaction. Sites created before 0.1.7 have it off.

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

/// `PUT /sites/{id}/www-redirect` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct WwwRedirectRequest {
    pub enabled: bool,
}

/// The setting after the change and the route job that applies it.
#[derive(Debug, Serialize, ToSchema)]
pub struct WwwRedirectResponse {
    pub www_redirect: bool,
    pub job: JobOut,
}

/// Turn the `www.` redirect on or off (operator and above); queues a route
/// update. 409 (and nothing stored) when the site has an active job or is
/// not running or stopped.
#[utoipa::path(
    put,
    path = "/api/v1/sites/{id}/www-redirect",
    params(("id" = String, Path, description = "Site id")),
    request_body = WwwRedirectRequest,
    responses(
        (status = 202, body = WwwRedirectResponse),
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
    super::ApiJson(body): super::ApiJson<WwwRedirectRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::conflict(format!(
            "site cannot change its www redirect from status {}",
            site.status
        )));
    }
    require_no_running_job(&state.db, &site.id).await?;
    let job = state
        .db
        .sites()
        .set_www_redirect_with_route_job(&site.id, &site.server_id, body.enabled)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        if body.enabled {
            "site.www_redirect_on"
        } else {
            "site.www_redirect_off"
        },
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(WwwRedirectResponse {
            www_redirect: body.enabled,
            job: JobOut::from_row(&job),
        }),
    ))
}
