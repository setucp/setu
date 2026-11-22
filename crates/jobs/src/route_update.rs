//! `route_update` (0.1.7): the site's Caddy route is rewritten from the
//! stored basic-auth credentials (set, replaced or removed through
//! `PUT`/`DELETE /sites/{id}/basic-auth`). One agent step, `set_route`,
//! carrying the stored user name and bcrypt hash (never a password). The
//! site keeps its status: a failed route change leaves the old route in
//! place and fails only the job, and `GET /sites/{id}/basic-auth` reports
//! it (`route.status = failed`, read from this job) until a later change
//! applies.

use setu_common::AppError;
use setu_proto::{BasicAuth, Command};

use crate::JobCtx;

/// The `SetRoute` command for a site from its stored credentials.
pub async fn route_command(ctx: &JobCtx, site_id: &str) -> Result<Command, AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let auth = ctx.db.basic_auths().get(site_id).await?;
    let port = u16::try_from(site.port)
        .map_err(|_| AppError::validation(format!("site {site_id} has an invalid port")))?;
    Ok(Command::SetRoute {
        site_short_id: site.short_id,
        domain: site.domain,
        port,
        basic_auth: auth.map(|a| BasicAuth {
            username: a.username,
            password_hash: a.password_hash,
        }),
        www_redirect: site.www_redirect,
        force_reload: false,
    })
}

/// Run the job for `site_id`.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot change its route from status {}",
            site.status
        )));
    }
    ctx.step("set_route", async {
        let mut cmd = route_command(ctx, site_id).await?;
        // The certificate retry (`payload.reload`): Caddy must reload the
        // route even though nothing in it changed.
        if let Command::SetRoute { force_reload, .. } = &mut cmd {
            *force_reload = ctx
                .payload_get("reload")
                .await
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
        }
        ctx.agent_ok(&site.server_id, cmd).await.map(drop)
    })
    .await?;
    ctx.step("finish", async { Ok(()) }).await
}
