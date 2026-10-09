//! `/api/v1` handlers: auth, operators, servers, apps, plans (spec §6).

pub mod agent;
pub mod apps;
pub mod auth;
pub mod backups;
pub mod docs;
pub mod jobs;
pub mod operators;
pub mod plans;
pub mod servers;
pub mod setup;
pub mod sites;

use axum::{
    extract::{FromRequest, Request, State},
    middleware,
    response::IntoResponse,
    routing::{delete, get, post, put},
    Json, Router,
};
use serde::Serialize;
use setu_common::AppError;
use utoipa::ToSchema;

use super::auth as authn;
use super::embed;
use super::state::AppState;

/// `{ "error": { "code": "...", "message": "..." } }` (spec §6).
#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ErrorDetail {
    pub code: String,
    pub message: String,
}

/// JSON body extractor that maps every rejection (malformed JSON, missing
/// fields, wrong content type) to the `validation` error envelope. Axum's
/// default `Json` renders those as plain-text 400/415/422 bodies, which
/// would break the spec §6 envelope contract.
pub struct ApiJson<T>(pub T);

impl<S, T> FromRequest<S> for ApiJson<T>
where
    T: serde::de::DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        axum::Json::<T>::from_request(req, state)
            .await
            .map(|axum::Json(value)| Self(value))
            .map_err(|e| AppError::validation(format!("invalid request body: {e}")))
    }
}

/// Assemble the full control-plane router: health probes, `/api/v1`
/// (sessions, the `Origin` guard, the OpenAPI document behind a session;
/// no Swagger UI, spec §7), `/agent/v1` (bearer tokens) and the embedded
/// UI. Serve it with `connect_info::<listen::Peer>` (see
/// [`crate::listen::serve`]).
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/operators", get(operators::list).post(operators::create))
        .route("/operators/{id}/password", put(operators::set_password))
        .route("/setup", get(setup::status).post(setup::create_owner))
        .route("/setup/local-server", post(setup::local_server))
        .route("/servers", get(servers::list).post(servers::create))
        .route("/servers/{id}", get(servers::get).delete(servers::delete))
        .route("/servers/{id}/cordon", post(servers::cordon))
        .route("/apps", get(apps::list).post(apps::publish))
        .route("/apps/validate", post(apps::validate))
        .route("/apps/{id}/versions", get(apps::versions))
        .route("/plans", get(plans::list).post(plans::create))
        .route("/plans/{id}", get(plans::get).put(plans::update))
        .route("/sites", get(sites::list).post(sites::create))
        .route("/sites/{id}", get(sites::get).delete(sites::delete))
        .route("/sites/{id}/restart", post(sites::restart))
        .route("/sites/{id}/stop", post(sites::stop))
        .route("/sites/{id}/start", post(sites::start))
        .route("/sites/{id}/update", post(sites::update))
        .route("/sites/{id}/retry", post(sites::retry))
        .route("/sites/{id}/env", get(sites::env_get).put(sites::env_put))
        .route("/sites/{id}/env/{name}/reveal", post(sites::env_reveal))
        .route("/sites/{id}/logs", get(sites::logs))
        .route(
            "/sites/{id}/backups",
            get(backups::list).post(backups::create),
        )
        .route("/sites/{id}/backups/{bid}", delete(backups::delete))
        .route("/sites/{id}/backups/{bid}/restore", post(backups::restore))
        .route("/backups", get(backups::list_all))
        .route("/backups/{bid}", delete(backups::delete_any))
        .route("/jobs", get(jobs::list))
        .route("/jobs/{id}", get(jobs::get))
        .route("/jobs/{id}/log", get(jobs::log))
        .route("/audit", get(jobs::audit_list))
        .route("/openapi.json", get(docs::openapi_json))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authn::csrf_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            state.clone(),
            authn::session::session_middleware,
        ))
        .with_state(state.clone());

    // Agent protocol (spec §4): bearer tokens, never sessions, so these
    // routes sit outside the CSRF layer above.
    let agent_api = Router::new()
        .route("/join", post(agent::join))
        .route("/commands", get(agent::commands))
        .route("/commands/{id}/result", post(agent::result))
        .route("/heartbeat", post(agent::heartbeat))
        .with_state(state.clone());

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1", api)
        .nest("/agent/v1", agent_api)
        // Embedded UI (plan Task 12): `/` is the SPA shell, `/assets/*`
        // the fingerprinted bundle, everything else client-side routing.
        // The API nests above win over these, and unknown `/api/*` or
        // `/agent/*` paths keep the JSON `not_found` envelope via the
        // fallback in `embed`.
        .route("/", get(embed::index))
        .route("/assets/{*path}", get(embed::asset))
        .fallback(embed::fallback)
        .with_state(state)
}

#[derive(Serialize, ToSchema)]
pub struct HealthOut {
    pub status: String,
}

/// Liveness: always 200, no auth, no database.
#[utoipa::path(get, path = "/healthz", responses((status = 200, body = HealthOut)))]
pub async fn healthz() -> impl IntoResponse {
    Json(HealthOut {
        status: "ok".to_string(),
    })
}

#[derive(Serialize, ToSchema)]
pub struct ReadyOut {
    pub status: String,
    pub database: String,
}

/// Readiness: 200 when `SELECT 1` succeeds, 503 otherwise.
#[utoipa::path(get, path = "/readyz", responses((status = 200, body = ReadyOut)))]
pub async fn readyz(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    sqlx::query("SELECT 1::int")
        .fetch_one(state.db.pool())
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "readyz database check failed");
            AppError::internal()
        })?;
    Ok(Json(ReadyOut {
        status: "ok".to_string(),
        database: "up".to_string(),
    }))
}
