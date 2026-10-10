//! Delete state machine (spec §6), named steps resumed from the job's
//! cursor: `backup` (`pre_delete`, kept 30 days), `remove_route`, `stop`,
//! `drop_database`, `site_remove`, `finish` (the row is deleted; this
//! job keeps its row with `site_id` NULL, so it ends `done` with its log;
//! its backups keep `site_name` and `domain`, `site_id` becomes NULL; the
//! sealed payload fields of the site's jobs are nulled first).
//!
//! A failed `pre_delete` backup aborts the delete with the site
//! untouched, except for a `failed` site, which may never have had a
//! database or files to back up (ledger R94). `remove_route` and `stop`
//! failures are ignored (the route/unit may already be gone); a later
//! failure marks the site `failed` with the step.

use setu_common::{AppError, ErrorCode};
use setu_proto::Command;

use crate::{mark_failed, JobCtx, Steps};

/// The steps, in order.
pub const STEPS: &[&str] = &[
    "backup",
    "remove_route",
    "stop",
    "drop_database",
    "site_remove",
    "finish",
];

/// Delete the site; ends with the row gone.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let cursor = ctx.cursor().await;
    let site = match ctx.db.sites().get(site_id).await {
        // Interrupted after the row went: the delete is complete.
        Err(e) if e.code == ErrorCode::NotFound && cursor.as_deref() == Some("finish") => {
            return Ok(())
        }
        other => other?,
    };
    let steps = Steps::run_from(cursor.as_deref(), STEPS);
    let server = site.server_id.as_str();
    let short = site.short_id.clone();

    let backed_up = steps
        .run(
            ctx,
            "backup",
            crate::backup::take(ctx, &site, "pre_delete", "pre_backup_id"),
        )
        .await;
    if let Err(e) = backed_up {
        if site.status != "failed" {
            return Err(e);
        }
        ctx.log("step backup: ignored (the site is failed)").await;
    }
    ctx.db.sites().set_status(site_id, "deleting", None).await?;

    for (name, cmd) in [
        (
            "remove_route",
            Command::RemoveRoute {
                site_short_id: short.clone(),
                domain: site.domain.clone(),
            },
        ),
        (
            "stop",
            Command::Stop {
                site: short.clone(),
            },
        ),
    ] {
        // Best effort: logged as an error, then the delete continues.
        let _ = steps.run(ctx, name, ctx.agent_ok(server, cmd)).await;
    }
    for (name, cmd) in [
        (
            "drop_database",
            Command::DropDatabase {
                db: site.db_name.clone(),
                role: site.db_role.clone(),
            },
        ),
        (
            "site_remove",
            Command::SiteRemove {
                site: short.clone(),
            },
        ),
    ] {
        if let Err(e) = steps.run(ctx, name, ctx.agent_ok(server, cmd)).await {
            return Err(mark_failed(ctx, site_id, name, e).await);
        }
    }
    // Scrub before the row goes: the FK then unlinks the kept job rows,
    // and they could no longer be found by site (M1).
    let finish = async {
        ctx.db
            .jobs()
            .scrub_payload_secrets_for_site(site_id)
            .await?;
        ctx.db.sites().delete(site_id).await
    };
    if let Err(e) = steps.run(ctx, "finish", finish).await {
        return Err(mark_failed(ctx, site_id, "finish", e).await);
    }
    Ok(())
}
