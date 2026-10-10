//! `GET/POST /api/v1/sites` and the per-site routes (spec §5/§6, plan
//! Task 9): restart/stop/start, update, retry, delete, env (masked),
//! logs (SSE). Every mutation writes `audit_log` and returns 202 with the
//! enqueued job; the background `Runner` (started by `setu serve` and by
//! `TestServer`) picks the job up.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
    Json,
};
use futures::channel::mpsc;
use serde::{Deserialize, Serialize};
use setu_common::{AppError, ErrorCode};
use utoipa::ToSchema;

use crate::auth::{audit, require_role, ClientIp, Role, Session};
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

#[derive(Deserialize, ToSchema)]
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

// Holds prompt answers: `Debug` prints their names only.
impl std::fmt::Debug for CreateSiteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CreateSiteRequest")
            .field("name", &self.name)
            .field("app_id", &self.app_id)
            .field("version", &self.version)
            .field("domain", &self.domain)
            .field("prompts", &self.prompts.keys().collect::<Vec<_>>())
            .finish()
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreateSiteResponse {
    pub site: SiteOut,
    pub job: JobOut,
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateSiteRequest {
    pub version: String,
    /// Answers for `prompt` rules the target version adds (sealed into
    /// the job; existing answers are kept by name).
    #[serde(default)]
    pub prompts: BTreeMap<String, String>,
}

// Holds prompt answers: `Debug` prints their names only.
impl std::fmt::Debug for UpdateSiteRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UpdateSiteRequest")
            .field("version", &self.version)
            .field("prompts", &self.prompts.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// The newest 1-minute sample of a site (spec §3 `site_metrics`).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SiteMetricOut {
    pub at: chrono::DateTime<chrono::Utc>,
    pub cpu_seconds: f64,
    pub memory_bytes: i64,
    pub disk_bytes: i64,
}

/// `GET /sites/{id}`: the site plus its latest metric sample.
#[derive(Debug, Serialize, ToSchema)]
pub struct SiteDetailOut {
    #[serde(flatten)]
    pub site: SiteOut,
    pub metrics: Option<SiteMetricOut>,
}

#[derive(Deserialize, ToSchema)]
pub struct RevealRequest {
    /// The caller's own password (re-authentication).
    pub password: String,
}

// Holds a credential: `Debug` redacts it.
impl std::fmt::Debug for RevealRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RevealRequest([redacted])")
    }
}

/// The plaintext of one env value. Holds a secret: `Debug` redacts it.
#[derive(Serialize, ToSchema)]
pub struct RevealOut {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for RevealOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RevealOut")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .finish()
    }
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

#[derive(Deserialize, ToSchema)]
pub struct EnvPutRequest {
    pub env: BTreeMap<String, String>,
}

// Holds env values: `Debug` prints their names only.
impl std::fmt::Debug for EnvPutRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvPutRequest")
            .field("env", &self.env.keys().collect::<Vec<_>>())
            .finish()
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
    require_role(&session, Role::Viewer).await?;
    let sites = state.db.sites().list().await?;
    Ok(Json(SiteListOut {
        sites: sites.iter().map(SiteOut::from_row).collect(),
    }))
}

/// Lower-case a host name and drop a trailing dot (for comparisons).
fn host_key(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

/// Refuse a site domain that would shadow the panel (R130): the
/// `public_url` host, `localhost` and anything under `.localhost` or
/// `localhost.localdomain`, and the hostname of any joined server.
/// Caddy routes are first-match, so a site claiming one of these names
/// could take over the panel's route (or the server's own name).
async fn refuse_reserved_domain(state: &AppState, domain: &str) -> Result<(), AppError> {
    let domain = host_key(domain);
    let reserved = |what: &str| {
        Err(AppError::validation(format!(
            "domain {domain} is reserved ({what})"
        )))
    };
    if let Some(panel) = state.config.public_url.host_str() {
        if host_key(panel) == domain {
            return reserved("the panel's host");
        }
    }
    if domain == "localhost" || domain.ends_with(".localhost") || domain == "localhost.localdomain"
    {
        return reserved("localhost");
    }
    for server in state.db.servers().list().await? {
        if server
            .hostname
            .as_deref()
            .is_some_and(|h| host_key(h) == domain)
        {
            return reserved("a server's hostname");
        }
    }
    Ok(())
}

/// Create a site and enqueue its provision job (operator and above).
///
/// Validates the domain (unique → 409), the app version (must exist and
/// parse; `image` manifests are rejected with `validation` — phase 2),
/// the plan (must allow the app) and the server (explicit, or the
/// online server with the most free memory). Generates the env once,
/// from the allocated port and short id and the operator-supplied
/// `prompts`, seals it into the `provisioning` row in the same
/// transaction, then enqueues `provision_site` (which reads the stored
/// env and never regenerates it).
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
    ip: ClientIp,
    ApiJson(body): ApiJson<CreateSiteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    tracing::debug!("site create {body:?}");

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
    refuse_reserved_domain(&state, &domain).await?;
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
    // Server pick, capacity (85 %), port and short id in one
    // transaction (spec §3); `db_name`/`db_role` are `site_<short>`. The
    // env is generated exactly once, here, from that allocation (prompt
    // answers included) and stored sealed; provision only reads it, so
    // the answers never sit in a job payload (security finding 4).
    let site = state
        .db
        .sites()
        .create_with(
            &setu_db::NewSite {
                name: name.clone(),
                app_id: body.app_id.clone(),
                app_version: body.version.clone(),
                server_id: body.server_id.clone(),
                plan_id: plan.id.clone(),
                domain: domain.clone(),
                owner_email: body.owner_email.clone(),
                env_sealed: serde_json::json!([]),
                plan_memory_mb: i64::from(plan.memory_mb),
            },
            |alloc| {
                setu_jobs::resolve_site_env(
                    &manifest,
                    alloc,
                    &domain,
                    &body.owner_email,
                    &body.prompts,
                    &state.sealer,
                )
            },
        )
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
            None,
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.create",
        Some(&site.id),
        ip.as_deref(),
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

/// One site with its latest metric sample (viewer and above).
#[utoipa::path(
    get,
    path = "/api/v1/sites/{id}",
    params(("id" = String, Path, description = "Site id")),
    responses(
        (status = 200, body = SiteDetailOut),
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
    let metrics = state
        .db
        .metrics()
        .latest_for_site(&site.id)
        .await?
        .map(|m| SiteMetricOut {
            at: m.at,
            cpu_seconds: m.cpu_seconds,
            memory_bytes: m.memory_bytes,
            disk_bytes: m.disk_bytes,
        });
    Ok(Json(SiteDetailOut {
        site: SiteOut::from_row(&site),
        metrics,
    }))
}

/// Enqueue a lifecycle job (`restart_site` | `stop_site` | `start_site`;
/// operator and above). Guards: no job already live for the site, and
/// the status transition must make sense.
async fn enqueue_lifecycle(
    state: &AppState,
    caller_id: &str,
    ip: &ClientIp,
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
        ip.as_deref(),
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let (site, job) =
        enqueue_lifecycle(&state, &caller.id, &ip, &id, "restart_site", "site.restart").await?;
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let (site, job) =
        enqueue_lifecycle(&state, &caller.id, &ip, &id, "stop_site", "site.stop").await?;
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let (site, job) =
        enqueue_lifecycle(&state, &caller.id, &ip, &id, "start_site", "site.start").await?;
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
    ip: ClientIp,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<UpdateSiteRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
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
    // Answers for the target's `prompt` rules ride SEALED on the job (every
    // value `v1:`, security finding 4); the `env` step opens them.
    let mut prompts = serde_json::Map::new();
    for (name, value) in &body.prompts {
        if !matches!(
            manifest.env.get(name),
            Some(setu_manifest::EnvSpec::Prompt(_))
        ) {
            return Err(AppError::validation(format!(
                "{name} is not a prompt of version {}",
                body.version
            )));
        }
        if !setu_common::validate::no_control(value) {
            return Err(AppError::validation(format!(
                "prompt {name} must not contain control characters"
            )));
        }
        prompts.insert(name.clone(), serde_json::json!(state.sealer.seal(value)));
    }
    let mut payload = serde_json::json!({ "to_version": body.version });
    if !prompts.is_empty() {
        payload["prompts"] = serde_json::Value::Object(prompts);
    }
    let job = state
        .db
        .jobs()
        .enqueue(
            "update_site",
            Some(&site.id),
            Some(&site.server_id),
            Some(&payload),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.update",
        Some(&site.id),
        ip.as_deref(),
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

/// Retry a `failed` site: a fresh `provision_site` job from step 1 with
/// the same allocations and stored env (operator+).
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    require_no_running_job(&state.db, &id).await?;
    if site.status != "failed" {
        return Err(AppError::conflict(format!(
            "only failed sites can be retried (status: {})",
            site.status
        )));
    }
    // The env (prompt answers included) was stored at creation: the
    // retry provisions from step 1 with the same allocations.
    let job = state
        .db
        .jobs()
        .enqueue(
            "provision_site",
            Some(&site.id),
            Some(&site.server_id),
            None,
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.retry",
        Some(&site.id),
        ip.as_deref(),
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
    ip: ClientIp,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
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
        ip.as_deref(),
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
    require_role(&session, Role::Viewer).await?;
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
/// are derived and rejected with `validation`). Enqueues an
/// `env_update` job carrying the changes sealed; the job stores them,
/// rewrites the env file (never with `once` values) and restarts a
/// running site.
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
    ip: ClientIp,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<EnvPutRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    tracing::debug!("site env update {body:?}");
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

    // Validate keys against the manifest: `value`/`prompt` only (the
    // `env_update` job re-checks), values without control characters.
    for (key, value) in &body.env {
        match manifest.env.get(key) {
            Some(setu_manifest::EnvSpec::Value(_) | setu_manifest::EnvSpec::Prompt(_)) => {}
            Some(setu_manifest::EnvSpec::Generate(_) | setu_manifest::EnvSpec::From(_)) => {
                return Err(AppError::validation(format!(
                    "env var {key} is generated and cannot be set"
                )));
            }
            None => return Err(AppError::validation(format!("unknown env var {key}"))),
        }
        if !setu_common::validate::no_control(value) {
            return Err(AppError::validation(format!(
                "env var {key} must not contain control characters"
            )));
        }
    }

    // The changes ride SEALED on the job (every value `v1:`, secret or
    // not: plaintext never sits in `jobs.payload`, security finding 4);
    // the `env_update` job merges, stores, writes the file and restarts.
    let changes: serde_json::Map<String, serde_json::Value> = body
        .env
        .iter()
        .map(|(k, v)| (k.clone(), serde_json::json!(state.sealer.seal(v))))
        .collect();
    let job = state
        .db
        .jobs()
        .enqueue(
            "env_update",
            Some(&site.id),
            Some(&site.server_id),
            Some(&serde_json::json!({ "changes": changes })),
        )
        .await?;
    audit(
        &state.db,
        Some(&caller.id),
        "site.env_update",
        Some(&site.id),
        ip.as_deref(),
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

/// Reveal one env value (operator and above) after re-checking the
/// caller's password (argon2). Every call is audited: `env.reveal` when
/// the value is returned, `env.reveal_denied` on a wrong password, with
/// target `<site id>:<NAME>`. Wrong passwords count toward the login
/// limiter of the client IP.
#[utoipa::path(
    post,
    path = "/api/v1/sites/{id}/env/{name}/reveal",
    params(
        ("id" = String, Path, description = "Site id"),
        ("name" = String, Path, description = "Env var name"),
    ),
    request_body = RevealRequest,
    responses(
        (status = 200, body = RevealOut),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
        (status = 429, body = super::ErrorBody),
    )
)]
pub async fn env_reveal(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    Path((id, name)): Path<(String, String)>,
    ApiJson(body): ApiJson<RevealRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Operator).await?;
    let site = state.db.sites().get(&id).await?;
    let var = stored_env(&site)
        .into_iter()
        .find(|v| v.name == name)
        .ok_or_else(|| AppError::not_found(format!("env var {name} not found")))?;
    let target = format!("{}:{}", site.id, var.name);
    let key = ip.limiter_key();
    // Counted before the verify, under the limiter's lock (I4).
    state.begin_login_attempt(key).await?;
    if crate::auth::verify_password_blocking(&caller.password_hash, &body.password)
        .await
        .is_err()
    {
        audit(
            &state.db,
            Some(&caller.id),
            "env.reveal_denied",
            Some(&target),
            ip.as_deref(),
        )
        .await;
        return Err(AppError::forbidden("password is incorrect"));
    }
    state.forgive_login_attempt(key).await;
    let value = open_stored(&state.sealer, &var.value)?;
    audit(
        &state.db,
        Some(&caller.id),
        "env.reveal",
        Some(&target),
        ip.as_deref(),
    )
    .await;
    Ok(Json(RevealOut {
        name: var.name,
        value,
    }))
}

/// Live logs as SSE (viewer and above): `FetchLogs` every 2 s with the
/// journal cursor of the previous page, so each line renders once as a
/// `data: <line>` event (CR/LF inside a line escaped). Every poll also
/// sends a `: keep-alive` comment, so a gone client is noticed (the send
/// fails, or the channel reports closed) and the agent is no longer
/// called. The stream ends on disconnect, after an `error` event with a
/// fixed message when the agent call fails (details are logged), or
/// with a `reconnect` event after 30 minutes.
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
    use super::sse;
    require_role(&session, Role::Viewer).await?;
    let site = state.db.sites().get(&id).await?;
    let server_id = site.server_id.clone();
    let short = site.short_id.clone();

    let (tx, rx) = mpsc::channel::<Result<axum::body::Bytes, std::io::Error>>(32);
    let agent = state.agent.clone();
    tokio::spawn(sse::with_lifetime(tx, move |mut tx| async move {
        let mut cursor: Option<String> = None;
        loop {
            if tx.is_closed() {
                return;
            }
            let call = agent
                .call(
                    &server_id,
                    Some(&short),
                    setu_proto::Command::FetchLogs {
                        site: short.clone(),
                        cursor: cursor.clone(),
                        lines: 200,
                    },
                )
                .await;
            match call {
                Ok(result) if result.ok => {
                    let page: setu_proto::LogsOutput =
                        serde_json::from_value(result.output).unwrap_or_default();
                    for line in &page.lines {
                        if !sse::send(&mut tx, sse::data(line)).await {
                            return;
                        }
                    }
                    if page.cursor.is_some() {
                        cursor = page.cursor;
                    }
                }
                Ok(result) => {
                    tracing::warn!(site = %short, error = ?result.error, "logs: fetch_logs failed");
                    sse::send(&mut tx, sse::event("error", "fetching logs failed")).await;
                    return;
                }
                Err(e) => {
                    tracing::warn!(site = %short, error = %e, "logs: agent call failed");
                    sse::send(&mut tx, sse::event("error", "the agent did not answer")).await;
                    return;
                }
            }
            if !sse::offer(&mut tx, sse::KEEP_ALIVE) {
                return;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }));

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
