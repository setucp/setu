//! `delete_backup`: remove one backup's files on its server, then mark
//! the row `deleted` (spec §6). Works for backups whose site is gone:
//! the row keeps the server and the short id.
//!
//! Steps: `validate`, `delete_backup`, `finish`. A row already `deleted`
//! is an idempotent success.

use setu_common::AppError;
use setu_proto::Command;

use crate::JobCtx;

/// Delete backup `backup_id`.
pub async fn run(ctx: &JobCtx, backup_id: &str) -> Result<(), AppError> {
    let backup = ctx
        .step("validate", ctx.db.backups().get(backup_id))
        .await?;
    if backup.status != "deleted" {
        ctx.step(
            "delete_backup",
            ctx.agent_ok(
                &backup.server_id,
                Command::DeleteBackup {
                    site: backup.site_short_id.clone(),
                    backup_id: backup.id.clone(),
                },
            ),
        )
        .await?;
    }
    ctx.step("finish", async {
        ctx.db.backups().mark_deleted(&backup.id).await?;
        Ok(())
    })
    .await
}
