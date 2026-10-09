//! Delete state machine (spec §5): stop, remove the route, drop the
//! database, remove the files, delete the row.
//!
//! Steps: `stop`, `remove_route`, `drop_database`, `site_remove`,
//! `finish` (WS6 adds the `pre_delete` backup first).
//! `stop` and `remove_route` failures are ignored (the unit/route may
//! already be gone); anything later marks the site `failed`.

use setu_common::{AppError, ErrorCode};
use setu_proto::Command;

use crate::{call_agent, fail, log_step, JobCtx};

/// Delete the site; ends with the row gone.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let server_id = site.server_id.clone();
    let domain = site.domain.clone();
    // Every command keys off the short id: no manifest or plan needed.
    let short = site.short_id.clone();
    let db_name = site.db_name.clone();

    let _ = ctx.db.sites().set_status(site_id, "deleting", None).await;

    // Best-effort: the unit may already be stopped/gone.
    match call_agent(
        ctx,
        &server_id,
        Command::Stop {
            site: short.clone(),
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
            site_short_id: short.clone(),
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
            role: site.db_role.clone(),
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

    match call_agent(ctx, &server_id, Command::SiteRemove { site: short }).await {
        Ok(result) if result.ok => {
            log_step(ctx, "site_remove", true, None).await;
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            return Err(fail(
                ctx,
                site_id,
                "site_remove",
                AppError::new(ErrorCode::Internal, msg),
            )
            .await);
        }
        Err(e) => return Err(fail(ctx, site_id, "site_remove", e.into()).await),
    }

    match ctx.db.sites().delete(site_id).await {
        Ok(()) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}
