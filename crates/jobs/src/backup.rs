//! Backup state machine (`backup_site`): agent `Backup` plus the row.
//!
//! Steps: `validate`, `backup`, `finish`. Never touches site status.
//! The kind (`manual`, `scheduled`) comes from the job payload; the
//! agent writes `/var/lib/setu/backups/<short>/<backup_id>/`.

use setu_common::{AppError, ErrorCode};
use setu_proto::Command;

use crate::{call_agent, load_manifest, log_step, require_ok, JobCtx};

/// Take a backup of `kind` (`manual` or `scheduled`); the row ends
/// `completed` (or `failed`).
pub async fn run(ctx: &JobCtx, site_id: &str, kind: &str) -> Result<(), AppError> {
    if kind != "manual" && kind != "scheduled" {
        return Err(AppError::validation(format!(
            "backup kind {kind} is not a job"
        )));
    }
    let site = ctx.db.sites().get(site_id).await?;
    // No env resolution here: backup needs the db name and the dirs only.
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    log_step(ctx, "validate", true, None).await;

    let row = ctx.db.backups().create(&site, kind).await?;
    match call_agent(
        ctx,
        &site.server_id,
        Command::Backup {
            site: site.short_id.clone(),
            db: site.db_name.clone(),
            dirs: manifest.backup.dirs.clone(),
            backup_id: row.id.clone(),
        },
    )
    .await
    {
        Ok(result) => match require_ok(result) {
            Ok(output) => {
                let size = output.get("size_bytes").and_then(|v| v.as_i64());
                let _ = ctx
                    .db
                    .backups()
                    .set_status(&row.id, "completed", size)
                    .await;
                log_step(ctx, "backup", true, None).await;
            }
            Err(msg) => {
                let _ = ctx.db.backups().set_status(&row.id, "failed", None).await;
                log_step(ctx, "backup", false, Some(&msg)).await;
                return Err(AppError::new(ErrorCode::Internal, msg));
            }
        },
        Err(e) => {
            let _ = ctx.db.backups().set_status(&row.id, "failed", None).await;
            let err = AppError::from(e);
            log_step(ctx, "backup", false, Some(&err.message.clone())).await;
            return Err(err);
        }
    }

    log_step(ctx, "finish", true, None).await;
    Ok(())
}
