//! Backup (`backup_site`, and the `pre_*` copies the other flows take):
//! the `backups` row first (`pending`), then the agent `Backup` into
//! `/var/lib/setu/backups/<short>/<backup_id>/`, then the row `completed`
//! with the size the agent reports (or `failed`). Never touches site
//! status.

use setu_common::AppError;
use setu_db::Site;
use setu_proto::Command;

use crate::{load_manifest, JobCtx};

/// Take a backup of `kind` (`manual`, `scheduled`, or a flow's `pre_*`)
/// as the step `backup`; returns the backup id. The id is kept in the
/// job payload (`backup_id`), so a resumed job re-uses its row.
pub async fn run(ctx: &JobCtx, site_id: &str, kind: &str) -> Result<String, AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    ctx.step("backup", take(ctx, &site, kind, "backup_id"))
        .await
}

/// The body of a backup step: re-use the row named by `payload_key` in
/// the job payload (resume), else create a `pending` row of `kind` and
/// record its id there before calling the agent. A row already
/// `completed` is not taken again.
#[allow(clippy::similar_names)] // reviewed: kept as-is (R150)
pub(crate) async fn take(
    ctx: &JobCtx,
    site: &Site,
    kind: &str,
    payload_key: &str,
) -> Result<String, AppError> {
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    let row = if let Some(id) = ctx.payload_str(payload_key).await {
        ctx.db.backups().get(&id).await?
    } else {
        let row = ctx.db.backups().create(site, kind).await?;
        ctx.payload_set(payload_key, serde_json::json!(row.id))
            .await?;
        row
    };
    if row.status == "completed" {
        return Ok(row.id);
    }
    let taken = ctx
        .agent_ok(
            &site.server_id,
            Command::Backup {
                site: site.short_id.clone(),
                db: site.db_name.clone(),
                dirs: manifest.backup.dirs.clone(),
                backup_id: row.id.clone(),
            },
        )
        .await;
    match taken {
        Ok(output) => {
            let size = output.get("size_bytes").and_then(serde_json::Value::as_i64);
            ctx.db
                .backups()
                .set_status(&row.id, "completed", size)
                .await?;
            Ok(row.id)
        }
        Err(e) => {
            let _ = ctx.db.backups().set_status(&row.id, "failed", None).await;
            Err(e)
        }
    }
}
