//! Delete state machine (spec §5): stop, remove the route, drop the
//! database, remove the files, delete the row.
//!
//! Steps: `stop`, `remove_route`, `drop_database`, `remove`, `finish`.
//! `stop` and `remove_route` failures are ignored (the unit/route may
//! already be gone); anything later marks the site `failed`.

use setu_common::{AppError, ErrorCode};
use setu_proto::Command;

use crate::{build_spec, call_agent, fail, load_manifest, log_step, minimal_spec, JobCtx};

/// Delete the site; ends with the row gone.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let server_id = site.server_id.clone();
    let domain = site.domain.clone();
    let db_name = site.db_name.clone();

    // `build_spec` needs no prompts (Stop/Remove key off `site_id`);
    // fall back to `minimal_spec` when the manifest/plan is gone.
    let spec = match load_manifest(&ctx.db, &site.app_id, &site.app_version).await {
        Ok(manifest) => match ctx.db.plans().get(&site.plan_id).await {
            Ok(plan) => build_spec(&plan, &site, &manifest).unwrap_or_else(|_| minimal_spec(&site)),
            Err(_) => minimal_spec(&site),
        },
        Err(_) => minimal_spec(&site),
    };

    let _ = ctx.db.sites().set_status(site_id, "deleting", None).await;

    // Best-effort: the unit may already be stopped/gone.
    match call_agent(
        ctx,
        &server_id,
        Command::Stop {
            site_id: site.id.clone(),
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
            log_step(ctx, "stop", false, Some(&format!("{msg} (ignored)"))).await;
        }
        Err(e) => {
            log_step(ctx, "stop", false, Some(&format!("{e} (ignored)"))).await;
        }
    }
    match call_agent(
        ctx,
        &server_id,
        Command::RemoveRoute {
            domain: domain.clone(),
        },
    )
    .await
    {
        Ok(result) if result.ok => {
            log_step(ctx, "remove_route", true, None).await;
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            log_step(
                ctx,
                "remove_route",
                false,
                Some(&format!("{msg} (ignored)")),
            )
            .await;
        }
        Err(e) => {
            log_step(ctx, "remove_route", false, Some(&format!("{e} (ignored)"))).await;
        }
    }

    match call_agent(
        ctx,
        &server_id,
        Command::DropDatabase {
            db: db_name.clone(),
            role: db_name.clone(),
        },
    )
    .await
    {
        Ok(result) if result.ok => {
            log_step(ctx, "drop_database", true, None).await;
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            return Err(fail(
                ctx,
                site_id,
                "drop_database",
                AppError::new(ErrorCode::Internal, msg),
            )
            .await);
        }
        Err(e) => return Err(fail(ctx, site_id, "drop_database", e.into()).await),
    }

    match call_agent(ctx, &server_id, Command::Remove { site: spec }).await {
        Ok(result) if result.ok => {
            log_step(ctx, "remove", true, None).await;
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            return Err(fail(
                ctx,
                site_id,
                "remove",
                AppError::new(ErrorCode::Internal, msg),
            )
            .await);
        }
        Err(e) => return Err(fail(ctx, site_id, "remove", e.into()).await),
    }

    match ctx.db.sites().delete(site_id).await {
        Ok(()) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}
