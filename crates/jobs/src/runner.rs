//! Job runner (spec §6 "Job runner"): claims jobs with `FOR UPDATE SKIP
//! LOCKED` and a lease, renews the lease while the job runs, reclaims
//! jobs whose lease expired (a dead runner), resets expired `running`
//! jobs on boot, and limits `install` steps per server.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::{AgentClient, Clock, InstallSlots, JobCtx};
use setu_common::{AppError, Sealer};
use setu_db::{Db, Job};

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
    /// Pause before the provision `verify` step (spec §6: 3 s).
    pub verify_delay: Duration,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            concurrency: 4,
            lease: Duration::from_secs(60),
            renew_every: Duration::from_secs(20),
            installs_per_server: crate::INSTALLS_PER_SERVER,
            idle_poll: Duration::from_secs(1),
            verify_delay: crate::VERIFY_DELAY,
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
                let jobs = self.db.jobs();
                let finished = match result {
                    Ok(()) => jobs.finish(&job.id, &self.name, true, None).await,
                    Err(e) => jobs.finish(&job.id, &self.name, false, Some(&e.message)).await,
                };
                match finished {
                    Ok(_) => {}
                    // A lost lease is fatal for this run: the new holder
                    // finishes the job; this worker just stops.
                    Err(e) if e.code == setu_common::ErrorCode::Conflict => {
                        tracing::warn!(job_id = %job.id, "job lease lost; not finishing it");
                    }
                    Err(e) => tracing::error!(error = %e, job_id = %job.id, "finishing job failed"),
                }
            }
            () = renew => {
                tracing::warn!(job_id = %job.id, "job lease lost; another runner owns it");
            }
        }
    }

    #[allow(clippy::expect_used)] // reviewed: kept as-is (R150)
    async fn dispatch(&self, job: &Job) -> Result<(), AppError> {
        let job_id = job.id.clone();
        let ctx = JobCtx::build(
            self.db.clone(),
            self.agent.clone(),
            self.sealer.clone(),
            self.clock.clone(),
            self.installs.clone(),
            Box::new(move |line: &str| {
                tracing::info!(job_id = %job_id, "{line}");
            }),
        )
        .with_job(&job.id, &self.name)
        .with_verify_delay(self.config.verify_delay);
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
                if kind != "manual" && kind != "scheduled" {
                    return Err(AppError::validation(format!(
                        "backup kind {kind} is not a job"
                    )));
                }
                crate::backup::run(&ctx, &site("backup_site")?, &kind)
                    .await
                    .map(drop)
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
            "env_update" => {
                let changes = open_changes(&self.sealer, &job.payload)?;
                crate::env_update::run(&ctx, &site("env_update")?, &changes).await
            }
            kind @ ("restart_site" | "stop_site" | "start_site") => {
                let op = crate::lifecycle::Op::from_job_kind(kind).expect("lifecycle kind");
                crate::lifecycle::run(&ctx, &site(kind)?, op).await
            }
            other => Err(AppError::validation(format!("unknown job kind {other}"))),
        }
    }
}

/// The `env_update` payload's `changes`: name → SEALED value (`PUT
/// /sites/{id}/env` seals them; plaintext never sits in `jobs.payload`).
fn open_changes(
    sealer: &Sealer,
    payload: &serde_json::Value,
) -> Result<BTreeMap<String, String>, AppError> {
    let Some(map) = payload.get("changes").and_then(|c| c.as_object()) else {
        return Err(AppError::validation("env_update job needs payload.changes"));
    };
    map.iter()
        .map(|(name, sealed)| {
            let plain = sealed
                .as_str()
                .filter(|s| s.starts_with("v1:"))
                .and_then(|s| sealer.open(s).ok())
                .ok_or_else(|| {
                    tracing::error!(name = %name, "env_update change is not sealed");
                    AppError::internal()
                })?;
            Ok((name.clone(), plain))
        })
        .collect()
}
