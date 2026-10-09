//! Update state machine (spec §5, strategy `replace`).
//!
//! Named steps: `validate`, `backup` (pre-update), `install`,
//! `write_env`, `stop`, `switch_version`, `start`, `health`, `finish`.
//!
//! - The target manifest is loaded and image-rejected BEFORE anything:
//!   no status change, no backup row, no agent call.
//! - `install` runs before `stop`: a checksum mismatch fails with the
//!   old site still `running` (Review-Focus #5, no downtime).
//! - Any failure at or after `stop` rolls back: `SwitchVersion(old)` +
//!   `Start`, site back to `running` on the old version, error contains
//!   `rollback`.
//! - The env is `setu_manifest::merge(stored, new manifest)`: generated
//!   and prompt values kept, `value`/`from` re-resolved, `once` values
//!   never re-emitted (they stay sealed in storage for reveal).

use setu_common::{AppError, ErrorCode};
use setu_db::Site;
use setu_manifest::{EnvSource, EnvSpec, Manifest};
use setu_proto::Command;

use crate::{
    build_site, call_agent, call_install, fail, job_prompts, load_manifest, log_step, require_ok,
    resolve_error, resolve_input, sealed_env, stored_plaintext, to_proto_envs, JobCtx,
};

/// Update the site to `to_version`.
pub async fn run(ctx: &JobCtx, site_id: &str, to_version: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" {
        return Err(AppError::validation(format!(
            "site {site_id} is not running (status: {})",
            site.status
        )));
    }
    let new_manifest = match load_manifest(&ctx.db, &site.app_id, to_version).await {
        Ok(m) => m,
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
    if new_manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let old_manifest = match load_manifest(&ctx.db, &site.app_id, &site.app_version).await {
        Ok(m) => m,
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
    let plan = match ctx.db.plans().get(&site.plan_id).await {
        Ok(p) => p,
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
    // Specs for both versions are built before anything mutates: build
    // failures (missing target, unresolvable env) abort pre-status.
    // Prompt answers ride on the job payload (the API recovers them from
    // the site's provision job); direct `run` callers resolve without.
    let prompts = job_prompts(ctx).await;
    // The old version must still be buildable: rollback returns to it.
    if let Err(e) = crate::build_spec(&plan, &site, &old_manifest) {
        return Err(fail(ctx, site_id, "validate", e).await);
    }
    let new_built = match build_site(&plan, &site, &new_manifest, &prompts) {
        Ok(b) => b,
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
    let server_id = site.server_id.clone();
    let old_version = site.app_version.clone();

    // The file on disk holds the CURRENT values (provision wrote them,
    // minus `once` secrets deleted after first health; the sealed DB copy
    // retains everything). The merged env refreshes author `value` and
    // derived `from` entries while keeping stored `generate`/`prompt`
    // secrets — an update must never rotate them.
    let current = match stored_plaintext(&site, &ctx.sealer) {
        Ok(m) => m,
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
    let existing: Vec<setu_manifest::EnvVar> = current
        .iter()
        .map(|(name, (value, secret, once))| setu_manifest::EnvVar {
            name: name.clone(),
            value: value.clone(),
            secret: *secret,
            once: *once,
        })
        .collect();
    // The role password was minted once at provision and lives in
    // Postgres: re-resolve `postgres_url` from the stored URL, never a
    // fresh password (that would desynchronise the file from the role).
    let db_url = new_manifest
        .env
        .iter()
        .filter(|(_, spec)| matches!(spec, EnvSpec::From(f) if f.from == EnvSource::PostgresUrl))
        .find_map(|(name, _)| current.get(name).map(|(v, _, _)| v.clone()))
        .or_else(|| {
            new_built
                .env_manifest
                .iter()
                .find(|v| {
                    matches!(new_manifest.env.get(&v.name),
                        Some(EnvSpec::From(f)) if f.from == EnvSource::PostgresUrl)
                })
                .map(|v| v.value.clone())
        })
        .unwrap_or_default();
    let mut input = resolve_input(&site, db_url, &prompts);
    let merged = match setu_manifest::merge(&existing, &new_manifest, &mut input) {
        Ok(m) => m,
        Err(e) => return Err(fail(ctx, site_id, "validate", resolve_error(e)).await),
    };
    // Storage keeps the sealed `once` values the merge never re-emits.
    let mut stored_after = merged.clone();
    stored_after.extend(
        existing
            .iter()
            .filter(|v| v.once && !merged.iter().any(|m| m.name == v.name))
            .cloned(),
    );
    // Rollback restores the previous file, without `once` values.
    let current_proto = to_proto_envs(
        &existing
            .iter()
            .filter(|v| !v.once)
            .cloned()
            .collect::<Vec<_>>(),
    );

    log_step(ctx, "validate", true, None).await;
    ctx.db.sites().set_status(site_id, "updating", None).await?;

    if let Err(e) = pre_update_backup(ctx, &site, &old_manifest).await {
        log_step(ctx, "backup", false, Some(&e.message.clone())).await;
        let _ = ctx.db.sites().set_status(site_id, "running", None).await;
        return Err(e);
    }
    log_step(ctx, "backup", true, None).await;

    // Install beside the old version BEFORE stopping: checksum (or any
    // install) failure leaves the old site untouched and `running`.
    let installed = match call_install(
        ctx,
        &server_id,
        Command::Install {
            site: site.short_id.clone(),
            binary: new_built.binary.clone(),
        },
    )
    .await
    {
        Ok(result) => require_ok(result)
            .map(|_| ())
            .map_err(|msg| AppError::new(ErrorCode::Internal, msg)),
        Err(e) => Err(AppError::from(e)),
    };
    if let Err(e) = installed {
        return abort_running(ctx, site_id, "install", e).await;
    }
    log_step(ctx, "install", true, None).await;

    // Write the merged env BEFORE stopping: the old process keeps its
    // loaded env and is unaffected by the file swap. On failure the
    // previous file is restored best-effort and the old site stays up.
    if let Err(e) = write_env_file(ctx, &server_id, &site.short_id, &to_proto_envs(&merged)).await {
        let _ = write_env_file(ctx, &server_id, &site.short_id, &current_proto).await;
        return abort_running(ctx, site_id, "write_env", e).await;
    }
    log_step(ctx, "write_env", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "stop",
        &server_id,
        Command::Stop {
            site: site.short_id.clone(),
        },
    )
    .await
    {
        return Err(rollback(ctx, &site, &old_version, "stop", e, &current_proto).await);
    }
    log_step(ctx, "stop", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "switch_version",
        &server_id,
        Command::SwitchVersion {
            site: site.short_id.clone(),
            version: to_version.to_string(),
        },
    )
    .await
    {
        return Err(rollback(
            ctx,
            &site,
            &old_version,
            "switch_version",
            e,
            &current_proto,
        )
        .await);
    }
    log_step(ctx, "switch_version", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "start",
        &server_id,
        Command::Start {
            site: site.short_id.clone(),
        },
    )
    .await
    {
        return Err(rollback(ctx, &site, &old_version, "start", e, &current_proto).await);
    }
    log_step(ctx, "start", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "health",
        &server_id,
        Command::Health {
            site: new_built.spec.clone(),
        },
    )
    .await
    {
        return Err(rollback(ctx, &site, &old_version, "health", e, &current_proto).await);
    }
    log_step(ctx, "health", true, None).await;

    let sealed = sealed_env(&ctx.sealer, &stored_after);
    if let Err(e) = ctx.db.sites().set_env(site_id, &sealed).await {
        return Err(fail(ctx, site_id, "finish", e).await);
    }
    ctx.db.sites().set_version(site_id, to_version).await?;
    match ctx.db.sites().set_status(site_id, "running", None).await {
        Ok(_) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => {
            log_step(ctx, "finish", false, Some(&e.message.clone())).await;
            Err(e)
        }
    }
}

/// One agent `WriteEnv`: returns the error without touching site
/// status; the caller picks the recovery (abort vs rollback).
async fn write_env_file(
    ctx: &JobCtx,
    server_id: &str,
    short: &str,
    env: &[setu_proto::EnvVar],
) -> Result<(), AppError> {
    match call_agent(
        ctx,
        server_id,
        Command::WriteEnv {
            site: short.to_string(),
            env: env.to_vec(),
        },
    )
    .await
    {
        Ok(result) => require_ok(result)
            .map(|_| ())
            .map_err(|msg| AppError::new(ErrorCode::Internal, msg)),
        Err(e) => Err(AppError::from(e)),
    }
}

/// One agent call for the update flow: returns the error without touching
/// site status; the caller picks the recovery (abort vs rollback).
async fn update_call(
    ctx: &JobCtx,
    _step: &str,
    server_id: &str,
    cmd: Command,
) -> Result<(), AppError> {
    match call_agent(ctx, server_id, cmd).await {
        Ok(result) => require_ok(result)
            .map(|_| ())
            .map_err(|msg| AppError::new(ErrorCode::Internal, msg)),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Pre-stop failure: the old site is untouched, back to `running`.
async fn abort_running(
    ctx: &JobCtx,
    site_id: &str,
    step: &str,
    err: AppError,
) -> Result<(), AppError> {
    let msg = err.message.clone();
    log_step(ctx, step, false, Some(&msg)).await;
    let _ = ctx.db.sites().set_status(site_id, "running", None).await;
    Err(err)
}

/// Post-stop failure: restore the previous env file (the update may
/// have written new values, e.g. a poisoned default — the old binary
/// must boot with its old env), swap back, start, site `running` on the
/// old version. The error always contains `rollback`. If the switch-back
/// itself fails the site is `failed` (never claim running-on-old);
/// only a successful switch-back AND start restores `running`.
async fn rollback(
    ctx: &JobCtx,
    site: &Site,
    old_version: &str,
    step: &str,
    cause: AppError,
    old_env: &[setu_proto::EnvVar],
) -> AppError {
    let cause_msg = cause.message.clone();
    log_step(ctx, step, false, Some(&cause_msg)).await;
    if write_env_file(ctx, &site.server_id, &site.short_id, old_env)
        .await
        .is_err()
    {
        log_step(ctx, "rollback", false, Some("env restore failed")).await;
    }
    let switched = matches!(
        call_agent(
            ctx,
            &site.server_id,
            Command::SwitchVersion {
                site: site.short_id.clone(),
                version: old_version.to_string(),
            },
        )
        .await
        .map(require_ok),
        Ok(Ok(_))
    );
    let started = if switched {
        matches!(
            call_agent(
                ctx,
                &site.server_id,
                Command::Start {
                    site: site.short_id.clone(),
                },
            )
            .await
            .map(require_ok),
            Ok(Ok(_))
        )
    } else {
        false
    };
    if switched && started {
        let _ = ctx.db.sites().set_status(&site.id, "running", None).await;
        log_step(ctx, "rollback", true, None).await;
    } else {
        let _ = ctx
            .db
            .sites()
            .set_status(&site.id, "failed", Some(step))
            .await;
        if switched {
            log_step(ctx, "rollback", false, Some("start failed")).await;
        } else {
            log_step(ctx, "rollback", false, Some("switch-back failed")).await;
        }
    }
    AppError::new(
        ErrorCode::Internal,
        format!("rollback: {step} failed: {cause_msg}"),
    )
}

/// `pre_update` backup row plus the agent `Backup` call. Failure aborts
/// the update before anything mutates.
async fn pre_update_backup(ctx: &JobCtx, site: &Site, manifest: &Manifest) -> Result<(), AppError> {
    let row = ctx.db.backups().create(site, "pre_update").await?;
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
        Ok(result) if result.ok => {
            let size = result.output.get("size_bytes").and_then(|v| v.as_i64());
            let _ = ctx
                .db
                .backups()
                .set_status(&row.id, "completed", size)
                .await;
            Ok(())
        }
        Ok(result) => {
            let _ = ctx.db.backups().set_status(&row.id, "failed", None).await;
            Err(AppError::new(
                ErrorCode::Internal,
                result
                    .error
                    .unwrap_or_else(|| "agent command failed".to_string()),
            ))
        }
        Err(e) => {
            let _ = ctx.db.backups().set_status(&row.id, "failed", None).await;
            Err(AppError::from(e))
        }
    }
}
