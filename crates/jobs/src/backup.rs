//! Manual backup state machine: agent `Backup` plus the backup row.
//!
//! Steps: `validate`, `backup`, `finish`. Never touches site status.

use std::path::PathBuf;

use setu_common::{AppError, ErrorCode};
use setu_proto::Command;

use crate::update::backup_dest;
use crate::{build_spec, call_agent, load_manifest, log_step, require_ok, JobCtx};

/// Take a `manual` backup of the site; the row ends `completed`.
pub async fn run(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    // No env resolution here: backup needs paths + db name only, and the
    // backup job carries no prompt answers (they live on the provision
    // job's payload). Re-resolving would fail on prompt manifests and
    // mint fresh secrets pointlessly.
    let spec = build_spec(&plan, &site, &manifest)?;
    log_step(ctx, "validate", true, None).await;

    let row = ctx.db.backups().create(&site, "manual").await?;
    let dest = backup_dest(&site.short_id, &row.id);
    match call_agent(
        ctx,
        &site.server_id,
        Command::Backup {
            site: spec,
            db: site.db_name.clone(),
            dirs: manifest.backup.dirs.clone(),
            dest: PathBuf::from(&dest),
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
