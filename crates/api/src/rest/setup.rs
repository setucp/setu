//! `GET/POST /api/v1/setup` and `POST /api/v1/setup/local-server` —
//! the first-run wizard (spec §7, §8).
//!
//! On boot with zero operators and no stored token hash, `setu serve`
//! mints a one-time setup token ([`ensure_setup_token`]): 32 random bytes
//! (base64url), its SHA-256 hex in `settings.setup_token_hash`, the token
//! itself in `<data_dir>/setup-token` (mode 0600 from creation). `GET` is
//! public and reports whether the owner still needs creating. `POST`
//! takes the token: a wrong one is 401, the first right one creates the
//! owner under an advisory lock (201) and deletes the hash and the file;
//! once an operator exists every `POST` is 409. `POST /local-server`
//! (owner) registers this box as a pending control-plane server and
//! returns its install command plus, separately, the join token.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use axum::{extract::State, http::StatusCode, response::IntoResponse, Json};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use setu_common::{AppError, Listen};
use setu_db::Db;
use utoipa::ToSchema;

use super::auth::OperatorOut;
use super::servers::{control_plane_url, ServerOut};
use super::ApiJson;
use crate::auth::{audit, hash_password, require_role, ClientIp, Role, Session};
use crate::state::AppState;

/// File name of the setup token under `data_dir`.
pub const SETUP_TOKEN_FILE: &str = "setup-token";
/// Caddy's local CA root on the panel host (`tls internal`).
pub const CADDY_LOCAL_CA: &str = "/var/lib/caddy/.local/share/caddy/pki/authorities/local/root.crt";

#[derive(Debug, Serialize, ToSchema)]
pub struct SetupStatusOut {
    /// True while no operator exists (the wizard must run).
    pub needed: bool,
    /// The configured `public_url`, shown by the wizard.
    pub public_url: String,
}

#[derive(Deserialize, ToSchema)]
pub struct SetupRequest {
    /// The one-time token from `<data_dir>/setup-token`.
    pub token: String,
    pub email: String,
    pub password: String,
}

// Holds credentials: `Debug` redacts the token and the password.
impl std::fmt::Debug for SetupRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupRequest")
            .field("token", &"[redacted]")
            .field("email", &self.email)
            .field("password", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct LocalServerRequest {
    /// Explicit server name; defaults to the machine's hostname.
    pub name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct LocalServerResponse {
    pub server: ServerOut,
    /// Single-use join token (valid 1 hour); shown once. Pass it as
    /// `SETU_JOIN_TOKEN` or on stdin, never on the command line.
    pub join_token: String,
    /// `setu agent install --control-plane …` for this box (no token).
    pub install_command: String,
}

// Holds the join token: `Debug` redacts it.
impl std::fmt::Debug for LocalServerResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalServerResponse")
            .field("server", &self.server.id)
            .field("join_token", &"[redacted]")
            .field("install_command", &self.install_command)
            .finish()
    }
}

/// The control-plane host's name for the default local-server name.
/// Falls back to `local-server` when the hostname is unavailable.
pub fn local_hostname() -> String {
    hostname::get()
        .ok()
        .and_then(|h| h.into_string().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "local-server".to_string())
}

fn sha256_hex(s: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(s.as_bytes()))
}

/// Boot step (spec §8): with zero operators, make sure a setup token
/// exists. Mints one when no hash is stored, or when the hash is stored
/// but the file is gone (the token can then only be read from the file
/// anyway). Returns the file's path when a token was written. The token
/// is never logged.
pub async fn ensure_setup_token(db: &Db, data_dir: &Path) -> Result<Option<PathBuf>, AppError> {
    if db.operators().count().await? > 0 {
        return Ok(None);
    }
    let path = data_dir.join(SETUP_TOKEN_FILE);
    let stored = db.settings().get(setu_db::SETUP_TOKEN_HASH).await?;
    if stored.is_some() && path.exists() {
        return Ok(None);
    }
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(rand::random::<[u8; 32]>());
    write_token_file(data_dir, &path, &token).map_err(|e| {
        tracing::error!("cannot write the setup token to {}: {e}", path.display());
        AppError::internal()
    })?;
    db.settings()
        .set(
            setu_db::SETUP_TOKEN_HASH,
            &serde_json::json!(sha256_hex(&token)),
        )
        .await?;
    tracing::info!("setup token written to {}", path.display());
    Ok(Some(path))
}

/// `<data_dir>/setup-token`, mode 0600 from creation (a stale file is
/// removed first, so the mode never comes from an older file).
fn write_token_file(data_dir: &Path, path: &Path, token: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(token.as_bytes())?;
    file.write_all(b"\n")?;
    file.sync_all()
}

/// Setup status (public, no auth).
#[utoipa::path(
    get,
    path = "/api/v1/setup",
    responses((status = 200, body = SetupStatusOut))
)]
pub async fn status(State(state): State<AppState>) -> Result<impl IntoResponse, AppError> {
    let needed = state.db.operators().count().await? == 0;
    Ok(Json(SetupStatusOut {
        needed,
        public_url: control_plane_url(&state.config.public_url),
    }))
}

/// Create the owner with the setup token. 401 on a wrong token, 201 once,
/// 409 for everyone once any operator exists.
#[utoipa::path(
    post,
    path = "/api/v1/setup",
    request_body = SetupRequest,
    responses(
        (status = 201, body = OperatorOut),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 409, body = super::ErrorBody),
    )
)]
pub async fn create_owner(
    State(state): State<AppState>,
    ip: ClientIp,
    ApiJson(body): ApiJson<SetupRequest>,
) -> Result<impl IntoResponse, AppError> {
    if state.db.operators().count().await? > 0 {
        return Err(AppError::conflict("setup already complete"));
    }
    if !body.email.contains('@') || body.email.len() < 3 {
        return Err(AppError::validation("email must contain @"));
    }
    if body.password.len() < 8 {
        return Err(AppError::validation(
            "password must be at least 8 characters",
        ));
    }
    let hash = hash_password(&body.password)?;
    // Count, token check, insert and hash removal run in one transaction
    // under the setup advisory lock: two concurrent POSTs make one owner.
    let op = state
        .db
        .operators()
        .create_first_owner(&body.email, &hash, &sha256_hex(body.token.trim()))
        .await?;
    let path = state.config.data_dir.join(SETUP_TOKEN_FILE);
    if let Err(e) = std::fs::remove_file(&path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("cannot remove {}: {e}", path.display());
        }
    }
    audit(
        &state.db,
        None,
        "setup.create-owner",
        Some(&op.email),
        ip.as_deref(),
    )
    .await;
    Ok((StatusCode::CREATED, Json(OperatorOut::from_row(&op))))
}

/// The local agent's install command (spec §7): on a TCP listener it
/// joins over loopback; behind the unix socket it joins through Caddy at
/// the public URL, trusting Caddy's local CA when Caddy runs
/// `tls internal`.
pub fn local_install_command(listen: &Listen, public_url: &url::Url, tls_internal: bool) -> String {
    match listen {
        Listen::Tcp(addr) => {
            let host = if addr.is_ipv6() { "[::1]" } else { "127.0.0.1" };
            format!(
                "setu agent install --control-plane http://{host}:{}",
                addr.port()
            )
        }
        Listen::Unix(path) => {
            let mut cmd = format!(
                "setu agent install --control-plane {}",
                control_plane_url(public_url)
            );
            // The agent on the panel's own box re-applies Caddy's base
            // config: it must keep the panel route (reconciled from
            // `agent.toml`) and, under `tls internal`, the internal issuer.
            if let Some(host) = public_url.host_str() {
                cmd.push_str(" --panel-host ");
                cmd.push_str(host);
                let upstream = format!("unix/{}", path.display());
                if upstream != setu_agent::caddy::DEFAULT_PANEL_UPSTREAM {
                    cmd.push_str(" --upstream ");
                    cmd.push_str(&upstream);
                }
            }
            if tls_internal {
                cmd.push_str(" --tls internal --ca ");
                cmd.push_str(CADDY_LOCAL_CA);
            }
            cmd
        }
    }
}

/// Register this box as a pending control-plane server (owner only).
#[utoipa::path(
    post,
    path = "/api/v1/setup/local-server",
    request_body = LocalServerRequest,
    responses(
        (status = 201, body = LocalServerResponse),
        (status = 401, body = super::ErrorBody),
        (status = 403, body = super::ErrorBody),
    )
)]
pub async fn local_server(
    State(state): State<AppState>,
    session: Session,
    ip: ClientIp,
    ApiJson(body): ApiJson<LocalServerRequest>,
) -> Result<impl IntoResponse, AppError> {
    let caller = require_role(&session, Role::Owner).await?;
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(local_hostname);
    let (server, join_token) = state.db.servers().create_pending(&name, true).await?;
    let tls_internal = state
        .db
        .settings()
        .get(setu_db::TLS_INTERNAL)
        .await?
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let command =
        local_install_command(&state.config.listen, &state.config.public_url, tls_internal);
    audit(
        &state.db,
        Some(&caller.id),
        "setup.local-server",
        Some(&server.id),
        ip.as_deref(),
    )
    .await;
    Ok((
        StatusCode::CREATED,
        Json(LocalServerResponse {
            server: ServerOut::from_row(&server, state.clock.now()),
            join_token,
            install_command: command,
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostname_fallback_is_never_empty() {
        assert!(!local_hostname().is_empty());
    }

    #[test]
    fn local_command_uses_loopback_on_tcp_and_public_url_on_unix() {
        let public: url::Url = "https://panel.example/".parse().unwrap();
        let tcp = Listen::Tcp("0.0.0.0:7000".parse().unwrap());
        assert_eq!(
            local_install_command(&tcp, &public, true),
            "setu agent install --control-plane http://127.0.0.1:7000"
        );
        let unix = Listen::Unix("/run/setu/http.sock".into());
        assert_eq!(
            local_install_command(&unix, &public, false),
            "setu agent install --control-plane https://panel.example --panel-host panel.example"
        );
        assert_eq!(
            local_install_command(&unix, &public, true),
            format!(
                "setu agent install --control-plane https://panel.example \
                 --panel-host panel.example --tls internal --ca {CADDY_LOCAL_CA}"
            )
        );
        let other = Listen::Unix("/run/other/panel.sock".into());
        assert_eq!(
            local_install_command(&other, &public, false),
            "setu agent install --control-plane https://panel.example \
             --panel-host panel.example --upstream unix//run/other/panel.sock"
        );
    }
}
