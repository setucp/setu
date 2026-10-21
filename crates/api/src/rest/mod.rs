//! `/api/v1` handlers: auth, operators, servers, apps, plans (spec §6).

pub mod agent;
pub mod apps;
pub mod auth;
pub mod backups;
pub mod docs;
pub mod jobs;
pub mod operators;
pub mod panel;
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
        .route("/panel/backup", get(panel::backup_status))
        .route("/operators/{id}/password", put(operators::set_password))
        .route("/operators/{id}", delete(operators::delete))
        .route("/setup", get(setup::status).post(setup::create_owner))
        .route("/setup/local-server", post(setup::local_server))
        .route("/servers", get(servers::list).post(servers::create))
        .route("/servers/{id}", get(servers::get).delete(servers::delete))
        .route("/servers/{id}/cordon", post(servers::cordon))
        .route("/servers/{id}/rotate-token", post(servers::rotate_token))
        .route("/apps", get(apps::list).post(apps::publish))
        .route("/apps/validate", post(apps::validate))
        .route("/apps/{id}/versions", get(apps::versions))
        .route(
            "/apps/{id}/versions/{version}/prompts",
            get(apps::version_prompts),
        )
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
        .route(
            "/sites/{id}/reset",
            get(backups::reset_get)
                .put(backups::reset_put)
                .delete(backups::reset_delete),
        )
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

/// Server-sent-event framing shared by the site logs and job log tails.
pub(crate) mod sse {
    use std::time::Duration;

    /// Longest life of one SSE response; the stream then ends with a
    /// `reconnect` event and the client opens a new one (a forgotten tab
    /// cannot hold an agent poll loop open forever).
    pub const MAX_LIFETIME: Duration = Duration::from_mins(30);

    /// An SSE comment: ignored by `EventSource`, but the write fails once
    /// the client is gone, which is how an idle stream notices.
    pub const KEEP_ALIVE: &str = ": keep-alive\n\n";

    /// Ends the stream at [`MAX_LIFETIME`].
    pub const RECONNECT: &str = "event: reconnect\ndata: stream lifetime reached\n\n";

    /// One `data:` event for `line`, with `\r` and `\n` escaped so a line
    /// can never end its event early or inject a field.
    pub fn data(line: &str) -> String {
        format!("data: {}\n\n", escape(line))
    }

    /// A named event with an escaped payload.
    pub fn event(name: &str, payload: &str) -> String {
        format!("event: {name}\ndata: {}\n\n", escape(payload))
    }

    /// The sending half every SSE task writes to.
    pub type Tx = futures::channel::mpsc::Sender<Result<axum::body::Bytes, std::io::Error>>;

    /// Queue `text` without waiting: `false` once the client is gone (a
    /// full buffer just skips it; the next poll tries again).
    pub fn offer(tx: &mut Tx, text: &'static str) -> bool {
        match tx.try_send(Ok(axum::body::Bytes::from_static(text.as_bytes()))) {
            Ok(()) => true,
            Err(e) => !e.is_disconnected(),
        }
    }

    /// Send `text`, waiting for room: `false` once the client is gone.
    pub async fn send(tx: &mut Tx, text: String) -> bool {
        use futures::SinkExt as _;
        tx.send(Ok(axum::body::Bytes::from(text))).await.is_ok()
    }

    /// Run `body` for at most [`MAX_LIFETIME`]; past it, end the stream
    /// with [`RECONNECT`]. Holds even when a send blocks on a stalled
    /// client.
    pub async fn with_lifetime<F>(tx: Tx, body: impl FnOnce(Tx) -> F)
    where
        F: std::future::Future<Output = ()>,
    {
        with_lifetime_of(MAX_LIFETIME, tx, body).await;
    }

    async fn with_lifetime_of<F>(limit: Duration, mut tx: Tx, body: impl FnOnce(Tx) -> F)
    where
        F: std::future::Future<Output = ()>,
    {
        if tokio::time::timeout(limit, body(tx.clone())).await.is_err() {
            let _ = offer(&mut tx, RECONNECT);
        }
    }

    fn escape(line: &str) -> String {
        line.replace('\r', "\\r").replace('\n', "\\n")
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn data_escapes_cr_and_lf() {
            assert_eq!(super::data("a\rb\nc"), "data: a\\rb\\nc\n\n");
            assert_eq!(
                super::event("error", "x\ny"),
                "event: error\ndata: x\\ny\n\n"
            );
        }

        #[tokio::test]
        async fn lifetime_ends_with_reconnect() {
            use futures::StreamExt as _;
            let (tx, mut rx) = futures::channel::mpsc::channel(4);
            super::with_lifetime_of(std::time::Duration::from_millis(50), tx, |_tx| {
                std::future::pending::<()>()
            })
            .await;
            let first = rx.next().await.expect("an event").expect("ok");
            assert_eq!(&first[..], super::RECONNECT.as_bytes());
        }
    }
}
