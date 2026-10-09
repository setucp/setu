//! `/agent/v1` endpoints: the long-poll agent protocol (spec §4).
//!
//! Agents authenticate with `Authorization: Bearer <server token>` —
//! never sessions or cookies, so these routes sit outside the CSRF layer
//! (wired in [`super::router`]). Server `status` stays `pending` until
//! the first join and is otherwise computed from `last_seen_at` at read
//! time ([`super::servers::effective_status`]), so no write path here
//! needs to flip it.

use std::time::Duration;

use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use setu_common::AppError;
use setu_db::Server;
use setu_proto::{CommandEnvelope, CommandResult, Heartbeat, JoinRequest, JoinResponse};

use crate::state::AppState;

use super::ApiJson;

/// Long-poll ceiling: default and cap (spec §4: `?wait=30`).
const MAX_WAIT_SECS: u64 = 30;
/// Queue poll cadence inside the long-poll window. Task 8 review ruling
/// (see `docs/plans/2026-10-09-setu-mvp.ledger.md`): plain DB polling is
/// fine at MVP scale, no `Notify`; `DbAgentClient` keeps its own 500 ms
/// `wait`. Kept as a named const so the cadence is greppable/tunable.
const POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The agent behind `Authorization: Bearer <server token>`, or 401. The
/// scheme match is case-insensitive (RFC 7235); a missing header, a wrong
/// scheme, or an empty/whitespace token is `unauthorized`, as is an
/// unknown token. Token values are never logged.
async fn agent_server(headers: &HeaderMap, state: &AppState) -> Result<Server, AppError> {
    let unauthorized = || AppError::unauthorized("invalid server token");
    let raw = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(unauthorized)?;
    let (scheme, token) = raw.split_once(' ').ok_or_else(unauthorized)?;
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(unauthorized());
    }
    let token = token.trim();
    if token.is_empty() || token.contains(char::is_whitespace) {
        return Err(unauthorized());
    }
    state
        .db
        .servers()
        .by_token(token)
        .await?
        .ok_or_else(unauthorized)
}

/// Redeem a single-use join token (`JoinRequest` → `JoinResponse`). A
/// spent, unknown or expired token is 401 from the repo; the join marks
/// the server `online`.
#[utoipa::path(
    post,
    path = "/agent/v1/join",
    request_body = JoinRequest,
    responses(
        (status = 200, body = JoinResponse),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn join(
    State(state): State<AppState>,
    ApiJson(body): ApiJson<JoinRequest>,
) -> Result<impl IntoResponse, AppError> {
    // The agent's random nonce: a retry within 60 s with the same nonce
    // gets the same token, any other nonce is refused (spec §5, R76).
    if body.nonce.is_empty() || body.nonce.len() > 128 {
        return Err(AppError::validation("join nonce must be 1-128 characters"));
    }
    let input = setu_db::JoinInput {
        nonce: body.nonce.clone(),
        hostname: body.hostname.clone(),
        public_ip: None,
        arch: body.arch.clone(),
        os: body.os.clone(),
        cpu_cores: i32::try_from(body.cpu_cores).unwrap_or(i32::MAX),
        memory_mb: i64::try_from(body.memory_mb).unwrap_or(i64::MAX),
        disk_mb: i64::try_from(body.disk_mb).unwrap_or(i64::MAX),
    };
    let (server, token) = state
        .db
        .servers()
        .join(&body.join_token, &input, &state.sealer)
        .await?;
    tracing::info!(server_id = %server.id, "agent joined");
    Ok(Json(JoinResponse {
        server_id: server.id,
        server_token: token,
    }))
}

#[derive(Debug, Deserialize)]
pub struct CommandsQuery {
    /// Long-poll ceiling in seconds (`30`, or `30s` per spec §4);
    /// default and cap 30. `0` polls once and returns 204 when empty.
    pub wait: Option<String>,
}

/// Parse `?wait=` (plain seconds, with one optional trailing `s`) into a
/// capped budget; anything unparsable is a `validation` 400. The suffix
/// is stripped exactly once, so `30s` is accepted but `30ss` is rejected.
fn parse_wait(raw: Option<String>) -> Result<Duration, AppError> {
    let Some(raw) = raw else {
        return Ok(Duration::from_secs(MAX_WAIT_SECS));
    };
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_suffix('s').unwrap_or(trimmed).trim();
    let secs: u64 = trimmed
        .parse()
        .map_err(|_| AppError::validation("invalid ?wait=: expected seconds (e.g. ?wait=30)"))?;
    Ok(Duration::from_secs(secs.min(MAX_WAIT_SECS)))
}

/// The next undelivered command for this server (200), or 204 when none
/// arrives within the wait window. Delivery marks the command delivered
/// and counts the attempt; a new agent `boot_id` requeues delivered
/// commands (spec §5). Polled per server at 600/min
/// (spec §7); past that the poll is 429.
#[utoipa::path(
    get,
    path = "/agent/v1/commands",
    params(("wait" = Option<String>, Query, description = "Long-poll ceiling in seconds, default/cap 30")),
    responses(
        (status = 200, body = CommandEnvelope, description = "Next undelivered command"),
        (status = 204, description = "No command within the wait window"),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 429, body = super::ErrorBody),
    )
)]
pub async fn commands(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<CommandsQuery>,
) -> Result<Response, AppError> {
    let server = agent_server(&headers, &state).await?;
    state.check_agent_rate(&server.id).await?;
    let budget = parse_wait(query.wait)?;
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        if let Some(envelope) = state.db.commands().next_for(&server.id).await? {
            return Ok(Json(envelope).into_response());
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        tokio::time::sleep(remaining.min(POLL_INTERVAL)).await;
    }
}

/// Report a command result; unblocks the `DbAgentClient::call` waiting on
/// it. Scoped to the authed server: a command owned by another server is
/// 404, so agents cannot complete each other's commands. Unknown ids are
/// 404; a body id that disagrees with the path is a 400 (the path is
/// authoritative). Repeat posts are an ok idempotent no-op (first writer
/// wins, no overwrite — see `complete_for`).
#[utoipa::path(
    post,
    path = "/agent/v1/commands/{id}/result",
    params(("id" = String, Path, description = "Command id")),
    request_body = CommandResult,
    responses(
        (status = 204, description = "Result recorded"),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
        (status = 404, body = super::ErrorBody),
    )
)]
pub async fn result(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    ApiJson(body): ApiJson<CommandResult>,
) -> Result<impl IntoResponse, AppError> {
    let server = agent_server(&headers, &state).await?;
    if body.id != id {
        return Err(AppError::validation("result id does not match command id"));
    }
    state
        .db
        .commands()
        .complete_for(&server.id, &id, &body)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Heartbeat: marks the server `online` and stores the metrics batch
/// (the repo upserts both atomically).
#[utoipa::path(
    post,
    path = "/agent/v1/heartbeat",
    request_body = Heartbeat,
    responses(
        (status = 204, description = "Heartbeat recorded"),
        (status = 400, body = super::ErrorBody),
        (status = 401, body = super::ErrorBody),
    )
)]
pub async fn heartbeat(
    State(state): State<AppState>,
    headers: HeaderMap,
    ApiJson(body): ApiJson<Heartbeat>,
) -> Result<impl IntoResponse, AppError> {
    let server = agent_server(&headers, &state).await?;
    // A new `boot_id` (agent restart or host reboot) requeues every
    // command delivered to the old process (spec §5).
    let boot_id = (!body.boot_id.is_empty()).then_some(body.boot_id.as_str());
    let outcome = state
        .db
        .servers()
        .heartbeat(
            &server.id,
            &setu_db::HeartbeatInput {
                agent_version: &body.agent_version,
                boot_id,
                metrics: &body.metrics,
            },
        )
        .await?;
    if outcome.boot_changed {
        state.db.commands().requeue_delivered(&server.id).await?;
    }
    Ok(StatusCode::NO_CONTENT)
}
