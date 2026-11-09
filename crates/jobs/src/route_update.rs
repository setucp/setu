//! `route_update` (0.1.7): the site's Caddy route is rewritten from the
//! stored basic-auth credentials (set, replaced or removed through
//! `PUT`/`DELETE /sites/{id}/basic-auth`). One agent step, `set_route`,
//! carrying the stored user name and bcrypt hash (never a password). The
//! site keeps its status: a failed route change leaves the old route in
//! place and fails only the job.

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
        let cmd = route_command(ctx, site_id).await?;
        ctx.agent_ok(&site.server_id, cmd).await.map(drop)
    })
    .await?;
    ctx.step("finish", async { Ok(()) }).await
}
