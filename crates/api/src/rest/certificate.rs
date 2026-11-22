//! `GET /api/v1/sites/{id}/certificate` and `POST …/certificate/retry`
//! (0.1.7): the TLS certificate Caddy serves for a site's domain, read
//! live from the agent (nothing is stored), and a retry that makes Caddy
//! attempt issuance at once by rewriting the site's route (a
//! `route_update` job; Caddy is not restarted).

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use setu_common::AppError;
use setu_proto::CertOutput;
use utoipa::ToSchema;

use crate::auth::{audit, require_role, ClientIp, Role, Session};
use crate::state::AppState;

use super::sites::{require_no_running_job, JobOut};

/// Days before expiry at which the state turns `expiring`.
const EXPIRING_DAYS: i64 = 14;

/// Where the certificate stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CertState {
    /// No publicly issued certificate yet (none served, or Caddy's own).
    Pending,
    /// A public certificate with more than 14 days left.
    Valid,
    /// A public certificate that ends within 14 days.
    Expiring,
    /// The served certificate has ended.
    Expired,
    /// The agent could not be asked.
    Unknown,
}

/// The certificate of a site's domain.
#[derive(Debug, Serialize, ToSchema)]
pub struct CertificateOut {
    pub domain: String,
    pub state: CertState,
    pub issuer: Option<String>,
    pub not_before: Option<DateTime<Utc>>,
    pub not_after: Option<DateTime<Utc>>,
    /// Whole days until `not_after` (negative once expired).
    pub days_left: Option<i64>,
    /// Why no certificate was read, when none was.
    pub error: Option<String>,
    /// When the agent was asked.
    pub checked_at: DateTime<Utc>,
}

fn at(secs: Option<i64>) -> Option<DateTime<Utc>> {
    secs.and_then(|s| DateTime::from_timestamp(s, 0))
}

/// Classify what the agent read.
#[must_use]
pub fn classify(cert: &CertOutput, now: DateTime<Utc>) -> CertState {
    if !cert.served || !cert.public {
        return CertState::Pending;
    }
    let Some(end) = at(cert.not_after) else {
        return CertState::Pending;
    };
    if end <= now {
        CertState::Expired
    } else if end - now < Duration::days(EXPIRING_DAYS) {
        CertState::Expiring
    } else {
        CertState::Valid
    }
}

/// The certificate Caddy serves for the site's domain (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/certificate",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = CertificateOut),
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
    let now = state.clock.now();
    let call = state
        .agent
        .call(
            &site.server_id,
            Some(&site.short_id),
            setu_proto::Command::CertStatus {
                site: site.short_id.clone(),
                domain: site.domain.clone(),
            },
        )
        .await;
    let (state_, cert, error) = match call {
        Ok(result) if result.ok => {
            let cert: CertOutput = serde_json::from_value(result.output).unwrap_or_default();
            (classify(&cert, now), cert.clone(), cert.error)
        }
        Ok(result) => {
            tracing::warn!(site = %site.short_id, error = ?result.error, "certificate: cert_status failed");
            (
                CertState::Unknown,
                CertOutput::default(),
                Some("the agent could not read the certificate".to_string()),
            )
        }
        Err(e) => {
            tracing::warn!(site = %site.short_id, error = %e, "certificate: agent call failed");
            (
                CertState::Unknown,
                CertOutput::default(),
                Some("the agent did not answer".to_string()),
            )
        }
    };
    let not_after = at(cert.not_after);
    Ok(Json(CertificateOut {
        domain: site.domain,
        state: state_,
        issuer: cert.issuer,
        not_before: at(cert.not_before),
        not_after,
        days_left: not_after.map(|end| (end - now).num_days()),
        error,
        checked_at: now,
    }))
}

/// Make Caddy try to get the certificate now (operator and above): queues
/// a `route_update` job that rewrites the site's route. 409 when the site
/// is not running or stopped, or has an active job.
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/certificate/retry",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = JobOut),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn retry(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::conflict(format!(
            "site cannot retry its certificate from status {}",
            site.status
        )));
    }
    require_no_running_job(&state.db, &site.id).await?;
    let job = state
        .db
        .jobs()
        .enqueue(
            setu_db::ROUTE_JOB,
            Some(&site.id),
            Some(&site.server_id),
            None,
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.certificate_retry",
        Some(&site.id),
        ip.as_deref(),
    )
    .await;
    Ok((StatusCode::ACCEPTED, Json(JobOut::from_row(&job))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cert(public: bool, days: i64, now: DateTime<Utc>) -> CertOutput {
        CertOutput {
            served: true,
            public,
            not_before: Some((now - Duration::days(30)).timestamp()),
            not_after: Some((now + Duration::days(days)).timestamp()),
            ..CertOutput::default()
        }
    }

    #[test]
    fn states_follow_public_issuance_and_days_left() {
        let now = Utc::now();
        assert_eq!(classify(&CertOutput::default(), now), CertState::Pending);
        assert_eq!(classify(&cert(false, 90, now), now), CertState::Pending);
        assert_eq!(classify(&cert(true, 90, now), now), CertState::Valid);
        assert_eq!(classify(&cert(true, 13, now), now), CertState::Expiring);
        assert_eq!(classify(&cert(true, -1, now), now), CertState::Expired);
    }
}
