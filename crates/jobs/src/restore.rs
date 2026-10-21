//! Restore state machine (spec §6), named steps resumed from the
//! job's cursor: `validate`, `backup` (`pre_restore`), `stop`, `restore`
//! (database replaced, dirs swapped), `start`, `health`, `finish`.
//!
//! A failure before `restore` (the data is untouched) leaves the site
//! `running`; at or after `restore` it is `failed` with the step.
//!
//! A scheduled reset (0.1.4 §2, `reset`) passes `backup` without taking
//! one: a sandbox reset every hour would otherwise keep a month of
//! `pre_restore` copies.

use setu_common::AppError;
use setu_proto::Command;

use crate::{build_spec, load_manifest, mark_failed, JobCtx, Steps};

/// The steps, in order.
pub const STEPS: &[&str] = &[
    "validate", "backup", "stop", "restore", "start", "health", "finish",
];

/// Restore the site from `backup_id`; ends `running`. `reset` skips the
/// `pre_restore` backup.
pub async fn run(
    ctx: &JobCtx,
    site_id: &str,
    backup_id: &str,
    reset: bool,
) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let steps = Steps::run_from(ctx.cursor().await.as_deref(), STEPS);
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    let target = crate::server_target(&ctx.db, &site.server_id).await?;
    let spec = build_spec(&plan, &site, &manifest, target)?;
    let server = site.server_id.as_str();
    let short = site.short_id.clone();

    steps
        .run(ctx, "validate", async {
            let backup = ctx.db.backups().get(backup_id).await?;
            if backup.site_id.as_deref() != Some(site.id.as_str()) {
                return Err(AppError::validation("backup does not belong to this site"));
            }
            if backup.status != "completed" {
                return Err(AppError::validation(format!(
                    "backup {} is {}",
                    backup.id, backup.status
                )));
            }
            ctx.db.sites().set_status(site_id, "updating", None).await?;
            Ok(())
        })
        .await?;

    let untouched = |e: AppError| async move {
        let _ = ctx.db.sites().set_status(site_id, "running", None).await;
        e
    };
    let pre_backup = async {
        if reset {
            Ok(())
        } else {
            crate::backup::take(ctx, &site, "pre_restore", "pre_backup_id")
                .await
                .map(|_| ())
        }
    };
    if let Err(e) = steps.run(ctx, "backup", pre_backup).await {
        return Err(untouched(e).await);
    }
    if let Err(e) = steps
        .run(
            ctx,
            "stop",
            ctx.agent_ok(
                server,
                Command::Stop {
                    site: short.clone(),
                },
            ),
        )
        .await
    {
        return Err(untouched(e).await);
    }

    let after: [(&'static str, Command); 3] = [
        (
            "restore",
            Command::Restore {
                site: short.clone(),
                db: site.db_name.clone(),
                role: site.db_role.clone(),
                dirs: manifest.backup.dirs.clone(),
                backup_id: backup_id.to_string(),
            },
        ),
        (
            "start",
            Command::Start {
                site: short.clone(),
            },
        ),
        ("health", Command::Health { site: spec }),
    ];
    for (name, cmd) in after {
        if let Err(e) = steps.run(ctx, name, ctx.agent_ok(server, cmd)).await {
            return Err(mark_failed(ctx, site_id, name, e).await);
        }
    }
    if let Err(e) = steps
        .run(ctx, "finish", async {
            ctx.db.sites().set_status(site_id, "running", None).await?;
            Ok(())
        })
        .await
    {
        return Err(mark_failed(ctx, site_id, "finish", e).await);
    }
    Ok(())
}
