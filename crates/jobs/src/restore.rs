//! Restore state machine (spec §5): stop, restore the dump and dirs,
//! start, health.
//!
//! Steps: `validate`, `stop`, `restore`, `start`, `health`, `finish`.
//! Pre-mutation failure (`stop`) leaves the site `running`; anything at
//! or after `restore` marks it `failed` (the data was already touched).

use setu_common::AppError;
use setu_proto::Command;

use crate::{agent_step, build_spec, fail, load_manifest, log_step, JobCtx};

/// Restore the site from `backup_id`; ends `running`.
pub async fn run(ctx: &JobCtx, site_id: &str, backup_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let backup = ctx.db.backups().get(backup_id).await?;
    if backup.site_id.as_deref() != Some(site.id.as_str()) {
        return Err(AppError::validation("backup does not belong to this site"));
    }
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    // No env resolution (no prompts on this job, none needed — the env
    // file already on disk is untouched by restore); the spec is for
    // `health`.
    let spec = build_spec(&plan, &site, &manifest)?;
    let server_id = site.server_id.clone();

    log_step(ctx, "validate", true, None).await;
    ctx.db.sites().set_status(site_id, "updating", None).await?;

    match crate::call_agent(
        ctx,
        &server_id,
        Command::Stop {
            site: site.short_id.clone(),
        },
    )
    .await
    {
        Ok(result) if result.ok => {
            log_step(ctx, "stop", true, None).await;
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            log_step(ctx, "stop", false, Some(&msg)).await;
            let _ = ctx.db.sites().set_status(site_id, "running", None).await;
            return Err(AppError::new(setu_common::ErrorCode::Internal, msg));
        }
        Err(e) => {
            let err = AppError::from(e);
            log_step(ctx, "stop", false, Some(&err.message.clone())).await;
            let _ = ctx.db.sites().set_status(site_id, "running", None).await;
            return Err(err);
        }
    }

    agent_step(
        ctx,
        site_id,
        "restore",
        &server_id,
        Command::Restore {
            site: site.short_id.clone(),
            db: site.db_name.clone(),
            role: site.db_role.clone(),
            dirs: manifest.backup.dirs.clone(),
            backup_id: backup.id.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "start",
        &server_id,
        Command::Start {
            site: site.short_id.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "health",
        &server_id,
        Command::Health { site: spec },
    )
    .await?;

    // `agent_step` failures above already marked the site `failed`;
    // only the success path reaches here.
    match ctx.db.sites().set_status(site_id, "running", None).await {
        Ok(_) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}
