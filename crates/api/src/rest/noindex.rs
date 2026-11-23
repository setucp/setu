//! `GET` / `PUT /api/v1/sites/{id}/noindex` (0.1.7): ask search engines to
//! stay away from a site (a staging site). On, the site's route sets
//! `X-Robots-Tag: noindex, nofollow` and answers `/robots.txt` with a
//! disallow-all file. A `PUT` stores the option and queues the
//! `route_update` job that rewrites the route, in one transaction.

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

/// `PUT /sites/{id}/noindex` body.
#[derive(Debug, Deserialize, ToSchema)]
pub struct NoindexRequest {
    pub enabled: bool,
}

/// The option as stored.
#[derive(Debug, Serialize, ToSchema)]
pub struct NoindexResponse {
    pub enabled: bool,
}

/// The option after a change and the route job that applies it.
#[derive(Debug, Serialize, ToSchema)]
pub struct NoindexSetResponse {
    pub enabled: bool,
    pub job: JobOut,
}

/// Whether the site asks search engines to stay away.
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/noindex",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = NoindexResponse),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn get(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<Json<NoindexResponse>, AppError> {
    require_role(&session, Role::Viewer).await?;
    let site = state.db.sites().get(&id).await?;
    Ok(Json(NoindexResponse {
        enabled: state.db.options().noindex(&site.id).await?,
    }))
}

/// Turn the option on or off (operator and above); queues a route update.
/// 409 (and nothing stored) while the site has an active job.
#[utoipa::path(
    put,
    path = "/api/v1/sites/{id}/noindex",
    params(("id" = String, Path, description = "Site id")),
    request_body = NoindexRequest,
    responses(
        (status = 202, body = NoindexSetResponse),
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
    super::ApiJson(body): super::ApiJson<NoindexRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::conflict(format!(
            "site cannot change its noindex option from status {}",
            site.status
        )));
    }
    require_no_running_job(&state.db, &site.id).await?;
    super::sites::require_agent_with_route_options(&state.db, &site.server_id).await?;
    let job = state
        .db
        .options()
        .set_noindex_with_route_job(&site.id, &site.server_id, body.enabled)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.noindex_set",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(NoindexSetResponse {
            enabled: body.enabled,
            job: JobOut::from_row(&job),
        }),
    ))
}
