//! Web sessions over `db.sessions()` (spec §3, §7).
//!
//! The cookie `setu_session` carries the 32 random bytes (base64url) the
//! repo returns from `create`; only their SHA-256 is stored. A request
//! middleware loads the session once (the repo slides the 7-day expiry
//! when the last refresh is over an hour old) and puts it in the request
//! extensions, where the [`Session`] extractor reads it. Responses to an
//! authenticated request re-send the cookie with a fresh `Max-Age`, so
//! the browser's copy slides with the row. Attributes: `HttpOnly`,
//! `SameSite=Lax`, `Path=/`, and `Secure` exactly when `public_url` is
//! https.

use axum::{
    extract::{FromRequestParts, Request, State},
    http::{header, request::Parts, HeaderMap, HeaderValue},
    middleware::Next,
    response::{IntoResponse, Response},
};
use setu_common::AppError;
use setu_db::Operator;

use super::Role;
use crate::state::AppState;

/// Cookie name (spec §7).
pub const COOKIE_NAME: &str = "setu_session";
/// Browser-side lifetime, equal to the row's sliding 7 days.
const COOKIE_MAX_AGE_SECS: u64 = 7 * 24 * 3600;

/// The request's session as loaded by [`session_middleware`]: the raw
/// cookie value and its operator, both `None` when anonymous.
#[derive(Clone, Default)]
pub struct Session {
    cookie: Option<String>,
    operator: Option<Operator>,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("cookie", &self.cookie.as_ref().map(|_| "[redacted]"))
            .field("operator", &self.operator.as_ref().map(|o| &o.id))
            .finish()
    }
}

impl Session {
    /// The logged-in operator, if any.
    pub fn operator(&self) -> Option<&Operator> {
        self.operator.as_ref()
    }

    /// The live cookie value (for logout).
    pub fn cookie(&self) -> Option<&str> {
        self.cookie.as_deref()
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Session {
    type Rejection = AppError;

    #[allow(clippy::unused_async_trait_impl)] // reviewed: kept as-is (R150)
    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<Session>()
            .cloned()
            .unwrap_or_default())
    }
}

/// The logged-in operator, or 401 when anonymous.
pub async fn current_operator(session: &Session) -> Result<Operator, AppError> {
    session
        .operator
        .clone()
        .ok_or_else(|| AppError::unauthorized("not authenticated"))
}

/// [`current_operator`] plus a minimum-role check; below the minimum is 403.
pub async fn require_role(session: &Session, min: Role) -> Result<Operator, AppError> {
    let op = current_operator(session).await?;
    if Role::parse(&op.role)? < min {
        return Err(AppError::forbidden("insufficient role"));
    }
    Ok(op)
}

/// The `setu_session` value from the request's `Cookie` headers.
pub fn cookie_value(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE_NAME)
        .map(|(_, value)| value.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `Set-Cookie` for a live session.
pub fn set_cookie(value: &str, secure: bool) -> HeaderValue {
    cookie_header(value, COOKIE_MAX_AGE_SECS, secure)
}

/// `Set-Cookie` that removes the session cookie.
pub fn clear_cookie(secure: bool) -> HeaderValue {
    cookie_header("", 0, secure)
}

#[allow(clippy::expect_used)] // reviewed: kept as-is (R150)
fn cookie_header(value: &str, max_age: u64, secure: bool) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{COOKIE_NAME}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age}{secure}"
    ))
    .expect("cookie value is base64url")
}

/// `Secure` cookies only when the panel is served over https, i.e. when
/// `public_url` has the `https` scheme; a plain-http panel (loopback dev)
/// would otherwise lose its cookie.
pub fn secure_cookies(public_url: &url::Url) -> bool {
    public_url.scheme() == "https"
}

/// Load the session named by the cookie (sliding its expiry) into the
/// request, then refresh the browser's cookie on the way out unless the
/// handler set or cleared it itself (login, logout, password change).
pub async fn session_middleware(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let mut session = Session::default();
    if let Some(cookie) = cookie_value(req.headers()) {
        match state.db.sessions().load(&cookie).await {
            Ok(Some((_row, operator))) => {
                session.cookie = Some(cookie);
                session.operator = Some(operator);
            }
            Ok(None) => {}
            Err(e) => return e.into_response(),
        }
    }
    let refresh = session.cookie.clone();
    req.extensions_mut().insert(session);
    let mut resp = next.run(req).await;
    if let Some(cookie) = refresh {
        let handler_set_it = resp
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.starts_with(&format!("{COOKIE_NAME}=")));
        if !handler_set_it {
            resp.headers_mut().append(
                header::SET_COOKIE,
                set_cookie(&cookie, secure_cookies(&state.config.public_url)),
            );
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_value_finds_setu_session_among_others() {
        let mut h = HeaderMap::new();
        h.append(
            header::COOKIE,
            "a=1; setu_session=abc_DEF-1; b=2".parse().unwrap(),
        );
        assert_eq!(cookie_value(&h).as_deref(), Some("abc_DEF-1"));
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, "x=1".parse().unwrap());
        h.append(header::COOKIE, "setu_session=zz".parse().unwrap());
        assert_eq!(cookie_value(&h).as_deref(), Some("zz"));
        let mut h = HeaderMap::new();
        h.append(header::COOKIE, "setu_session=".parse().unwrap());
        assert_eq!(cookie_value(&h), None);
    }

    #[test]
    fn cookie_attributes_follow_the_scheme() {
        let plain = set_cookie("v", false);
        let plain = plain.to_str().unwrap();
        assert!(plain.contains("HttpOnly") && plain.contains("SameSite=Lax"));
        assert!(!plain.contains("Secure"));
        assert!(set_cookie("v", true)
            .to_str()
            .unwrap()
            .ends_with("; Secure"));
        assert!(clear_cookie(false).to_str().unwrap().contains("Max-Age=0"));
        assert!(secure_cookies(&"https://p.example".parse().unwrap()));
        assert!(!secure_cookies(&"http://127.0.0.1:7000".parse().unwrap()));
    }
}
