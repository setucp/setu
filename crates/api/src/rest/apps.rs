//! `GET /api/v1/apps`, `POST /apps/validate`, `POST /apps`,
//! `GET /api/v1/apps/{id}/versions`.
//!
//! Publishing accepts either pasted TOML or a URL. URL fetches carry the
//! same guard rules as the installer: public hosts only (private, loopback,
//! link-local and other special ranges are rejected before connecting), a
//! 1 MiB response cap and a 10 s timeout, with redirects disabled so a
//! public URL cannot bounce to an internal one.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::{
    extract::{ConnectInfo, Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use tower_sessions::Session;
use utoipa::ToSchema;

use crate::auth::{audit, audit_ip, require_role, Role};
use crate::state::AppState;

use super::ApiJson;

/// 1 MiB manifest cap; 10 s fetch timeout.
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize, ToSchema)]
pub struct AppOut {
    pub id: String,
    pub name: String,
    pub homepage: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppListOut {
    pub apps: Vec<AppOut>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppVersionOut {
    pub id: String,
    pub app_id: String,
    pub version: String,
    pub channel: String,
    pub published_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AppVersionListOut {
    pub versions: Vec<AppVersionOut>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ValidateRequest {
    pub toml: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ManifestErrorOut {
    pub path: String,
    pub line: Option<usize>,
    pub message: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ValidateResponse {
    pub ok: bool,
    pub errors: Vec<ManifestErrorOut>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct PublishRequest {
    /// Pasted manifest TOML. Exactly one of `toml` / `url` is required.
    pub toml: Option<String>,
    /// URL to fetch the manifest from (public hosts only).
    pub url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PublishResponse {
    pub app: AppOut,
    pub version: AppVersionOut,
}

/// List catalogue apps (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/apps",
    responses(
        (status = 200, body = AppListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let apps = state.db.apps().list().await?;
    Ok(Json(AppListOut {
        apps: apps
            .into_iter()
            .map(|a| AppOut {
                id: a.id,
                name: a.name,
                homepage: a.homepage,
            })
            .collect(),
    }))
}

/// Validate pasted TOML without storing it (operator and above). Always
/// 200: validity is in the body (`ok` + `errors`).
#[utoipa::path(
    post,
    path = "/api/v1/apps/validate",
    request_body = ValidateRequest,
    responses(
        (status = 200, body = ValidateResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
    )
)]
pub async fn validate(
    State(state): State<AppState>,
    session: Session,
    ApiJson(body): ApiJson<ValidateRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Operator).await?;
    Ok(Json(validate_toml(&body.toml)))
}

/// Publish a version from pasted TOML or a URL (operator and above).
#[utoipa::path(
    post,
    path = "/api/v1/apps",
    request_body = PublishRequest,
    responses(
        (status = 201, body = PublishResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
    )
)]
pub async fn publish(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<PublishRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let src = match (body.toml, body.url) {
        (Some(toml), None) => toml,
        (None, Some(url)) => fetch_manifest_url(&url).await?,
        (Some(_), Some(_)) => {
            return Err(AppError::validation("provide exactly one of toml, url"));
        }
        (None, None) => {
            return Err(AppError::validation("provide toml or url"));
        }
    };
    let manifest = parse_or_validation(&src)?;
    let json = serde_json::to_value(&manifest).map_err(|e| {
        tracing::error!(error = %e, "manifest re-serialisation failed");
        AppError::internal()
    })?;
    let row = state.db.apps().upsert_version(&json).await?;
    let app = state.db.apps().get(&row.app_id).await?;
    audit(
        &state.db,
        Some(&caller.id),
        "app.publish",
        Some(&format!("{}/{}", row.app_id, row.version)),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(PublishResponse {
            app: AppOut {
                id: app.id,
                name: app.name,
                homepage: app.homepage,
            },
            version: AppVersionOut {
                id: row.id,
                app_id: row.app_id,
                version: row.version,
                channel: row.channel,
                published_at: row.published_at,
            },
        }),
    ))
}

/// One app's versions, newest first (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/apps/{id}/versions",
    params(("id" = String, Path, description = "App id")),
    responses(
        (status = 200, body = AppVersionListOut),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn versions(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    state.db.apps().get(&id).await?;
    let rows = state.db.apps().versions(&id).await?;
    Ok(Json(AppVersionListOut {
        versions: rows
            .into_iter()
            .map(|v| AppVersionOut {
                id: v.id,
                app_id: v.app_id,
                version: v.version,
                channel: v.channel,
                published_at: v.published_at,
            })
            .collect(),
    }))
}

fn validate_toml(src: &str) -> ValidateResponse {
    match setu_manifest::Manifest::parse(src) {
        Ok(_) => ValidateResponse {
            ok: true,
            errors: Vec::new(),
        },
        Err(errors) => ValidateResponse {
            ok: false,
            errors: errors
                .into_iter()
                .map(|e| ManifestErrorOut {
                    path: e.path,
                    line: e.line,
                    message: e.message,
                })
                .collect(),
        },
    }
}

fn parse_or_validation(src: &str) -> Result<setu_manifest::Manifest, AppError> {
    setu_manifest::Manifest::parse(src).map_err(|errors| {
        let first = errors
            .iter()
            .map(|e| e.message.clone())
            .collect::<Vec<_>>()
            .join("; ");
        AppError::validation(format!("invalid manifest: {first}"))
    })
}

/// True for addresses a manifest URL must never resolve to: loopback,
/// private, link-local, unspecified, broadcast, multicast, documentation,
/// benchmarking (198.18.0.0/15), reserved (240.0.0.0/4), software-scope
/// (0.0.0.0/8) and carrier-grade-NAT ranges (v4, incl. the cloud metadata
/// address 169.254.169.254 which is link-local), and the v6 equivalents.
fn is_non_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let raw = u32::from(v4);
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_documentation()
                // Explicit: the cloud instance-metadata endpoint.
                || ip == IpAddr::from([169, 254, 169, 254])
                // 0.0.0.0/8 software scope (`is_unspecified` is only .0.0.0).
                || v4.octets()[0] == 0
                // 198.18.0.0/15 benchmarking (`is_benchmarking` is nightly).
                || (raw & 0xfffe_0000) == 0xc612_0000
                // 240.0.0.0/4 reserved/future use (`is_reserved` is nightly).
                || (raw & 0xf000_0000) == 0xf000_0000
                // 100.64.0.0/10 carrier-grade NAT.
                || (raw & 0xffc0_0000) == 0x6440_0000
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // 2001:db8::/32 documentation (`is_documentation` is nightly
                // for v6).
                || (v6.segments()[0] == 0x2001 && v6.segments()[1] == 0x0db8)
                || (v6.segments()[0] & 0xffc0) == 0xfe80
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                || v6
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_non_public(IpAddr::V4(v4)))
        }
    }
}

/// Fetch a manifest with the installer-grade guards: http(s) only, host
/// must resolve to public addresses only, redirects disabled (a public URL
/// must not bounce to an internal one), 10 s timeout, 1 MiB cap.
///
/// Ruling (Task 7 review, SSRF residual TOCTOU): the `lookup_host` check
/// below runs before `reqwest`'s own internal resolution, so a hostile DNS
/// that flips answers between the two lookups could still steer the fetch
/// to a private address. Pinning the fetch to the vetted IP literal would
/// break https certificate verification (SNI/SAN mismatch against the IP),
/// so the residual is accepted: EVERY resolved address must be public
/// (not just the first), redirects stay disabled, and the timeout + size
/// cap apply. Exploiting the remainder needs attacker-controlled DNS plus
/// winning a race on every fetch.
async fn fetch_manifest_url(url_str: &str) -> Result<String, AppError> {
    let url =
        url::Url::parse(url_str).map_err(|_| AppError::validation("url is not a valid URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(AppError::validation("url must be http or https"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::validation("url must have a host"))?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AppError::validation("url must have a port"))?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .map_err(|_| AppError::validation("cannot resolve manifest host"))?
        .collect();
    if addrs.is_empty() {
        return Err(AppError::validation("cannot resolve manifest host"));
    }
    for addr in &addrs {
        if is_non_public(addr.ip()) {
            return Err(AppError::validation(
                "refusing to fetch manifest from a private address",
            ));
        }
    }
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| {
            tracing::error!(error = %e, "manifest fetch client failed");
            AppError::internal()
        })?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|_| AppError::validation("failed to fetch manifest"))?;
    if !resp.status().is_success() {
        return Err(AppError::validation(format!(
            "manifest fetch failed with status {}",
            resp.status()
        )));
    }
    let mut bytes = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| AppError::validation("failed to fetch manifest"))?;
        if bytes.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(AppError::validation("manifest exceeds 1 MiB"));
        }
        bytes.extend_from_slice(&chunk);
    }
    String::from_utf8(bytes).map_err(|_| AppError::validation("manifest is not valid UTF-8"))
}
