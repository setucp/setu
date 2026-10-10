//! Embedded control-plane UI (plan Task 12).
//!
//! `scripts/build-ui.sh` produces `ui/dist`; `build.rs` sets `cfg(setu_ui)`
//! when it exists and this module serves it via rust-embed with an SPA
//! fallback. API routes are nested *before* these handlers in
//! [`super::rest::router`], so `/api/*` and `/agent/*` never reach the
//! fallback. Without a prebuilt UI every path keeps the old JSON-`404`
//! behaviour, which is also what `cargo test` sees.

use axum::{
    extract::Path,
    http::{header, Method, Uri},
    response::IntoResponse,
};
use rust_embed::RustEmbed;

use setu_common::AppError;

#[cfg(setu_ui)]
#[derive(RustEmbed)]
#[folder = "../../ui/dist/"]
struct UiDist;

#[cfg(not(setu_ui))]
#[cfg_attr(not(setu_ui), allow(dead_code))]
#[derive(RustEmbed)]
#[folder = "ui-placeholder/"]
struct UiDist;

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".json") {
        "application/json"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else {
        "application/octet-stream"
    }
}

/// The bundled `index.html`, or `None` when the UI was not built.
#[cfg(setu_ui)]
fn index_html() -> Option<Vec<u8>> {
    UiDist::get("index.html").map(|f| f.data.into_owned())
}

#[cfg(not(setu_ui))]
fn index_html() -> Option<Vec<u8>> {
    None
}

fn html_response(body: Vec<u8>) -> axum::response::Response {
    ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response()
}

/// `GET /`: the SPA shell (or the JSON 404 when built API-only).
pub async fn index() -> impl IntoResponse {
    match index_html() {
        Some(body) => html_response(body),
        None => AppError::not_found("route not found").into_response(),
    }
}

/// `GET /assets/*path`: fingerprinted bundle files, immutable in practice.
pub async fn asset(Path(path): Path<String>) -> impl IntoResponse {
    let key = format!("assets/{path}");
    match UiDist::get(&key) {
        Some(file) => (
            [(header::CONTENT_TYPE, content_type(&key))],
            file.data.into_owned(),
        )
            .into_response(),
        None => AppError::not_found("route not found").into_response(),
    }
}

/// Catch-all: API/agent paths keep the `not_found` envelope; every other
/// GET serves the SPA shell so client-side routes resolve; non-GET
/// non-API paths are 404s.
pub async fn fallback(method: Method, uri: Uri) -> impl IntoResponse {
    let path = uri.path();
    if path == "/api"
        || path == "/agent"
        || path.starts_with("/api/")
        || path.starts_with("/agent/")
    {
        return AppError::not_found("route not found").into_response();
    }
    if method == Method::GET {
        if let Some(body) = index_html() {
            return html_response(body);
        }
    }
    AppError::not_found("route not found").into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;

    async fn status_of(method: Method, path: &str) -> (StatusCode, String) {
        let uri: Uri = path.parse().expect("test uri parses");
        let resp = fallback(method, uri).await.into_response();
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        (status, content_type)
    }

    #[tokio::test]
    async fn api_and_agent_paths_keep_json_404_in_both_build_modes() {
        for path in ["/api/v1/nope", "/agent/v1/nope", "/api", "/agent"] {
            let (status, content_type) = status_of(Method::GET, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "GET {path}");
            assert!(
                content_type.contains("application/json"),
                "GET {path} must be JSON, got: {content_type}"
            );
        }
    }

    #[tokio::test]
    async fn non_get_never_serves_the_spa() {
        let (status, _) = status_of(Method::POST, "/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    /// Root-path behaviour depends on the build: with the UI embedded
    /// (`scripts/build-ui.sh` ran, as in CI) plain GETs serve the SPA
    /// shell; API-only builds keep the JSON 404. `cargo test` must pass
    /// in both modes, so each side is asserted under its own `cfg`.
    #[tokio::test]
    #[cfg(setu_ui)]
    async fn root_serves_the_spa_when_embedded() {
        let (status, content_type) = status_of(Method::GET, "/nope").await;
        assert_eq!(status, StatusCode::OK);
        assert!(content_type.contains("text/html"), "got: {content_type}");
    }

    #[tokio::test]
    #[cfg(not(setu_ui))]
    async fn root_is_json_404_without_the_ui() {
        let (status, content_type) = status_of(Method::GET, "/nope").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(
            content_type.contains("application/json"),
            "got: {content_type}"
        );
    }
}
