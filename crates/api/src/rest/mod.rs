//! `/api/v1` handlers: auth, operators, servers, apps, plans (spec §6).

pub mod apps;
pub mod auth;
pub mod docs;
pub mod operators;
pub mod plans;
pub mod servers;

use axum::{
    extract::{FromRequest, Request, State},
    middleware,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::Serialize;
use setu_common::AppError;
use tower_sessions::{cookie::SameSite, Expiry, SessionManagerLayer};
use utoipa::{OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

use super::auth as authn;
use super::session_store::PgSessionStore;
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

/// `Secure` cookies only when the control plane is served over https, i.e.
/// when `public_url` has the `https` scheme. Plain-http deployments (local
/// dev, http test servers) must not set it or browsers/proxies would drop
/// the session cookie.
pub(crate) fn use_secure_cookies(public_url: &url::Url) -> bool {
    public_url.scheme() == "https"
}

/// Unknown routes render the `not_found` envelope instead of axum's
/// default plain-text 404.
async fn fallback() -> AppError {
    AppError::not_found("route not found")
}

/// Assemble the full control-plane router: health probes, `/api/v1`, the
/// OpenAPI document and Swagger UI. The session layer wraps everything so
/// the `Session` extractor works in every `/api/v1` handler.
pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/me", get(auth::me))
        .route("/operators", get(operators::list).post(operators::create))
        .route("/servers", get(servers::list).post(servers::create))
        .route("/servers/{id}", get(servers::get).delete(servers::delete))
        .route("/apps", get(apps::list).post(apps::publish))
        .route("/apps/validate", post(apps::validate))
        .route("/apps/{id}/versions", get(apps::versions))
        .route("/plans", get(plans::list).post(plans::create))
        .route("/plans/{id}", get(plans::get).put(plans::update))
        .layer(middleware::from_fn(authn::csrf_middleware))
        .with_state(state.clone());

    let store = PgSessionStore::new(state.db.pool().clone());
    let secure = use_secure_cookies(&state.config.public_url);
    let session_layer = SessionManagerLayer::new(store)
        .with_name("setu_session")
        .with_secure(secure)
        .with_same_site(SameSite::Lax)
        .with_expiry(Expiry::OnInactivity(time::Duration::hours(24)));

    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1", api)
        .merge(SwaggerUi::new("/api/docs").url("/api/openapi.json", docs::ApiDoc::openapi()))
        .fallback(fallback)
        .layer(session_layer)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secure_cookie_flag_follows_public_url_scheme() {
        let http: url::Url = "http://127.0.0.1:7000".parse().expect("http url parses");
        assert!(
            !use_secure_cookies(&http),
            "http public_url must not set Secure"
        );
        let https: url::Url = "https://control.example".parse().expect("https url parses");
        assert!(
            use_secure_cookies(&https),
            "https public_url must set Secure"
        );
    }
}
