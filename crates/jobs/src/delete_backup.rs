//! `delete_backup`: remove one backup's files on its server, then mark
//! the row `deleted` (spec §6). Works for backups whose site is gone:
//! the row keeps the server and the short id.
//!
//! Steps: `validate`, `delete_backup`, `finish`. A row already `deleted`
//! is an idempotent success.

use setu_common::{AppError, ErrorCode};
use setu_proto::Command;

use crate::{call_agent, log_step, require_ok, JobCtx};

/// Delete backup `backup_id`.
pub async fn run(ctx: &JobCtx, backup_id: &str) -> Result<(), AppError> {
    let backup = ctx.db.backups().get(backup_id).await?;
    log_step(ctx, "validate", true, None).await;
    if backup.status == "deleted" {
        log_step(ctx, "finish", true, None).await;
        return Ok(());
    }
    let called = call_agent(
        ctx,
        &backup.server_id,
        Command::DeleteBackup {
            site: backup.site_short_id.clone(),
            backup_id: backup.id.clone(),
        },
    )
    .await
    .map_err(AppError::from)
    .and_then(|r| require_ok(r).map_err(|msg| AppError::new(ErrorCode::Internal, msg)));
    if let Err(e) = called {
        log_step(ctx, "delete_backup", false, Some(&e.message.clone())).await;
        return Err(e);
    }
    log_step(ctx, "delete_backup", true, None).await;
    ctx.db.backups().mark_deleted(&backup.id).await?;
    log_step(ctx, "finish", true, None).await;
    Ok(())
}
