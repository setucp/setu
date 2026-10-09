//! Job runner (spec §6 "Job runner"): claims jobs with `FOR UPDATE SKIP
//! LOCKED` and a lease, renews the lease while the job runs, reclaims
//! jobs whose lease expired (a dead runner), resets expired `running`
//! jobs on boot, and limits `install` steps per server.

use std::sync::Arc;
use std::time::Duration;

use setu_common::{AppError, Sealer};
use setu_db::{Db, Job};
use setu_proto::Command;

use crate::{agent_step, fail, log_step, AgentClient, Clock, InstallSlots, JobCtx};

/// Runner tuning; [`Default`] is spec §6.
#[derive(Debug, Clone)]
pub struct RunnerConfig {
    /// Worker loops (jobs at once).
    pub concurrency: usize,
    /// `locked_until = now() + lease` on claim and on every renewal.
    pub lease: Duration,
    /// Renewal cadence while a job runs (well inside `lease`).
    pub renew_every: Duration,
    /// Concurrent `install` steps per server.
    pub installs_per_server: usize,
    /// Sleep between empty claims.
    pub idle_poll: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            concurrency: 4,
            lease: Duration::from_secs(60),
            renew_every: Duration::from_secs(20),
            installs_per_server: crate::INSTALLS_PER_SERVER,
            idle_poll: Duration::from_secs(1),
        }
    }
}

/// Background job runner.
pub struct Runner;

impl Runner {
    /// Spawn the runner. Aborting the returned handle drops every worker
    /// and its renewal with it (the leases then run out and another
    /// runner reclaims the jobs).
    pub fn spawn(
        db: Db,
        agent: Arc<dyn AgentClient>,
        sealer: Sealer,
        clock: Arc<dyn Clock>,
        config: RunnerConfig,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            match db.jobs().reset_expired_running().await {
                Ok(0) => {}
                Ok(n) => tracing::info!(jobs = n, "requeued jobs whose lease expired"),
                Err(e) => tracing::error!(error = %e, "boot job reset failed"),
            }
            // Unique per spawn: renewals by one runner never extend a
            // lease another runner took over.
            let prefix = format!("runner-{}", setu_common::new_id());
            let installs = InstallSlots::new(config.installs_per_server);
            let mut workers = tokio::task::JoinSet::new();
            for idx in 0..config.concurrency.max(1) {
                let worker = Worker {
                    db: db.clone(),
                    agent: agent.clone(),
                    sealer: sealer.clone(),
                    clock: clock.clone(),
                    installs: installs.clone(),
                    config: config.clone(),
                    name: format!("{prefix}-{idx}"),
                };
                workers.spawn(async move { worker.run().await });
            }
            let _ = workers.join_all().await;
        })
    }
}

struct Worker {
    db: Db,
    agent: Arc<dyn AgentClient>,
    sealer: Sealer,
    clock: Arc<dyn Clock>,
    installs: InstallSlots,
    config: RunnerConfig,
    name: String,
}

impl Worker {
    async fn run(self) {
        loop {
            match self.db.jobs().claim(&self.name, self.config.lease).await {
                Ok(Some(job)) => self.run_leased(&job).await,
                Ok(None) => tokio::time::sleep(self.config.idle_poll).await,
                Err(e) => {
                    tracing::error!(error = %e, "job claim failed");
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }

    /// Run the job while renewing its lease; the renewal ends with the
    /// job (both live in one `select!`). A lost lease (another runner
    /// reclaimed the job) stops this run without finishing the job.
    async fn run_leased(&self, job: &Job) {
        let renew = async {
            let mut tick = tokio::time::interval(self.config.renew_every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await; // the claim just set the lease
            loop {
                tick.tick().await;
                match self
                    .db
                    .jobs()
                    .renew(&job.id, &self.name, self.config.lease)
                    .await
                {
                    Ok(true) => {}
                    Ok(false) => return,
                    Err(e) => tracing::warn!(error = %e, job_id = %job.id, "lease renewal failed"),
                }
            }
        };
        tokio::select! {
            result = self.dispatch(job) => {
                let finished = match result {
                    Ok(()) => self.db.jobs().finish(&job.id, true, None).await,
                    Err(e) => self.db.jobs().finish(&job.id, false, Some(&e.message)).await,
                };
                if let Err(e) = finished {
                    tracing::error!(error = %e, job_id = %job.id, "finishing job failed");
                }
            }
            () = renew => {
                tracing::warn!(job_id = %job.id, "job lease lost; another runner owns it");
            }
        }
    }

    async fn dispatch(&self, job: &Job) -> Result<(), AppError> {
        let job_id = job.id.clone();
        let ctx = JobCtx {
            db: self.db.clone(),
            agent: self.agent.clone(),
            sealer: self.sealer.clone(),
            clock: self.clock.clone(),
            installs: self.installs.clone(),
            log: Box::new(move |line: &str| {
                tracing::info!(job_id = %job_id, "{line}");
            }),
            job_id: Some(job.id.clone()),
        };
        let payload_str = |key: &str| {
            job.payload
                .get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let site = |kind: &str| {
            job.site_id
                .clone()
                .ok_or_else(|| AppError::validation(format!("{kind} job has no site_id")))
        };
        match job.kind.as_str() {
            "provision_site" => crate::provision::run(&ctx, &site("provision_site")?).await,
            "update_site" => {
                let to = payload_str("to_version")
                    .or_else(|| payload_str("version"))
                    .ok_or_else(|| {
                        AppError::validation("update_site job needs payload.to_version")
                    })?;
                crate::update::run(&ctx, &site("update_site")?, &to).await
            }
            "backup_site" => {
                let kind = payload_str("kind").unwrap_or_else(|| "manual".to_string());
                crate::backup::run(&ctx, &site("backup_site")?, &kind).await
            }
            "restore_site" => {
                let backup_id = payload_str("backup_id").ok_or_else(|| {
                    AppError::validation("restore_site job needs payload.backup_id")
                })?;
                crate::restore::run(&ctx, &site("restore_site")?, &backup_id).await
            }
            "delete_site" => crate::delete::run(&ctx, &site("delete_site")?).await,
            "delete_backup" => {
                let backup_id = payload_str("backup_id").ok_or_else(|| {
                    AppError::validation("delete_backup job needs payload.backup_id")
                })?;
                crate::delete_backup::run(&ctx, &backup_id).await
            }
            "env_update" => crate::env_update::run(&ctx, &site("env_update")?).await,
            "restart_site" => restart(&ctx, &site("restart_site")?).await,
            "stop_site" => stop_site(&ctx, &site("stop_site")?).await,
            "start_site" => start_site(&ctx, &site("start_site")?).await,
            other => Err(AppError::validation(format!("unknown job kind {other}"))),
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
            site: site.short_id.clone(),
        },
    )
    .await?;
    agent_step(
        ctx,
        site_id,
        "start",
        &site.server_id,
        Command::Start {
            site: site.short_id.clone(),
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
            site: site.short_id.clone(),
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
            site: site.short_id.clone(),
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
