//! `GET /api/v1/domain-check?domain=…&server_id=…` (0.1.7): the DNS
//! pre-check the site form shows before provisioning. It reads DNS only;
//! nothing is stored and no job is queued. Findings come from
//! [`crate::dns::assess`].

use axum::{
    extract::{Query, State},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use utoipa::{IntoParams, ToSchema};

use crate::auth::{require_role, Role, Session};
use crate::dns::{assess, Finding, Level};
use crate::state::AppState;

/// Query of `GET /domain-check`.
#[derive(Debug, Deserialize, IntoParams)]
pub struct DomainCheckQuery {
    /// The site's domain, as typed in the form.
    pub domain: String,
    /// The server the site would be provisioned on.
    pub server_id: String,
}

/// Result of the pre-check.
#[derive(Debug, Serialize, ToSchema)]
pub struct DomainCheckOut {
    /// The normalised domain that was checked.
    pub domain: String,
    /// The server's public address, when its agent has reported one.
    pub server_ip: Option<String>,
    /// The findings, in a fixed order.
    pub findings: Vec<Finding>,
    /// True when no finding is an error (warnings do not block).
    pub ok: bool,
}

/// Check where a domain points before a site is created (operator and
/// above). A bad domain is a 400, an unknown server a 404; a name that
/// does not resolve is a normal 200 with an error finding.
#[utoipa::path(
    get,
    path = "/api/v1/domain-check",
    params(DomainCheckQuery),
    responses(
        (status = 200, body = DomainCheckOut),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn check(
    State(state): State<AppState>,
    session: Session,
    Query(query): Query<DomainCheckQuery>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Operator).await?;
    let domain = setu_common::domain::normalise(&query.domain).map_err(|e| {
        AppError::validation(format!(
            "{e}: domain must be a bare hostname like example.com"
        ))
    })?;
    let server = state.db.servers().get(&query.server_id).await?;
    let server_ip = server
        .public_ip
        .as_deref()
        .and_then(|ip| ip.trim().parse().ok());
    let answers = state.resolver.lookup(&domain).await;
    let findings = assess(&answers, server_ip);
    let ok = findings.iter().all(|f| f.level != Level::Error);
    Ok(Json(DomainCheckOut {
        domain,
        server_ip: server_ip.map(|ip| ip.to_string()),
        findings,
        ok,
    }))
}
