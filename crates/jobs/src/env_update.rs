//! `env_update` (spec §6): the operator changed `prompt`/`value`
//! variables (already stored, sealed); write the env file from storage
//! without `once` values, then restart. A job, so it serialises with the
//! site's other work. WS6 may refine (payload-carried changes).
//!
//! Steps: `validate`, `write_env`, `restart`, `finish`; an agent failure
//! marks the site `failed` with the step.

use setu_common::AppError;
use setu_proto::{Command, EnvVar};

use crate::{agent_step, fail, log_step, stored_plaintext, JobCtx};

/// Rewrite the site's env file from storage and restart it.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot update env from status {}",
            site.status
        )));
    }
    let env: Vec<EnvVar> = match stored_plaintext(&site, &ctx.sealer) {
        Ok(vars) => vars
            .into_iter()
            .filter(|(_, (_, _, once))| !once)
            .map(|(name, (value, secret, once))| EnvVar {
                name,
                value,
                secret,
                once,
            })
            .collect(),
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
    log_step(ctx, "validate", true, None).await;
    let short = site.short_id.clone();
    agent_step(
        ctx,
        site_id,
        "write_env",
        &site.server_id,
        Command::WriteEnv {
            site: short.clone(),
            env,
        },
    )
    .await?;
    if site.status == "running" {
        agent_step(
            ctx,
            site_id,
            "restart",
            &site.server_id,
            Command::Restart { site: short },
        )
        .await?;
    }
    log_step(ctx, "finish", true, None).await;
    Ok(())
}
