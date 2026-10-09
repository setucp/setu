//! Update state machine (spec §5, strategy `replace`).
//!
//! Named steps: `validate`, `backup` (pre-update), `install`, `stop`,
//! `switch_version`, `start`, `health`, `finish` (plus `run_command`
//! instead of install→start for strategy `command`).
//!
//! - The target manifest is loaded and image-rejected BEFORE anything:
//!   no status change, no backup row, no agent call.
//! - `install` runs before `stop`: a checksum mismatch fails with the
//!   old site still `running` (Review-Focus #5, no downtime).
//! - Any failure at or after `stop` rolls back: `SwitchVersion(old)` +
//!   `Start`, site back to `running` on the old version, error contains
//!   `rollback`.
//! - Strategy `command` runs the manifest's command as a `Restart`
//!   followed by `Health` (Ruling: proto has no generic exec command;
//!   the agent-side detail is deferred to the Task 10 handler). No
//!   rollback beyond what the app does itself: failures there mark the
//!   site `failed`.

use std::path::PathBuf;

use setu_common::{AppError, ErrorCode};
use setu_db::Site;
use setu_manifest::{Manifest, Update};
use setu_proto::Command;

use crate::{
    build_site, call_agent, fail, job_prompts, load_manifest, log_step, merge_update_env,
    require_ok, sealed_env, stored_plaintext, to_proto_envs, JobCtx,
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
    if matches!(new_manifest.run, setu_manifest::Run::Image { .. }) {
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
    let old_built = match build_site(&plan, &site, &old_manifest, &prompts) {
        Ok(b) => b,
        Err(e) => return Err(fail(ctx, site_id, "validate", e).await),
    };
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
    let merged = merge_update_env(&new_manifest, &new_built.env_manifest, &current);
    let persistent: Vec<setu_manifest::EnvVar> =
        merged.iter().filter(|v| !v.once).cloned().collect();
    let has_once = persistent.len() != merged.len();
    let current_proto = to_proto_envs(
        &current
            .iter()
            .map(|(name, (value, secret, once))| setu_manifest::EnvVar {
                name: name.clone(),
                value: value.clone(),
                secret: *secret,
                once: *once,
            })
            .collect::<Vec<_>>(),
    );

    log_step(ctx, "validate", true, None).await;
    ctx.db.sites().set_status(site_id, "updating", None).await?;

    if let Err(e) = pre_update_backup(ctx, &site, &old_manifest, &old_built.spec).await {
        log_step(ctx, "backup", false, Some(&e.message.clone())).await;
        let _ = ctx.db.sites().set_status(site_id, "running", None).await;
        return Err(e);
    }
    log_step(ctx, "backup", true, None).await;

    if let Update::Command { command: _ } = &new_manifest.update {
        return run_command_strategy(ctx, &site, &server_id, to_version).await;
    }

    // Install beside the old version BEFORE stopping: checksum (or any
    // install) failure leaves the old site untouched and `running`.
    if let Err(e) = update_call(
        ctx,
        "install",
        &server_id,
        Command::Install {
            site: new_built.spec.clone(),
        },
    )
    .await
    {
        return abort_running(ctx, site_id, "install", e).await;
    }
    log_step(ctx, "install", true, None).await;

    // Write the merged env BEFORE stopping: the old process keeps its
    // loaded env and is unaffected by the file swap. On failure the
    // previous file is restored best-effort and the old site stays up.
    if let Err(e) = write_env_file(ctx, &server_id, &new_built.spec, &to_proto_envs(&merged)).await
    {
        let _ = write_env_file(ctx, &server_id, &old_built.spec, &current_proto).await;
        return abort_running(ctx, site_id, "write_env", e).await;
    }
    log_step(ctx, "write_env", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "stop",
        &server_id,
        Command::Stop {
            site_id: site.id.clone(),
        },
    )
    .await
    {
        return Err(rollback(
            ctx,
            &site,
            &old_version,
            "stop",
            e,
            &old_built.spec,
            &current_proto,
        )
        .await);
    }
    log_step(ctx, "stop", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "switch_version",
        &server_id,
        Command::SwitchVersion {
            site_id: site.id.clone(),
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
            &old_built.spec,
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
            site_id: site.id.clone(),
        },
    )
    .await
    {
        return Err(rollback(
            ctx,
            &site,
            &old_version,
            "start",
            e,
            &old_built.spec,
            &current_proto,
        )
        .await);
    }
    log_step(ctx, "start", true, None).await;

    if let Err(e) = update_call(
        ctx,
        "health",
        &server_id,
        Command::Health {
            site_id: site.id.clone(),
            port: site.port as u16,
            path: new_manifest.app.health.clone(),
            timeout_s: 90,
        },
    )
    .await
    {
        return Err(rollback(
            ctx,
            &site,
            &old_version,
            "health",
            e,
            &old_built.spec,
            &current_proto,
        )
        .await);
    }
    log_step(ctx, "health", true, None).await;

    // Mirror provision: `once` secrets leave the env file after the new
    // version's first healthy boot (file cleanup only — the running
    // process already loaded them, so no restart). A cleanup failure is
    // recorded but does not fail the update.
    if has_once {
        match write_env_file(
            ctx,
            &server_id,
            &new_built.spec,
            &to_proto_envs(&persistent),
        )
        .await
        {
            Ok(()) => log_step(ctx, "write_env_cleanup", true, None).await,
            Err(e) => log_step(ctx, "write_env_cleanup", false, Some(&e.message)).await,
        }
    }

    let sealed = sealed_env(&ctx.sealer, &merged);
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

/// Strategy `command`: the app updates itself; we restart (which is how
/// the command takes effect in phase 1) and health-check. Failures mark
/// the site `failed` — there is nothing to roll back to.
async fn run_command_strategy(
    ctx: &JobCtx,
    site: &Site,
    server_id: &str,
    to_version: &str,
) -> Result<(), AppError> {
    if let Err(e) = update_call(
        ctx,
        "run_command",
        server_id,
        Command::Restart {
            site_id: site.id.clone(),
        },
    )
    .await
    {
        let msg = e.message.clone();
        log_step(ctx, "run_command", false, Some(&msg)).await;
        let _ = ctx
            .db
            .sites()
            .set_status(&site.id, "failed", Some("run_command"))
            .await;
        return Err(e);
    }
    log_step(ctx, "run_command", true, None).await;

    let manifest = load_manifest(&ctx.db, &site.app_id, to_version).await?;
    if let Err(e) = update_call(
        ctx,
        "health",
        server_id,
        Command::Health {
            site_id: site.id.clone(),
            port: site.port as u16,
            path: manifest.app.health.clone(),
            timeout_s: 90,
        },
    )
    .await
    {
        let msg = e.message.clone();
        log_step(ctx, "health", false, Some(&msg)).await;
        let _ = ctx
            .db
            .sites()
            .set_status(&site.id, "failed", Some("health"))
            .await;
        return Err(e);
    }
    log_step(ctx, "health", true, None).await;

    ctx.db.sites().set_version(&site.id, to_version).await?;
    match ctx.db.sites().set_status(&site.id, "running", None).await {
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
    spec: &setu_proto::SiteSpec,
    env: &[setu_proto::EnvVar],
) -> Result<(), AppError> {
    match call_agent(
        ctx,
        server_id,
        Command::WriteEnv {
            site: spec.clone(),
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
    old_spec: &setu_proto::SiteSpec,
    old_env: &[setu_proto::EnvVar],
) -> AppError {
    let cause_msg = cause.message.clone();
    log_step(ctx, step, false, Some(&cause_msg)).await;
    if write_env_file(ctx, &site.server_id, old_spec, old_env)
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
                site_id: site.id.clone(),
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
                    site_id: site.id.clone(),
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
async fn pre_update_backup(
    ctx: &JobCtx,
    site: &Site,
    manifest: &Manifest,
    spec: &setu_proto::SiteSpec,
) -> Result<(), AppError> {
    let dest = backup_dest(&site.id, "pre-update");
    let row = ctx
        .db
        .backups()
        .create(&site.id, "pre_update", &dest)
        .await?;
    match call_agent(
        ctx,
        &site.server_id,
        Command::Backup {
            site: spec.clone(),
            db: site.db_name.clone(),
            dirs: manifest.backup.dirs.clone(),
            dest: PathBuf::from(&dest),
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

pub(crate) fn backup_dest(site_id: &str, kind: &str) -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("/var/lib/setu/backups/{site_id}/{kind}-{secs}")
}
