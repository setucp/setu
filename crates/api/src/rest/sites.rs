//! `GET/POST /api/v1/sites` and the per-site routes (spec §5/§6, plan
//! Task 9): restart/stop/start, update, retry, delete, env (masked),
//! logs (SSE). Every mutation writes `audit_log` and returns 202 with the
//! enqueued job; the background `Runner` (started by `setu serve` and by
//! `TestServer`) picks the job up.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use futures::channel::mpsc;
use futures::SinkExt;
use rand::SeedableRng;
use serde::{Deserialize, Serialize};
use setu_common::{AppError, ErrorCode};
use tower_sessions::Session;
use utoipa::ToSchema;

use crate::auth::{audit, audit_ip, require_role, Role};
use crate::state::AppState;

use super::ApiJson;

/// Masked placeholder returned for secret env values (the plaintext is
/// never sent back; `has_value` tells the UI a value exists).
pub const MASKED_VALUE: &str = "********";

/// One site row (the sealed `env` is never included; see `./env`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SiteOut {
    pub id: String,
    pub name: String,
    pub app_id: String,
    pub app_version: String,
    pub server_id: String,
    pub plan_id: String,
    pub domain: String,
    pub status: String,
    pub failed_step: Option<String>,
    pub port: i32,
    pub data_dir: String,
    pub db_name: String,
    pub owner_email: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl SiteOut {
    pub fn from_row(s: &setu_db::Site) -> Self {
        Self {
            id: s.id.clone(),
            name: s.name.clone(),
            app_id: s.app_id.clone(),
            app_version: s.app_version.clone(),
            server_id: s.server_id.clone(),
            plan_id: s.plan_id.clone(),
            domain: s.domain.clone(),
            status: s.status.clone(),
            failed_step: s.failed_step.clone(),
            port: s.port,
            // Derived (spec §4); WS8 drops it from the API.
            data_dir: format!("/srv/setu/sites/{}/data", s.short_id),
            db_name: s.db_name.clone(),
            owner_email: s.owner_email.clone(),
            created_at: s.created_at,
            updated_at: s.updated_at,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SiteListOut {
    pub sites: Vec<SiteOut>,
}

/// One job row. `payload` is never rendered (it may carry prompt
/// answers); poll `log` for progress.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct JobOut {
    pub id: String,
    pub kind: String,
    pub site_id: Option<String>,
    pub server_id: Option<String>,
    pub status: String,
    pub attempts: i32,
    pub log: String,
    pub error: Option<String>,
    pub worker: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub finished_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl JobOut {
    pub fn from_row(j: &setu_db::Job) -> Self {
        Self {
            id: j.id.clone(),
            kind: j.kind.clone(),
            site_id: j.site_id.clone(),
            server_id: j.server_id.clone(),
            status: j.status.clone(),
            attempts: j.attempts,
            log: j.log.clone(),
            error: j.error.clone(),
            worker: j.locked_by.clone(),
            created_at: j.created_at,
            started_at: j.started_at,
            finished_at: j.finished_at,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSiteRequest {
    pub name: String,
    pub app_id: String,
    pub version: String,
    pub plan_id: String,
    pub server_id: Option<String>,
    pub domain: String,
    pub owner_email: String,
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
}

// Holds prompt secrets: redact the values.
impl CreateSiteRequest {
    fn redacted(&self) -> String {
        format!(
            "CreateSiteRequest {{ name: {:?}, app_id: {:?}, version: {:?}, domain: {:?} }}",
            self.name, self.app_id, self.version, self.domain
        )
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateSiteResponse {
    pub site: SiteOut,
    pub job: JobOut,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSiteRequest {
    pub version: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SiteJobResponse {
    pub site: SiteOut,
    pub job: JobOut,
}

/// A bare 202 job body (delete: the site row is gone when the job
/// finishes, so no site is returned).
#[derive(Debug, Serialize, ToSchema)]
pub struct JobResponse {
    pub job: JobOut,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnvEntryOut {
    pub name: String,
    /// Plaintext for non-secret vars, [`MASKED_VALUE`] for secrets.
    pub value: String,
    pub masked: bool,
    pub secret: bool,
    pub once: bool,
    pub has_value: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnvGetOut {
    pub env: Vec<EnvEntryOut>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct EnvPutRequest {
    pub env: BTreeMap<String, String>,
}

// Holds env values: redact them.
impl EnvPutRequest {
    fn redacted(&self) -> String {
        let names: Vec<&String> = self.env.keys().collect();
        format!("EnvPutRequest {{ env keys: {names:?} }}")
    }
}

/// One stored env entry (`sites.env` JSON array).
struct StoredVar {
    name: String,
    value: String,
    secret: bool,
    once: bool,
}

fn stored_env(site: &setu_db::Site) -> Vec<StoredVar> {
    let Some(arr) = site.env.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|v| {
            Some(StoredVar {
                name: v.get("name")?.as_str()?.to_string(),
                value: v.get("value")?.as_str()?.to_string(),
                secret: v.get("secret").and_then(|b| b.as_bool()).unwrap_or(false),
                once: v.get("once").and_then(|b| b.as_bool()).unwrap_or(false),
            })
        })
        .collect()
}

/// A value no role ever reads back from `GET env`: flagged `secret`, or
/// sourced from `postgres_url` (it embeds the site's database password;
/// security finding 1), whatever the stored flag says.
fn always_masked(manifest: Option<&setu_manifest::Manifest>, var: &StoredVar) -> bool {
    var.secret
        || manifest
            .and_then(|m| m.env.get(&var.name))
            .is_some_and(|spec| {
                matches!(spec, setu_manifest::EnvSpec::From(f)
                    if f.from == setu_manifest::EnvSource::PostgresUrl)
            })
}

/// Open one stored env value (`v1:` or `plain:`); anything else is
/// corrupt storage and surfaces as `internal`.
fn open_stored(sealer: &setu_common::Sealer, value: &str) -> Result<String, AppError> {
    sealer.open(value).map_err(|e| {
        tracing::error!(error = %e, "stored env failed to open");
        AppError::internal()
    })
}

/// Load the validated manifest struct for an app version. Stored
/// manifests were validated at publish; a corrupt row is `internal`.
fn load_manifest(version: &setu_db::AppVersion) -> Result<setu_manifest::Manifest, AppError> {
    serde_json::from_value(version.manifest.clone()).map_err(|e| {
        tracing::error!(error = %e, "stored manifest is corrupt");
        AppError::internal()
    })
}

/// Reject overlapping work: any `queued`/`running` job for the site is a
/// 409 so two state machines never interleave on one site.
pub(crate) async fn require_no_running_job(
    db: &setu_db::Db,
    site_id: &str,
) -> Result<(), AppError> {
    if db.jobs().active_for_site(site_id).await?.is_some() {
        return Err(AppError::conflict("a job is already running for this site"));
    }
    Ok(())
}

/// Prompt answers from the site's latest `provision_site` job payload
/// (stored by `POST /sites`). Later jobs (update/backup/restore/retry)
/// carry them so the state machines can re-resolve prompt env vars.
pub(crate) async fn provision_prompts(
    db: &setu_db::Db,
    site_id: &str,
) -> Result<serde_json::Value, AppError> {
    Ok(db
        .jobs()
        .list_recent(50)
        .await?
        .into_iter()
        .filter(|j| j.site_id.as_deref() == Some(site_id) && j.kind == "provision_site")
        .find_map(|j| j.payload.get("prompts").cloned())
        .unwrap_or(serde_json::json!({})))
}

/// List sites (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites",
    responses(
        (status = 200, body = SiteListOut),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn list(
    State(state): State<AppState>,
    session: Session,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let sites = state.db.sites().list().await?;
    Ok(Json(SiteListOut {
        sites: sites.iter().map(SiteOut::from_row).collect(),
    }))
}

/// Create a site and enqueue its provision job (operator and above).
///
/// Validates the domain (unique → 409), the app version (must exist and
/// parse; `image` manifests are rejected with `validation` — phase 2),
/// the plan (must allow the app) and the server (explicit, or the
/// online server with the most free memory). Resolves env with the
/// operator-supplied `prompts`, seals secrets, stores the row as
/// `provisioning`, then enqueues `provision_site` carrying the prompts
/// (the runner re-resolves with them).
#[utoipa::path(
    post,
    path = "/api/v1/sites",
    request_body = CreateSiteRequest,
    responses(
        (status = 202, body = CreateSiteResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn create(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    ApiJson(body): ApiJson<CreateSiteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    tracing::debug!("site create {}", body.redacted());

    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AppError::validation("site name must not be empty"));
    }
    let domain = setu_common::domain::normalise(&body.domain).map_err(|e| {
        AppError::validation(format!(
            "{e}: domain must be a bare hostname like example.com"
        ))
    })?;
    if !body.owner_email.contains('@') {
        return Err(AppError::validation("owner_email must contain @"));
    }
    // Pre-check for the friendly 409 before resolving env; `create`
    // re-checks inside its transaction.
    if state
        .db
        .sites()
        .by_domain_excluding(&domain, "")
        .await?
        .is_some()
    {
        return Err(AppError::conflict("domain already in use"));
    }

    let app_version = state
        .db
        .apps()
        .version(&body.app_id, &body.version)
        .await
        .map_err(|e| {
            if e.code == ErrorCode::NotFound {
                AppError::validation(format!(
                    "unknown app version {}/{}",
                    body.app_id, body.version
                ))
            } else {
                e
            }
        })?;
    let manifest = load_manifest(&app_version)?;
    if manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let plan = state.db.plans().get(&body.plan_id).await.map_err(|e| {
        if e.code == ErrorCode::NotFound {
            AppError::validation(format!("unknown plan {}", body.plan_id))
        } else {
            e
        }
    })?;
    if !plan.allows(&body.app_id) {
        return Err(AppError::validation(format!(
            "plan does not allow app {}",
            body.app_id
        )));
    }
    // Resolve env now (prompts included) so `GET env` works immediately;
    // the provision job re-resolves with the same prompts from its
    // payload and stores the sealed copy again before the first agent
    // call.
    let db_password = {
        let mut bytes = [0u8; 24];
        rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
        hex::encode(bytes)
    };
    let site_url = format!("https://{domain}");
    // Port and dirs need the row first; resolve with placeholders and
    // re-resolve inside the job (authoritative). Only prompt-missing and
    // manifest errors surface here.
    let mut resolve_input = setu_manifest::ResolveInput {
        site_url,
        owner_email: body.owner_email.clone(),
        port: 20000,
        run_dir: std::path::PathBuf::from("/srv/setu/sites/pending/run"),
        data_dir: std::path::PathBuf::from("/srv/setu/sites/pending/data"),
        db_url: format!("postgres://pending:{db_password}@127.0.0.1:5432/pending"),
        prompts: body.prompts.clone(),
        rng: rand::rngs::StdRng::from_os_rng(),
    };
    let resolved =
        setu_manifest::resolve_env(&manifest, &mut resolve_input).map_err(|e| match &e {
            setu_manifest::ResolveError::MissingPrompt(_) => {
                AppError::validation(format!("prompt values missing: {e}"))
            }
            setu_manifest::ResolveError::Invalid(_, _) => AppError::validation(e.to_string()),
        })?;
    let sealed: serde_json::Value = {
        let arr: Vec<serde_json::Value> = resolved
            .iter()
            .map(|v| {
                serde_json::json!({
                    "name": v.name,
                    "value": state.sealer.store(&v.value, v.secret),
                    "secret": v.secret,
                    "once": v.once,
                })
            })
            .collect();
        serde_json::Value::Array(arr)
    };

    // Server pick, capacity (85 %), port and short id in one
    // transaction (spec §3); `db_name`/`db_role` are `site_<short>`.
    let site = state
        .db
        .sites()
        .create(&setu_db::NewSite {
            name: name.clone(),
            app_id: body.app_id.clone(),
            app_version: body.version.clone(),
            server_id: body.server_id.clone(),
            plan_id: plan.id.clone(),
            domain: domain.clone(),
            owner_email: body.owner_email.clone(),
            env_sealed: sealed,
            plan_memory_mb: i64::from(plan.memory_mb),
        })
        .await
        .map_err(|e| match e {
            // A named server that does not exist is the caller's error.
            setu_db::CreateError::NoServer if body.server_id.is_some() => {
                AppError::validation("unknown server")
            }
            e => AppError::from(e),
        })?;

    let job = state
        .db
        .jobs()
        .enqueue(
            "provision_site",
            Some(&site.id),
            Some(&site.server_id),
            Some(&serde_json::json!({"prompts": body.prompts})),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.create",
        Some(&site.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(CreateSiteResponse {
            site: SiteOut::from_row(&site),
            job: JobOut::from_row(&job),
        }),
    ))
}

/// One site (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = SiteOut),
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
    let site = state.db.sites().get(&id).await?;
    Ok(Json(SiteOut::from_row(&site)))
}

/// Enqueue a lifecycle job (`restart_site` | `stop_site` | `start_site`;
/// operator and above). Guards: no job already live for the site, and
/// the status transition must make sense.
async fn enqueue_lifecycle(
    state: &AppState,
    caller_id: &str,
    peer: SocketAddr,
    id: &str,
    kind: &str,
    action: &str,
) -> Result<(SiteOut, JobOut), AppError> {
    let site = state.db.sites().get(id).await?;
    require_no_running_job(&state.db, id).await?;
    let allowed = match kind {
        "restart_site" => site.status == "running" || site.status == "stopped",
        "stop_site" => site.status == "running",
        "start_site" => site.status == "stopped",
        _ => false,
    };
    if !allowed {
        return Err(AppError::conflict(format!(
            "site cannot {action} from status {}",
            site.status
        )));
    }
    let job = state
        .db
        .jobs()
        .enqueue(kind, Some(&site.id), Some(&site.server_id), None)
        .await?;
    audit(
        &state.db,
        Some(caller_id),
        action,
        Some(&site.id),
        audit_ip(None, Some(peer)).as_deref(),
    )
    .await;
    Ok((SiteOut::from_row(&site), JobOut::from_row(&job)))
}

/// Restart: stop then start via the runner (operator and above).
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/restart",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = SiteJobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn restart(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let (site, job) = enqueue_lifecycle(
        &state,
        &caller.id,
        peer.0,
        &id,
        "restart_site",
        "site.restart",
    )
    .await?;
    Ok((StatusCode::ACCEPTED, Json(SiteJobResponse { site, job })))
}

/// Stop the unit (operator and above).
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/stop",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = SiteJobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn stop(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let (site, job) =
        enqueue_lifecycle(&state, &caller.id, peer.0, &id, "stop_site", "site.stop").await?;
    Ok((StatusCode::ACCEPTED, Json(SiteJobResponse { site, job })))
}

/// Start the unit (operator and above).
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/start",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = SiteJobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn start(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let (site, job) =
        enqueue_lifecycle(&state, &caller.id, peer.0, &id, "start_site", "site.start").await?;
    Ok((StatusCode::ACCEPTED, Json(SiteJobResponse { site, job })))
}

/// Update to another published version (operator and above): 202 with an
/// `update_site` job. 409 while any job is live for the site or the site
/// is not `running` (the runner would refuse it anyway).
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/update",
    params(("id" = String, Path, description = "Site id")),
    request_body = UpdateSiteRequest,
    responses(
        (status = 202, body = SiteJobResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn update(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateSiteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    require_no_running_job(&state.db, &id).await?;
    if site.status != "running" {
        return Err(AppError::conflict(format!(
            "site cannot update from status {}",
            site.status
        )));
    }
    // The target version must exist and parse; image targets are refused
    // here, before any job row exists.
    let target = state
        .db
        .apps()
        .version(&site.app_id, &body.version)
        .await
        .map_err(|e| {
            if e.code == ErrorCode::NotFound {
                AppError::validation(format!("unknown app version {}", body.version))
            } else {
                e
            }
        })?;
    let manifest = load_manifest(&target)?;
    if manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let job = state
        .db
        .jobs()
        .enqueue(
            "update_site",
            Some(&site.id),
            Some(&site.server_id),
            Some(&serde_json::json!({
                "to_version": body.version,
                "prompts": provision_prompts(&state.db, &site.id).await?,
            })),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.update",
        Some(&site.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(SiteJobResponse {
            site: SiteOut::from_row(&site),
            job: JobOut::from_row(&job),
        }),
    ))
}

/// Retry a `failed` site: back to `provisioning` with a fresh
/// `provision_site` job carrying the original prompts (operator+).
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/retry",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = SiteJobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn retry(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    require_no_running_job(&state.db, &id).await?;
    if site.status != "failed" {
        return Err(AppError::conflict(format!(
            "only failed sites can be retried (status: {})",
            site.status
        )));
    }
    // Recover the prompts the site was created with: the last
    // `provision_site` job for this site carries them.
    let prompts = provision_prompts(&state.db, &site.id).await?;
    let job = state
        .db
        .jobs()
        .enqueue(
            "provision_site",
            Some(&site.id),
            Some(&site.server_id),
            Some(&serde_json::json!({"prompts": prompts})),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.retry",
        Some(&site.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(SiteJobResponse {
            site: SiteOut::from_row(&site),
            job: JobOut::from_row(&job),
        }),
    ))
}

/// Delete the site (operator and above): 202 with a `delete_site` job.
/// The row disappears when the job finishes; until then `GET` still
/// finds it (`deleting`). 409 while another job is live.
#[utoipa::path(
    delete,
    path = "/api/v1/sites/{id}",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 202, body = JobResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn delete(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    require_no_running_job(&state.db, &id).await?;
    let job = state
        .db
        .jobs()
        .enqueue("delete_site", Some(&site.id), Some(&site.server_id), None)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.delete",
        Some(&site.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    Ok((
        StatusCode::ACCEPTED,
        Json(JobResponse {
            job: JobOut::from_row(&job),
        }),
    ))
}

/// Masked env list (viewer and above): names plus `secret`/`once`
/// flags and `has_value`; secret values render as [`MASKED_VALUE`],
/// never plaintext.
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/env",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = EnvGetOut),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn env_get(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let site = state.db.sites().get(&id).await?;
    // The manifest is only consulted to mask `postgres_url` sources; a
    // missing version masks on the stored flag alone.
    let manifest = state
        .db
        .apps()
        .version(&site.app_id, &site.app_version)
        .await
        .ok()
        .and_then(|v| load_manifest(&v).ok());
    let env = stored_env(&site)
        .into_iter()
        .map(|v| {
            let secret = always_masked(manifest.as_ref(), &v);
            Ok(EnvEntryOut {
                value: if secret {
                    MASKED_VALUE.to_string()
                } else {
                    open_stored(&state.sealer, &v.value)?
                },
                name: v.name,
                masked: secret,
                secret,
                once: v.once,
                has_value: true,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;
    Ok(Json(EnvGetOut { env }))
}

/// Update non-generated env vars (operator and above): only manifest
/// entries spelled `value` or `prompt` may change (`generate`/`from`
/// are derived and rejected with `validation`). Writes the sealed copy,
/// issues a synchronous `WriteEnv` to the agent, then enqueues
/// `restart_site` so the new values take effect.
#[utoipa::path(
    put,
    path = "/api/v1/sites/{id}/env",
    params(("id" = String, Path, description = "Site id")),
    request_body = EnvPutRequest,
    responses(
        (status = 202, body = SiteJobResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn env_put(
    State(state): State<AppState>,
    session: Session,
    peer: ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<EnvPutRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, &state.db, Role::Operator).await?;
    tracing::debug!("site env update {}", body.redacted());
    let site = state.db.sites().get(&id).await?;
    require_no_running_job(&state.db, &id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::conflict(format!(
            "site env cannot be updated from status {}",
            site.status
        )));
    }
    if body.env.is_empty() {
        return Err(AppError::validation("env must not be empty"));
    }
    let version = state
        .db
        .apps()
        .version(&site.app_id, &site.app_version)
        .await?;
    let manifest = load_manifest(&version)?;
    let plan = state.db.plans().get(&site.plan_id).await?;

    // Validate keys against the manifest: Value/Prompt only.
    let mut secrets: BTreeMap<String, bool> = BTreeMap::new();
    for key in body.env.keys() {
        let spec = manifest
            .env
            .get(key)
            .ok_or_else(|| AppError::validation(format!("unknown env var {key}")))?;
        match spec {
            setu_manifest::EnvSpec::Value(_) => {
                secrets.insert(key.clone(), false);
            }
            setu_manifest::EnvSpec::Prompt(p) => {
                secrets.insert(key.clone(), p.secret);
            }
            setu_manifest::EnvSpec::Generate(_) | setu_manifest::EnvSpec::From(_) => {
                return Err(AppError::validation(format!(
                    "env var {key} is generated and cannot be set"
                )));
            }
        }
    }

    // Merge: unseal the stored copy for the agent payload, apply the
    // updates, seal the stored copy again. Iterates the union of stored
    // keys and request keys: a manifest-valid var missing from storage
    // (manifest gained it after provisioning) must be persisted, not
    // silently dropped.
    let stored = stored_env(&site);
    let mut merged: Vec<(String, String, bool, bool)> = Vec::new();
    for v in &stored {
        if let Some(new_value) = body.env.get(&v.name) {
            let secret = secrets.get(&v.name).copied().unwrap_or(v.secret);
            merged.push((v.name.clone(), new_value.clone(), secret, v.once));
        } else {
            let plain = open_stored(&state.sealer, &v.value)?;
            merged.push((v.name.clone(), plain, v.secret, v.once));
        }
    }
    for (key, new_value) in &body.env {
        if stored.iter().any(|v| &v.name == key) {
            continue;
        }
        // Validated against the manifest above, so the spec exists and is
        // Value/Prompt; `once` is false for a fresh Value/Prompt entry.
        let spec_once = manifest
            .env
            .get(key)
            .map(|s| match s {
                setu_manifest::EnvSpec::Prompt(p) => p.once,
                _ => false,
            })
            .unwrap_or(false);
        let secret = secrets.get(key).copied().unwrap_or(false);
        merged.push((key.clone(), new_value.clone(), secret, spec_once));
    }
    let sealed: serde_json::Value = serde_json::Value::Array(
        merged
            .iter()
            .map(|(name, value, secret, once)| {
                serde_json::json!({
                    "name": name,
                    "value": state.sealer.store(value, *secret),
                    "secret": secret,
                    "once": once,
                })
            })
            .collect(),
    );
    state.db.sites().set_env(&site.id, &sealed).await?;

    // Synchronous WriteEnv so a failure surfaces here (409/502), not as
    // a dead restart job; the restart job then picks the values up.
    let proto_env: Vec<setu_proto::EnvVar> = merged
        .iter()
        .map(|(name, value, secret, once)| setu_proto::EnvVar {
            name: name.clone(),
            value: value.clone(),
            secret: *secret,
            once: *once,
        })
        .collect();
    let spec = env_site_spec(&site, &manifest, &plan);
    if let Err(e) = state
        .agent
        .call(
            &site.server_id,
            setu_proto::Command::WriteEnv {
                site: spec,
                env: proto_env,
            },
            Duration::from_secs(120),
        )
        .await
    {
        return Err(AppError::from(e));
    }

    let job = state
        .db
        .jobs()
        .enqueue("restart_site", Some(&site.id), Some(&site.server_id), None)
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.env_update",
        Some(&site.id),
        audit_ip(None, Some(peer.0)).as_deref(),
    )
    .await;
    let site = state.db.sites().get(&id).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(SiteJobResponse {
            site: SiteOut::from_row(&site),
            job: JobOut::from_row(&job),
        }),
    ))
}

/// Minimal agent spec for the env `WriteEnv`: keyed off `site_id`, so
/// the agent finds the right env file. Fills limits/health/exec from
/// the plan and manifest where available, defaults otherwise.
fn env_site_spec(
    site: &setu_db::Site,
    manifest: &setu_manifest::Manifest,
    plan: &setu_db::Plan,
) -> setu_proto::SiteSpec {
    let site_dir = std::path::PathBuf::from(format!("/srv/setu/sites/{}", site.short_id));
    let data_dir = site_dir.join("data");
    let (exec, args) = match manifest.run.target(setu_manifest::Target::X86_64Linux) {
        Some(t) => (
            site_dir.join("bin").join("current").join(&t.exec),
            manifest.run.args.clone(),
        ),
        None => (site_dir.join("bin").join("current").join("app"), Vec::new()),
    };
    setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: site.short_id.clone(),
        user: format!("setu-site-{}", site.short_id),
        data_dir,
        run_dir: site_dir.join("run"),
        port: site.port as u16,
        exec,
        args,
        env_file: site_dir.join("env"),
        limits: setu_proto::Limits {
            memory_mb: u32::try_from(plan.memory_mb).unwrap_or(512),
            cpu_percent: u32::try_from(plan.cpu_percent).unwrap_or(100),
            disk_mb: u32::try_from(plan.disk_mb).unwrap_or(5120),
        },
        health_path: manifest.app.health.clone(),
        binary: None,
    }
}

/// Live logs as SSE (viewer and above): `FetchLogs` every 2 s, unseen
/// lines since the cursor render as `data: <line>` events. The stream
/// ends when the client disconnects (a failed send breaks the loop) or
/// when the agent call fails (an `error` event first).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}/logs",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, description = "SSE log stream (`data: <line>` events)"),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn logs(
    State(state): State<AppState>,
    session: Session,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    require_role(&session, &state.db, Role::Viewer).await?;
    let site = state.db.sites().get(&id).await?;
    let server_id = site.server_id.clone();
    let site_id = site.id.clone();

    let (mut tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    let agent = state.agent.clone();
    tokio::spawn(async move {
        let mut cursor = 0usize;
        loop {
            let call = agent
                .call(
                    &server_id,
                    setu_proto::Command::FetchLogs {
                        site_id: site_id.clone(),
                        lines: 200,
                    },
                    Duration::from_secs(30),
                )
                .await;
            match call {
                Ok(result) => {
                    if !result.ok {
                        let msg = result
                            .error
                            .unwrap_or_else(|| "fetch logs failed".to_string());
                        let _ = tx
                            .send(Ok(axum::body::Bytes::from(format!(
                                "event: error\ndata: {msg}\n\n"
                            ))))
                            .await;
                        break;
                    }
                    let lines: Vec<String> = result
                        .output
                        .get("lines")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|v| v.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    // The agent returns a sliding window (`lines` caps the
                    // tail); once the log outgrows it, `len()` stops
                    // advancing past `cursor`. A shrunk window means
                    // rotation/truncation: restart from 0 so the stream
                    // resends the current window once instead of going
                    // silent forever.
                    if lines.len() < cursor {
                        cursor = 0;
                    }
                    for line in lines.iter().skip(cursor) {
                        let chunk = format!("data: {line}\n\n");
                        if tx.send(Ok(axum::body::Bytes::from(chunk))).await.is_err() {
                            return;
                        }
                    }
                    cursor = lines.len().max(cursor);
                }
                Err(e) => {
                    let _ = tx
                        .send(Ok(axum::body::Bytes::from(format!(
                            "event: error\ndata: {e}\n\n"
                        ))))
                        .await;
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
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
