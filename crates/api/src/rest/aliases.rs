//! `GET` / `PUT /api/v1/sites/{id}/aliases` (0.1.7, issue #18): extra
//! hostnames of a site, each redirecting to the canonical domain or serving
//! the site. A `PUT` replaces the list and queues the `route_update` job
//! that rewrites the site's Caddy routes, in one transaction.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use setu_db::{SiteAlias, MAX_ALIASES};
use utoipa::ToSchema;

use crate::auth::{audit, require_role, ClientIp, Role, Session};
use crate::state::AppState;

use super::sites::{refuse_reserved_domain, require_no_running_job, JobOut};

/// One alias: a hostname and how it answers.
#[derive(Debug, Clone, Deserialize, Serialize, ToSchema)]
pub struct AliasItem {
    pub host: String,
    /// `redirect` (308 to the site's domain) or `serve` (the site answers).
    pub mode: String,
}

/// `PUT /sites/{id}/aliases` body: the whole new list.
#[derive(Debug, Deserialize, ToSchema)]
pub struct AliasesRequest {
    pub aliases: Vec<AliasItem>,
}

/// The aliases of a site.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasesResponse {
    pub aliases: Vec<AliasItem>,
}

/// The aliases after a change and the route job that applies them.
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasesSetResponse {
    pub aliases: Vec<AliasItem>,
    pub job: JobOut,
}

fn out(rows: Vec<SiteAlias>) -> Vec<AliasItem> {
    rows.into_iter()
        .map(|a| AliasItem {
            host: a.host,
            mode: a.mode,
        })
        .collect()
}

/// The site's aliases.
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/aliases",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = AliasesResponse),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn get(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<Json<AliasesResponse>, AppError> {
    require_role(&session, Role::Viewer).await?;
    let site = state.db.sites().get(&id).await?;
    Ok(Json(AliasesResponse {
        aliases: out(state.db.aliases().list(&site.id).await?),
    }))
}

/// Replace the site's aliases (operator and above); queues a route update.
/// 400 for a bad host or mode, 409 (and nothing stored) when a host is
/// another site's domain or alias, or the site has an active job.
#[utoipa::path(
    put,
    path = "/api/v1/sites/{id}/aliases",
    params(("id" = String, Path, description = "Site id")),
    request_body = AliasesRequest,
    responses(
        (status = 202, body = AliasesSetResponse),
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
    super::ApiJson(body): super::ApiJson<AliasesRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    if body.aliases.len() > MAX_ALIASES {
        return Err(AppError::validation(format!(
            "at most {MAX_ALIASES} aliases per site"
        )));
    }
    let mut rows: Vec<SiteAlias> = Vec::new();
    for a in &body.aliases {
        let host = setu_common::domain::normalise(&a.host).map_err(|e| {
            AppError::validation(format!(
                "{e}: alias must be a bare hostname like example.com"
            ))
        })?;
        if a.mode != "redirect" && a.mode != "serve" {
            return Err(AppError::validation(
                "alias mode must be \"redirect\" or \"serve\"",
            ));
        }
        if host == site.domain {
            return Err(AppError::validation(format!(
                "{host} is the site's own domain"
            )));
        }
        if rows.iter().any(|r| r.host == host) {
            return Err(AppError::validation(format!("{host} is listed twice")));
        }
        refuse_reserved_domain(&state, &host).await?;
        rows.push(SiteAlias {
            host,
            mode: a.mode.clone(),
        });
    }
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::conflict(format!(
            "site cannot change its aliases from status {}",
            site.status
        )));
    }
    require_no_running_job(&state.db, &site.id).await?;
    super::sites::require_agent_with_route_options(&state.db, &site.server_id).await?;
    let job = state
        .db
        .aliases()
        .replace_with_route_job(&site.id, &site.server_id, &rows)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.aliases_set",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(AliasesSetResponse {
            aliases: out(rows),
            job: JobOut::from_row(&job),
        }),
    ))
}
