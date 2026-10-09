//! Operator auth: argon2id passwords, Postgres-backed sessions, role
//! checks and the Origin CSRF guard (spec §7).

use axum::{
    extract::Request,
    http::{HeaderMap, Method},
    middleware::Next,
    response::Response,
};
use setu_common::AppError;
use setu_db::{Db, Operator};
use tower_sessions::Session;

/// Session key holding the logged-in operator's id.
pub const SESSION_OPERATOR_ID: &str = "operator_id";

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

/// The logged-in operator, or 401 when anonymous (or the session points at
/// a deleted operator).
pub async fn current_operator(session: &Session, db: &Db) -> Result<Operator, AppError> {
    let id: Option<String> = session.get(SESSION_OPERATOR_ID).await.map_err(|e| {
        tracing::error!("session load failed: {e}");
        AppError::internal()
    })?;
    match id {
        None => Err(AppError::unauthorized("not authenticated")),
        Some(id) => match db.operators().by_id(&id).await? {
            Some(op) => Ok(op),
            None => Err(AppError::unauthorized("not authenticated")),
        },
    }
}

/// [`current_operator`] plus a minimum-role check; below the minimum is 403.
pub async fn require_role(session: &Session, db: &Db, min: Role) -> Result<Operator, AppError> {
    let op = current_operator(session, db).await?;
    let have = Role::parse(&op.role)?;
    if have < min {
        return Err(AppError::forbidden("insufficient role"));
    }
    Ok(op)
}

/// CSRF guard (spec §7: same-site cookies plus origin check): on
/// POST/PUT/DELETE/PATCH, when an `Origin` header is present it must match
/// the request `Host` (host and effective port), else 403. Requests without
/// `Origin` (curl, non-browser clients) pass.
pub async fn csrf_middleware(req: Request, next: Next) -> Result<Response, AppError> {
    let is_mutating = matches!(
        req.method(),
        &Method::POST | &Method::PUT | &Method::DELETE | &Method::PATCH
    );
    if is_mutating {
        if let Some(origin) = req.headers().get("origin").cloned() {
            check_origin(req.headers(), origin.to_str().unwrap_or_default())?;
        }
    }
    Ok(next.run(req).await)
}

fn check_origin(headers: &HeaderMap, origin: &str) -> Result<(), AppError> {
    let origin_url =
        url::Url::parse(origin).map_err(|_| AppError::forbidden("invalid Origin header"))?;
    let origin_host = origin_url
        .host_str()
        .ok_or_else(|| AppError::forbidden("invalid Origin header"))?;
    let origin_port = origin_url.port_or_known_default().unwrap_or(80);

    let host_header = headers
        .get("host")
        .and_then(|h| h.to_str().ok())
        .unwrap_or_default();
    let (host_name, host_port) = split_host(host_header, origin_url.scheme());

    if !norm_host(origin_host).eq_ignore_ascii_case(norm_host(host_name))
        || origin_port != host_port
    {
        return Err(AppError::forbidden("origin does not match host"));
    }
    Ok(())
}

/// Split a `Host` header into (name, effective port); a missing port
/// defaults by the origin scheme (`http` → 80, `https` → 443).
fn split_host<'a>(host: &'a str, scheme: &str) -> (&'a str, u16) {
    let default_port = if scheme == "https" { 443 } else { 80 };
    // `rsplit_once(':')` also handles `[::1]:7000`; a bare `[::1]` has no
    // port and falls through to the default.
    if let Some((name, port_str)) = host.rsplit_once(':') {
        if let Ok(port) = port_str.parse::<u16>() {
            return (name, port);
        }
    }
    (host, default_port)
}

/// Strip one pair of brackets around an IPv6 literal so `[::1]` and `::1`
/// compare equal. `Url::host_str` keeps the brackets while some clients
/// and proxies send the `Host` header without them (and vice versa).
fn norm_host(h: &str) -> &str {
    if let Some(inner) = h.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        inner
    } else {
        h
    }
}

/// Record one audit row; failures are logged, never fatal to the request.
pub async fn audit(
    db: &Db,
    operator_id: Option<&str>,
    action: &str,
    target: Option<&str>,
    ip: Option<&str>,
) {
    if let Err(e) = db.audit().record(operator_id, action, target, ip).await {
        tracing::error!(error = %e, "audit record failed");
    }
}

/// Best-effort client IP for audit rows: explicit override first, else the
/// socket peer address.
pub fn audit_ip(explicit: Option<String>, peer: Option<std::net::SocketAddr>) -> Option<String> {
    explicit.or_else(|| peer.map(|s| s.ip().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderMap;

    fn headers_with_host(host: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("host", host.parse().expect("host header parses"));
        headers
    }

    #[test]
    fn ipv6_origin_with_brackets_matches_bracketed_host() {
        let headers = headers_with_host("[::1]:7000");
        check_origin(&headers, "http://[::1]:7000").expect("matching IPv6 origin passes");
    }

    #[test]
    fn ipv6_origin_with_brackets_matches_bracketless_host() {
        // Some clients/proxies send `Host: ::1` without brackets while
        // `Url::host_str` keeps them; both directions must compare equal.
        let headers = headers_with_host("[::1]:7000");
        check_origin(&headers, "http://[::1]:7000").expect("bracketed pair passes");
        let bare = headers_with_host("::1:7000");
        check_origin(&bare, "http://[::1]:7000").expect("bracketless host passes");
    }

    #[test]
    fn ipv6_origin_mismatched_port_is_rejected() {
        let headers = headers_with_host("[::1]:7000");
        let err = check_origin(&headers, "http://[::1]:7001").expect_err("wrong port rejects");
        assert_eq!(err.code, setu_common::ErrorCode::Forbidden);
    }

    #[test]
    fn ipv4_origin_still_matches() {
        let headers = headers_with_host("127.0.0.1:7000");
        check_origin(&headers, "http://127.0.0.1:7000").expect("IPv4 pair passes");
    }
}
