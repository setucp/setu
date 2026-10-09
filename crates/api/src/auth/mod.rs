//! Operator auth (spec §7): argon2id passwords, sessions in the
//! `sessions` table ([`session`]), the client IP behind Caddy or a
//! trusted proxy ([`client_ip`]), role checks, the `Origin` /
//! `Sec-Fetch-Site` guard on mutations and the audit trail.

pub mod client_ip;
pub mod session;

use axum::{
    extract::{Request, State},
    http::{HeaderMap, Method},
    middleware::Next,
    response::Response,
};
use setu_common::AppError;
use setu_db::Db;

pub use client_ip::ClientIp;
pub use session::{current_operator, require_role, Session};

use crate::state::AppState;

/// Role ordering: `owner` > `operator` > `viewer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Viewer = 1,
    Operator = 2,
    Owner = 3,
}

impl Role {
    pub fn parse(s: &str) -> Result<Self, AppError> {
        match s {
            "owner" => Ok(Self::Owner),
            "operator" => Ok(Self::Operator),
            "viewer" => Ok(Self::Viewer),
            _ => Err(AppError::validation(format!("unknown role {s}"))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Operator => "operator",
            Self::Viewer => "viewer",
        }
    }
}

/// Hash a password with argon2id default params. The plaintext is never
/// logged; errors map to `internal` without detail.
pub fn hash_password(password: &str) -> Result<String, AppError> {
    use argon2::{
        password_hash::{rand_core::OsRng, SaltString},
        Argon2, PasswordHasher,
    };
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| {
            tracing::error!("password hashing failed: {e}");
            AppError::internal()
        })
}

/// Verify a password against a stored PHC hash. Unknown hash format is an
/// `internal` (logged); a mismatch is `unauthorized` — both without detail.
pub fn verify_password(hash: &str, password: &str) -> Result<(), AppError> {
    use argon2::{PasswordHash, PasswordVerifier};
    let parsed = PasswordHash::new(hash).map_err(|e| {
        tracing::error!("stored password hash is corrupt: {e}");
        AppError::internal()
    })?;
    argon2::Argon2::default()
        .verify_password(password.as_bytes(), &parsed)
        .map_err(|_| AppError::unauthorized("invalid email or password"))
}

/// CSRF guard (spec §7): every POST/PUT/DELETE/PATCH under `/api/v1`
/// must carry an `Origin` whose scheme, host and port equal the
/// configured `public_url`, or `Sec-Fetch-Site: same-origin` (set by the
/// browser, never by page script). Anything else is 403 — including a
/// request with neither header, so a non-browser client names the panel
/// in `Origin`.
pub async fn csrf_middleware(
    State(state): State<AppState>,
    req: Request,
    next: Next,
) -> Result<Response, AppError> {
    let is_mutating = matches!(
        req.method(),
        &Method::POST | &Method::PUT | &Method::DELETE | &Method::PATCH
    );
    if is_mutating && !same_origin(req.headers(), &state.config.public_url) {
        return Err(AppError::forbidden(
            "cross-origin request refused: Origin must match the panel URL",
        ));
    }
    Ok(next.run(req).await)
}

/// `Origin` equals `public_url`'s origin, or the browser says the
/// request is same-origin.
pub(crate) fn same_origin(headers: &HeaderMap, public_url: &url::Url) -> bool {
    let fetch_site_same = headers
        .get("sec-fetch-site")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("same-origin"));
    if fetch_site_same {
        return true;
    }
    headers
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .and_then(|o| url::Url::parse(o.trim()).ok())
        .is_some_and(|origin| origin_matches(&origin, public_url))
}

fn origin_matches(origin: &url::Url, public_url: &url::Url) -> bool {
    origin.scheme().eq_ignore_ascii_case(public_url.scheme())
        && match (origin.host_str(), public_url.host_str()) {
            (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
            _ => false,
        }
        && origin.port_or_known_default() == public_url.port_or_known_default()
}

/// Record one audit row; failures are logged, never fatal to the request.
/// `target_kind` is the action's prefix (`site.create` → `site`).
pub async fn audit(
    db: &Db,
    operator_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    ip: Option<&str>,
) {
    let target_kind = target.and(action.split('.').next());
    if let Err(e) = db
        .audit()
        .record(operator_id, action, target_kind, target, ip)
        .await
    {
        tracing::error!(error = %e, "audit record failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, v.parse().expect("header value"));
        }
        h
    }

    fn url(s: &str) -> url::Url {
        s.parse().expect("url parses")
    }

    #[test]
    fn origin_must_equal_public_url_origin() {
        let public = url("https://panel.example.com/");
        assert!(same_origin(
            &headers(&[("origin", "https://panel.example.com")]),
            &public
        ));
        assert!(same_origin(
            &headers(&[("origin", "https://PANEL.example.com:443")]),
            &public
        ));
        for bad in [
            "https://evil.example",
            "http://panel.example.com",
            "https://panel.example.com:8443",
            "null",
        ] {
            assert!(!same_origin(&headers(&[("origin", bad)]), &public), "{bad}");
        }
        assert!(!same_origin(&HeaderMap::new(), &public), "no headers");
    }

    #[test]
    fn sec_fetch_site_same_origin_passes_and_cross_site_does_not() {
        let public = url("http://127.0.0.1:7000");
        assert!(same_origin(
            &headers(&[("sec-fetch-site", "same-origin")]),
            &public
        ));
        assert!(!same_origin(
            &headers(&[("sec-fetch-site", "cross-site")]),
            &public
        ));
        assert!(!same_origin(
            &headers(&[("sec-fetch-site", "same-site")]),
            &public
        ));
        let ipv6 = url("http://[::1]:7000");
        assert!(same_origin(
            &headers(&[("origin", "http://[::1]:7000")]),
            &ipv6
        ));
    }
}
