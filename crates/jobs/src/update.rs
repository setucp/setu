//! Update state machine (spec §6, strategy `replace`), named steps
//! resumed from the job's cursor:
//!
//! `validate` (V exists for the app and is `binary`), `env` (merge the
//! stored env against V's manifest: `generate`/`prompt` values kept by
//! name, new rules added, removed ones dropped, `once` never re-emitted;
//! the old env and the old `SiteSpec` go SEALED into the job payload and
//! the merged env into `sites.env`), `backup` (`pre_update`), `install`
//! V (a bad checksum or hostile archive fails here, before `stop`: the
//! running site is untouched), `stop`, `switch_version` V, `write_unit`,
//! `write_env` (no `once`), `start`, `health`, `finish`.
//!
//! Failure before `stop`: the site stays `running` on the old version
//! and its stored env is put back. Failure at or after `stop`: the
//! `rollback` step — `stop`, `switch_version` old, `write_unit` old,
//! `write_env` old (no `once`), `restore` from the `pre_update` backup
//! (database replaced, dirs swapped: the new version may have migrated;
//! only when the new version was started, ledger R93), `start`,
//! `health` — leaves the site `running` on the old version and the job
//! `failed` with an error starting `rollback:`. A rollback that fails
//! leaves the site `failed` (`failed_step = rollback`).

use std::collections::BTreeMap;

use setu_common::{AppError, ErrorCode};
use setu_db::Site;
use setu_proto::{Command, SiteSpec};

use crate::{
    binary_ref, build_spec, fully_sealed_env, load_manifest, open_env, register_secrets,
    resolve_error, resolve_input, sealed_env, server_target, site_env, stored_db_url, without_once,
    JobCtx, Steps,
};

/// The steps, in order (spec §6); `rollback` is the cursor of a rollback.
pub const STEPS: &[&str] = &[
    "validate",
    "env",
    "backup",
    "install",
    "stop",
    "switch_version",
    "write_unit",
    "write_env",
    "start",
    "health",
    "finish",
];

/// Steps whose failure rolls back (the old site was stopped); from
/// `start` on the new version ran, so the rollback restores the backup.
const ROLLBACK_FROM: &str = "stop";
const RESTORE_FROM: &str = "start";

/// What a rollback needs, kept sealed in the job payload.
struct Old {
    version: String,
    env: Vec<setu_manifest::EnvVar>,
    spec: SiteSpec,
    /// The `pre_update` backup, once taken.
    backup_id: Option<String>,
}

/// Update the site to `to_version`.
#[allow(clippy::too_many_lines)] // reviewed: kept as-is (R150)
pub async fn run(ctx: &JobCtx, site_id: &str, to_version: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    let cursor = ctx.cursor().await;
    if cursor.as_deref() == Some("rollback") {
        // Interrupted mid-rollback: run it again (every step idempotent).
        let failed = ctx
            .payload_str("failed_step")
            .await
            .unwrap_or_else(|| "health".to_string());
        let old = load_old(ctx).await?;
        return Err(rollback(ctx, &site, &old, &failed, "resumed after interruption").await);
    }
    if cursor.is_none() && site.status != "running" {
        return Err(AppError::validation(format!(
            "site {site_id} is not running (status: {})",
            site.status
        )));
    }
    let steps = Steps::run_from(cursor.as_deref(), STEPS);

    let new_manifest = match steps
        .run(ctx, "validate", load_target(ctx, &site, to_version))
        .await
    {
        Ok(Some(m)) => m,
        Ok(None) => load_target(ctx, &site, to_version).await?,
        Err(e) => return Err(e),
    };
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    let target = server_target(&ctx.db, &site.server_id).await?;
    let new_spec = build_spec(&plan, &site, &new_manifest, target)?;
    let binary = binary_ref(&new_manifest, target)?;

    // `env`: merge and park the old state sealed in the payload. On
    // resume the old state comes back from the payload and the merged env
    // from `sites.env`.
    let mut old = match steps
        .run(ctx, "env", env_step(ctx, &site, &new_manifest))
        .await
    {
        Ok(Some(old)) => old,
        Ok(None) => load_old(ctx).await?,
        Err(e) => return Err(abort(ctx, &site, None, e).await),
    };
    let site = ctx.db.sites().get(site_id).await?;
    let merged = without_once(&site_env(ctx, &site)?);
    let server = site.server_id.as_str();
    let short = site.short_id.clone();

    match steps
        .run(
            ctx,
            "backup",
            crate::backup::take(ctx, &site, "pre_update", "pre_backup_id"),
        )
        .await
    {
        Ok(Some(id)) => old.backup_id = Some(id),
        Ok(None) => {}
        Err(e) => return Err(abort(ctx, &site, Some(&old), e).await),
    }
    if let Err(e) = steps
        .run(
            ctx,
            "install",
            ctx.agent_ok(
                server,
                Command::Install {
                    site: short.clone(),
                    binary,
                },
            ),
        )
        .await
    {
        return Err(abort(ctx, &site, Some(&old), e).await);
    }

    let after_stop: [(&'static str, Command); 6] = [
        (
            "stop",
            Command::Stop {
                site: short.clone(),
            },
        ),
        (
            "switch_version",
            Command::SwitchVersion {
                site: short.clone(),
                version: to_version.to_string(),
            },
        ),
        (
            "write_unit",
            Command::WriteUnit {
                site: new_spec.clone(),
            },
        ),
        (
            "write_env",
            Command::WriteEnv {
                site: short.clone(),
                env: merged,
            },
        ),
        (
            "start",
            Command::Start {
                site: short.clone(),
            },
        ),
        ("health", Command::Health { site: new_spec }),
    ];
    debug_assert_eq!(after_stop[0].0, ROLLBACK_FROM);
    for (name, cmd) in after_stop {
        if let Err(e) = steps.run(ctx, name, ctx.agent_ok(server, cmd)).await {
            return Err(rollback(ctx, &site, &old, name, &e.message).await);
        }
    }

    steps
        .run(ctx, "finish", async {
            ctx.db.sites().set_version(&site.id, to_version).await?;
            ctx.db.sites().set_status(&site.id, "running", None).await?;
            Ok(())
        })
        .await?;
    Ok(())
}

/// `validate`: V exists for the app and is a binary manifest.
async fn load_target(
    ctx: &JobCtx,
    site: &Site,
    to_version: &str,
) -> Result<setu_manifest::Manifest, AppError> {
    let manifest = load_manifest(&ctx.db, &site.app_id, to_version).await?;
    let target = server_target(&ctx.db, &site.server_id).await?;
    binary_ref(&manifest, target)?;
    Ok(manifest)
}

/// `env`: merge the stored env against V; store the old env and the old
/// spec sealed in the payload, the merged env in `sites.env` (with the
/// sealed `once` values kept for reveal), and mark the site `updating`.
async fn env_step(
    ctx: &JobCtx,
    site: &Site,
    new_manifest: &setu_manifest::Manifest,
) -> Result<Old, AppError> {
    let old_manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    let target = server_target(&ctx.db, &site.server_id).await?;
    let old_spec = build_spec(&plan, site, &old_manifest, target)?;
    let existing = site_env(ctx, site)?;
    // The role password lives in Postgres since provision: a
    // `postgres_url` is re-resolved from the stored URL, never a fresh
    // password (that would desynchronise the file from the role).
    let db_url = stored_db_url(&old_manifest, &existing)
        .unwrap_or_default()
        .to_string();
    // Answers for prompts V adds, sealed in the payload by `POST
    // /sites/{id}/update`; existing answers are kept by name.
    let prompts = open_prompts(ctx).await?;
    let mut input = resolve_input(
        &site.short_id,
        u16::try_from(site.port).map_err(|_| AppError::internal())?,
        &site.domain,
        &site.owner_email,
        db_url,
        &prompts,
    );
    let mut merged =
        setu_manifest::merge(&existing, new_manifest, &mut input).map_err(resolve_error)?;
    // Extra variables (0.1.4 §1) ride along; a name the new version
    // declares is its rule's from now on.
    merged.extend(
        existing
            .iter()
            .filter(|v| {
                !old_manifest.env.contains_key(&v.name)
                    && !new_manifest.env.contains_key(&v.name)
                    && !merged.iter().any(|m| m.name == v.name)
            })
            .cloned()
            .collect::<Vec<_>>(),
    );
    register_secrets(ctx, &merged);
    let mut stored = merged.clone();
    stored.extend(
        existing
            .iter()
            .filter(|v| v.once && !merged.iter().any(|m| m.name == v.name))
            .cloned(),
    );
    let spec_json = serde_json::to_string(&old_spec).map_err(|_| AppError::internal())?;
    ctx.payload_set("old_version", serde_json::json!(site.app_version))
        .await?;
    ctx.payload_set("old_env", fully_sealed_env(&ctx.sealer, &existing))
        .await?;
    ctx.payload_set("old_spec", serde_json::json!(ctx.sealer.seal(&spec_json)))
        .await?;
    ctx.db
        .sites()
        .set_status(&site.id, "updating", None)
        .await?;
    ctx.db
        .sites()
        .set_env(&site.id, &sealed_env(&ctx.sealer, &stored))
        .await?;
    // The answers now live (sealed) in `sites.env` only.
    if !prompts.is_empty() {
        ctx.payload_set("prompts", serde_json::Value::Null).await?;
    }
    Ok(Old {
        version: site.app_version.clone(),
        env: existing,
        spec: old_spec,
        backup_id: None,
    })
}

/// `payload.prompts`: name → `v1:` sealed answer (absent: none). Every
/// answer is registered for log redaction.
async fn open_prompts(ctx: &JobCtx) -> Result<BTreeMap<String, String>, AppError> {
    let Some(map) = ctx
        .payload_get("prompts")
        .await
        .and_then(|v| v.as_object().cloned())
    else {
        return Ok(BTreeMap::new());
    };
    let mut out = BTreeMap::new();
    for (name, sealed) in map {
        let plain = sealed
            .as_str()
            .filter(|s| s.starts_with("v1:"))
            .and_then(|s| ctx.sealer.open(s).ok())
            .ok_or_else(|| {
                tracing::error!(name = %name, "update prompt answer is not sealed");
                AppError::internal()
            })?;
        ctx.add_secret(&plain);
        out.insert(name, plain);
    }
    Ok(out)
}

/// The old state from the payload (resume).
async fn load_old(ctx: &JobCtx) -> Result<Old, AppError> {
    let corrupt = || {
        tracing::error!("update payload lacks the sealed old state");
        AppError::internal()
    };
    let version = ctx.payload_str("old_version").await.ok_or_else(corrupt)?;
    let env = open_env(
        &ctx.payload_get("old_env").await.ok_or_else(corrupt)?,
        &ctx.sealer,
    )?;
    register_secrets(ctx, &env);
    let sealed_spec = ctx.payload_str("old_spec").await.ok_or_else(corrupt)?;
    let spec_json = ctx.sealer.open(&sealed_spec).map_err(|_| corrupt())?;
    let spec = serde_json::from_str(&spec_json).map_err(|_| corrupt())?;
    Ok(Old {
        version,
        env,
        spec,
        backup_id: ctx.payload_str("pre_backup_id").await,
    })
}

/// Failure before `stop`: the old site never stopped. Put its env back
/// (when `env` already stored the merged one) and leave it `running`.
async fn abort(ctx: &JobCtx, site: &Site, old: Option<&Old>, err: AppError) -> AppError {
    if let Some(old) = old {
        let _ = ctx
            .db
            .sites()
            .set_env(&site.id, &sealed_env(&ctx.sealer, &old.env))
            .await;
    }
    let _ = ctx.db.sites().set_status(&site.id, "running", None).await;
    err
}

/// Roll back to the old version after `failed_step` failed (see module
/// docs). Returns the job's error: `rollback: …` when the site is
/// `running` on the old version again, `rollback failed: …` when not.
async fn rollback(
    ctx: &JobCtx,
    site: &Site,
    old: &Old,
    failed_step: &str,
    cause: &str,
) -> AppError {
    let _ = ctx
        .payload_set("failed_step", serde_json::json!(failed_step))
        .await;
    let restore = STEPS
        .iter()
        .position(|s| *s == failed_step)
        .zip(STEPS.iter().position(|s| *s == RESTORE_FROM))
        .is_some_and(|(failed, from)| failed >= from);
    let done = ctx
        .step("rollback", rollback_steps(ctx, site, old, restore))
        .await;
    match done {
        Ok(()) => {
            let _ = ctx
                .db
                .sites()
                .set_env(&site.id, &sealed_env(&ctx.sealer, &old.env))
                .await;
            let _ = ctx.db.sites().set_status(&site.id, "running", None).await;
            AppError::new(
                ErrorCode::Internal,
                format!(
                    "rollback: {failed_step} failed ({cause}); running {} again",
                    old.version
                ),
            )
        }
        Err(e) => {
            let _ = ctx
                .db
                .sites()
                .set_status(&site.id, "failed", Some("rollback"))
                .await;
            AppError::new(
                ErrorCode::Internal,
                format!(
                    "rollback failed: {} (after {failed_step} failed: {cause})",
                    e.message
                ),
            )
        }
    }
}

async fn rollback_steps(
    ctx: &JobCtx,
    site: &Site,
    old: &Old,
    restore: bool,
) -> Result<(), AppError> {
    let server = site.server_id.as_str();
    let short = site.short_id.clone();
    ctx.agent_ok(
        server,
        Command::Stop {
            site: short.clone(),
        },
    )
    .await?;
    ctx.agent_ok(
        server,
        Command::SwitchVersion {
            site: short.clone(),
            version: old.version.clone(),
        },
    )
    .await?;
    ctx.agent_ok(
        server,
        Command::WriteUnit {
            site: old.spec.clone(),
        },
    )
    .await?;
    ctx.agent_ok(
        server,
        Command::WriteEnv {
            site: short.clone(),
            env: without_once(&old.env),
        },
    )
    .await?;
    if restore {
        let backup_id = old
            .backup_id
            .clone()
            .ok_or_else(|| AppError::new(ErrorCode::Internal, "no pre_update backup to restore"))?;
        let old_manifest = load_manifest(&ctx.db, &site.app_id, &old.version).await?;
        ctx.agent_ok(
            server,
            Command::Restore {
                site: short.clone(),
                db: site.db_name.clone(),
                role: site.db_role.clone(),
                dirs: old_manifest.backup.dirs.clone(),
                backup_id,
            },
        )
        .await?;
    }
    ctx.agent_ok(
        server,
        Command::Start {
            site: short.clone(),
        },
    )
    .await?;
    ctx.agent_ok(
        server,
        Command::Health {
            site: old.spec.clone(),
        },
    )
    .await?;
    Ok(())
}
