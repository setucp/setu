//! `GET /api/v1/apps`, `POST /apps/validate`, `POST /apps`,
//! `GET /api/v1/apps/{id}/versions`, `POST /apps/{id}/check`,
//! `DELETE /apps/{id}/source`.
//!
//! Publishing accepts either pasted TOML or a URL. URL fetches carry the
//! same guard rules as the installer: public hosts only (private, loopback,
//! link-local and other special ranges are rejected before connecting), a
//! 1 MiB response cap and a 10 s timeout, with redirects disabled so a
//! public URL cannot bounce to an internal one. An app published from a
//! URL follows it (0.1.5 §1, [`crate::follow`]).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    Json,
};
use futures::StreamExt as _;
use serde::{Deserialize, Serialize};
use setu_common::AppError;
use utoipa::ToSchema;

use crate::auth::{audit, require_role, ClientIp, Role, Session};
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
    /// The manifest URL the app follows, if any.
    pub source: Option<AppSourceOut>,
}

/// A followed manifest URL and its last check.
#[derive(Debug, Serialize, ToSchema)]
pub struct AppSourceOut {
    pub url: String,
    pub checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_error: Option<String>,
}

impl From<setu_db::AppSource> for AppSourceOut {
    fn from(s: setu_db::AppSource) -> Self {
        Self {
            url: s.url,
            checked_at: s.checked_at,
            last_error: s.last_error,
        }
    }
}

/// `POST /apps/{id}/check`: the version the check published, if any.
#[derive(Debug, Serialize, ToSchema)]
pub struct CheckOut {
    pub published: Option<String>,
    pub source: AppSourceOut,
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
    require_role(&session, Role::Viewer).await?;
    let apps = state.db.apps().list().await?;
    let mut sources: std::collections::HashMap<String, setu_db::AppSource> = state
        .db
        .app_sources()
        .list()
        .await?
        .into_iter()
        .map(|s| (s.app_id.clone(), s))
        .collect();
    Ok(Json(AppListOut {
        apps: apps
            .into_iter()
            .map(|a| AppOut {
                source: sources.remove(&a.id).map(Into::into),
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
    session: Session,
    ApiJson(body): ApiJson<ValidateRequest>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Operator).await?;
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
    ip: ClientIp,
    ApiJson(body): ApiJson<PublishRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let src = match (body.toml, body.url.as_deref()) {
        (Some(toml), None) => toml,
        (None, Some(url)) => state.fetcher.fetch(url).await?,
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
    let source = match body.url.as_deref() {
        Some(url) => Some(state.db.app_sources().set(&app.id, url).await?),
        None => state.db.app_sources().get(&app.id).await?,
    };
    audit(
        &state.db,
        Some(&caller.id),
        "app.publish",
        Some(&format!("{}/{}", row.app_id, row.version)),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(PublishResponse {
            app: AppOut {
                id: app.id,
                name: app.name,
                homepage: app.homepage,
                source: source.map(Into::into),
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
    require_role(&session, Role::Viewer).await?;
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

/// Fetch the app's followed URL now and publish its version when new
/// (operator and above).
#[utoipa::path(
    post,
    path = "/api/v1/apps/{id}/check",
    params(("id" = String, Path, description = "App id")),
    responses(
        (status = 200, body = CheckOut),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn check(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Operator).await?;
    let source = state
        .db
        .app_sources()
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("app follows no manifest URL"))?;
    let published = crate::follow::check(
        &state.db,
        state.fetcher.as_ref(),
        state.clock.as_ref(),
        &source,
    )
    .await?;
    let source = state
        .db
        .app_sources()
        .get(&id)
        .await?
        .ok_or_else(|| AppError::not_found("app follows no manifest URL"))?;
    Ok(Json(CheckOut {
        published,
        source: source.into(),
    }))
}

/// Stop following the app's manifest URL (operator and above).
#[utoipa::path(
    delete,
    path = "/api/v1/apps/{id}/source",
    params(("id" = String, Path, description = "App id")),
    responses(
        (status = 204),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn clear_source(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    if !state.db.app_sources().clear(&id).await? {
        return Err(AppError::not_found("app follows no manifest URL"));
    }
    audit(
        &state.db,
        Some(&caller.id),
        "app.source_clear",
        Some(&id),
        ip.as_deref(),
    )
    .await;
    Ok(StatusCode::NO_CONTENT)
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

pub(crate) fn parse_or_validation(src: &str) -> Result<setu_manifest::Manifest, AppError> {
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
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_unspecified()
                // 64:ff9b::/96 NAT64 and 64:ff9b:1::/48 local-use NAT64:
                // a gateway forwards them to an embedded v4 address.
                || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2..6] == [0, 0, 0, 0])
                || (seg[0] == 0x0064 && seg[1] == 0xff9b && seg[2] == 0x0001)
                // 2002::/16 6to4: relays to the embedded v4 address.
                || seg[0] == 0x2002
                // ::a.b.c.d v4-compatible (deprecated, still routable by
                // some stacks).
                || (seg[..6] == [0, 0, 0, 0, 0, 0] && !v6.is_loopback())
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
/// The fetch is pinned to the addresses that were vetted
/// (`ClientBuilder::resolve_to_addrs`), so a hostile DNS that answers
/// differently on a second lookup cannot steer it to a private address;
/// the URL keeps its host name, so https still verifies the certificate
/// against it (SNI). This closes the TOCTOU the Task 7 ruling accepted
/// (M3, R138).
pub(crate) async fn fetch_manifest_url(url_str: &str) -> Result<String, AppError> {
    let url =
        url::Url::parse(url_str).map_err(|_| AppError::validation("url is not a valid URL"))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(AppError::validation("url must be http or https"));
    }
    let host = url
        .host_str()
        .ok_or_else(|| AppError::validation("url must have a host"))?
        .to_string();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AppError::validation("url must have a port"))?;
    let addrs: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), port))
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
    fetch_pinned(url, &host, &addrs).await
}

/// GET `url` with `host` resolved only to `addrs` (no DNS of its own).
async fn fetch_pinned(url: url::Url, host: &str, addrs: &[SocketAddr]) -> Result<String, AppError> {
    let client = reqwest::Client::builder()
        .timeout(FETCH_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .resolve_to_addrs(host, addrs)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// v6 forms that reach a v4 address — NAT64 (`64:ff9b::/96`, and the
    /// local-use `64:ff9b:1::/48`), 6to4 (`2002::/16`), v4-mapped and
    /// v4-compatible — are refused like the private v4 ranges (M3).
    #[test]
    fn non_public_covers_nat64_6to4_and_mapped() {
        for bad in [
            "64:ff9b::a9fe:a9fe",
            "64:ff9b::808:808",
            "64:ff9b:1::1",
            "2002:a9fe:a9fe::1",
            "2002:808:808::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "fe80::1",
            "fc00::1",
            "::1",
        ] {
            let ip: IpAddr = bad.parse().expect(bad);
            assert!(is_non_public(ip), "{bad} must be refused");
        }
        for good in ["2606:4700::1111", "8.8.8.8", "::ffff:8.8.8.8"] {
            let ip: IpAddr = good.parse().expect(good);
            assert!(!is_non_public(ip), "{good} is public");
        }
    }

    /// The fetch connects to the vetted addresses, not to a second DNS
    /// answer: a name that does not resolve at all is fetched from the
    /// address it was pinned to (and keeps its name for TLS/SNI).
    #[tokio::test]
    async fn fetch_uses_the_vetted_addresses() {
        let app = axum::Router::new().route(
            "/m.toml",
            axum::routing::get(|| async { "name = \"pinned\"" }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(l, app).await.expect("serve") });
        let url = url::Url::parse(&format!("http://manifest.invalid:{}/m.toml", addr.port()))
            .expect("url");
        let body = fetch_pinned(url, "manifest.invalid", &[addr])
            .await
            .expect("pinned fetch");
        assert_eq!(body, "name = \"pinned\"");
    }
}

/// One `prompt` env rule of an app version: what an operator is asked
/// for. Carries no value, ever (#7).
#[derive(Debug, Serialize, ToSchema)]
pub struct PromptOut {
    pub name: String,
    pub label: String,
    pub secret: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct PromptListOut {
    pub prompts: Vec<PromptOut>,
}

/// The `prompt` rules of one version, by name (viewer and above). Other
/// rules (`value`, `generate`, `from`) are never included, and neither
/// is any stored answer.
#[utoipa::path(
    get,
    path = "/api/v1/apps/{id}/versions/{version}/prompts",
    params(
        ("id" = String, Path, description = "App id"),
        ("version" = String, Path, description = "App version"),
    ),
    responses(
        (status = 200, body = PromptListOut),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn version_prompts(
    State(state): State<AppState>,
    session: Session,
    Path((id, version)): Path<(String, String)>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, Role::Viewer).await?;
    let row = state.db.apps().version(&id, &version).await?;
    let manifest: setu_manifest::Manifest = serde_json::from_value(row.manifest).map_err(|e| {
        tracing::error!(error = %e, "stored manifest is corrupt");
        AppError::internal()
    })?;
    Ok(Json(PromptListOut {
        prompts: prompt_rules(&manifest),
    }))
}

fn prompt_rules(manifest: &setu_manifest::Manifest) -> Vec<PromptOut> {
    manifest
        .env
        .iter()
        .filter_map(|(name, spec)| match spec {
            setu_manifest::EnvSpec::Prompt(p) => Some(PromptOut {
                name: name.clone(),
                label: p.label.clone(),
                secret: p.secret,
            }),
            _ => None,
        })
        .collect()
}
