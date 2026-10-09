//! Job runner: claims jobs (`FOR UPDATE SKIP LOCKED`), dispatches by
//! kind, finishes ok/error. Re-queues stale agent commands every 30 s.

use std::sync::Arc;
use std::time::Duration;

use setu_common::{AppError, Sealer};
use setu_db::{Db, Job};
use setu_proto::Command;

use crate::{agent_step, fail, log_step, AgentClient, JobCtx};

/// Background job runner.
pub struct Runner;

impl Runner {
    /// Spawn the runner: `concurrency` worker loops plus a 30 s stale
    /// command re-queue. The handle runs until aborted (process exit).
    pub fn spawn(
        db: Db,
        agent: Arc<dyn AgentClient>,
        sealer: Sealer,
        concurrency: usize,
    ) -> tokio::task::JoinHandle<()> {
        let concurrency = concurrency.max(1);
        tokio::spawn(async move {
            let mut workers = tokio::task::JoinSet::new();
            for idx in 0..concurrency {
                let db = db.clone();
                let agent = agent.clone();
                let sealer = sealer.clone();
                workers.spawn(async move { worker_loop(&db, &agent, &sealer, idx).await });
            }
            let _ = workers.join_all().await;
        })
    }
}

async fn worker_loop(db: &Db, agent: &Arc<dyn AgentClient>, sealer: &Sealer, idx: usize) {
    let worker = format!("runner-{idx}");
    // Re-queue on the first iteration so a restarted control plane
    // picks up commands stranded by the previous one.
    let mut last_requeue = std::time::Instant::now() - Duration::from_secs(3600);
    loop {
        if last_requeue.elapsed() > Duration::from_secs(30) {
            let _ = db
                .agent_commands()
                .requeue_stale(Duration::from_secs(120))
                .await;
            last_requeue = std::time::Instant::now();
        }
        match db.jobs().claim(&worker).await {
            Ok(Some(job)) => handle_job(db, agent, sealer, &job).await,
            Ok(None) => tokio::time::sleep(Duration::from_secs(1)).await,
            Err(e) => {
                tracing::error!(error = %e, "job claim failed");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

async fn handle_job(db: &Db, agent: &Arc<dyn AgentClient>, sealer: &Sealer, job: &Job) {
    let job_id = job.id.clone();
    let ctx = JobCtx {
        db: db.clone(),
        agent: agent.clone(),
        sealer: sealer.clone(),
        log: Box::new(move |line: &str| {
            tracing::info!(job_id = %job_id, "{line}");
        }),
        job_id: Some(job.id.clone()),
        timeout: Duration::from_secs(120),
    };

    let result: Result<(), AppError> = match job.kind.as_str() {
        "provision_site" => match job.site_id.as_deref() {
            Some(site_id) => crate::provision::run(&ctx, site_id).await,
            None => Err(AppError::validation("provision_site job has no site_id")),
        },
        "update_site" => {
            let to_version = job
                .payload
                .as_ref()
                .and_then(|p| p.get("to_version").or_else(|| p.get("version")))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            match (job.site_id.as_deref(), to_version) {
                (Some(site_id), Some(version)) => crate::update::run(&ctx, site_id, &version).await,
                _ => Err(AppError::validation(
                    "update_site job needs site_id and payload.to_version",
                )),
            }
        }
        "backup_site" => match job.site_id.as_deref() {
            Some(site_id) => crate::backup::run(&ctx, site_id).await,
            None => Err(AppError::validation("backup_site job has no site_id")),
        },
        "restore_site" => {
            let backup_id = job
                .payload
                .as_ref()
                .and_then(|p| p.get("backup_id"))
                .and_then(|v| v.as_str())
                .map(str::to_string);
            match (job.site_id.as_deref(), backup_id) {
                (Some(site_id), Some(backup_id)) => {
                    crate::restore::run(&ctx, site_id, &backup_id).await
                }
                _ => Err(AppError::validation(
                    "restore_site job needs site_id and payload.backup_id",
                )),
            }
        }
        "delete_site" => match job.site_id.as_deref() {
            Some(site_id) => crate::delete::run(&ctx, site_id).await,
            None => Err(AppError::validation("delete_site job has no site_id")),
        },
        "restart_site" => match job.site_id.as_deref() {
            Some(site_id) => restart(&ctx, site_id).await,
            None => Err(AppError::validation("restart_site job has no site_id")),
        },
        "stop_site" => match job.site_id.as_deref() {
            Some(site_id) => stop_site(&ctx, site_id).await,
            None => Err(AppError::validation("stop_site job has no site_id")),
        },
        "start_site" => match job.site_id.as_deref() {
            Some(site_id) => start_site(&ctx, site_id).await,
            None => Err(AppError::validation("start_site job has no site_id")),
        },
        "reconcile" => {
            match db
                .agent_commands()
                .requeue_stale(Duration::from_secs(120))
                .await
            {
                Ok(_) => {
                    log_step(&ctx, "reconcile", true, None).await;
                    Ok(())
                }
                Err(e) => {
                    log_step(&ctx, "reconcile", false, Some(&e.message.clone())).await;
                    Err(e)
                }
            }
        }
        other => Err(AppError::validation(format!("unknown job kind {other}"))),
    };
    match result {
        Ok(()) => {
            let _ = db.jobs().finish(&job.id, true, None).await;
        }
        Err(e) => {
            let _ = db.jobs().finish(&job.id, false, Some(&e.message)).await;
        }
    }
}

/// `restart_site` (no payload): stop then start, ending `running`.
async fn restart(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot restart from status {}",
            site.status
        )));
    }
    let _ = ctx.db.sites().set_status(site_id, "updating", None).await;
    agent_step(
        ctx,
        site_id,
        "stop",
        &site.server_id,
        Command::Stop {
            site_id: site.id.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "start",
        &site.server_id,
        Command::Start {
            site_id: site.id.clone(),
        },
    )
    .await?;
    // `agent_step` already marked `failed` on error; only the success
    // path reaches here.
    match ctx.db.sites().set_status(site_id, "running", None).await {
        Ok(_) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}

/// `stop_site` (no payload): stop the unit, ending `stopped`. Stopping an
/// already-`stopped` site is an idempotent success (the agent call is
/// still issued: the unit may have been started out of band).
async fn stop_site(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "running" && site.status != "stopped" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot stop from status {}",
            site.status
        )));
    }
    agent_step(
        ctx,
        site_id,
        "stop",
        &site.server_id,
        Command::Stop {
            site_id: site.id.clone(),
        },
    )
    .await?;
    match ctx.db.sites().set_status(site_id, "stopped", None).await {
        Ok(_) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}

/// `start_site` (no payload): start the unit, ending `running`. Starting
/// an already-`running` site is an idempotent success.
async fn start_site(ctx: &JobCtx, site_id: &str) -> Result<(), AppError> {
    let site = ctx.db.sites().get(site_id).await?;
    if site.status != "stopped" && site.status != "running" {
        return Err(AppError::validation(format!(
            "site {site_id} cannot start from status {}",
            site.status
        )));
    }
    agent_step(
        ctx,
        site_id,
        "start",
        &site.server_id,
        Command::Start {
            site_id: site.id.clone(),
        },
    )
    .await?;
    match ctx.db.sites().set_status(site_id, "running", None).await {
        Ok(_) => {
            log_step(ctx, "finish", true, None).await;
            Ok(())
        }
        Err(e) => Err(fail(ctx, site_id, "finish", e).await),
    }
}
