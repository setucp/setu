//! Site state machines over an [`AgentClient`] (spec §5, plan Task 6).
//!
//! Each public `run` function drives one job kind to completion:
//! [`provision`], [`update`], [`backup`], [`restore`], [`delete`], with
//! [`runner`] claiming jobs from the Postgres queue. Every step logs
//! `step <name>: ok` / `step <name>: error <msg>` through [`JobCtx::log`]
//! (and into `jobs.log` when the context carries a job id). Log lines
//! never contain env values: only step names and agent error one-liners.
//!
//! Retry policy: agent calls go through [`call_agent`], which retries
//! exactly once on [`AgentError::Timeout`] — the agent is idempotent, so
//! a redelivered command is safe (Review-Focus #3). Any other failure
//! aborts the state machine; provision marks the site `failed` with the
//! step name, update rolls back to the old version.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use rand::SeedableRng;
use setu_common::{AppError, ErrorCode, Sealer};
use setu_db::{Db, Plan, Site};
use setu_manifest::{Manifest, ResolveInput, Run, Target};
pub mod agent;
pub mod backup;
pub mod delete;
pub mod provision;
pub mod restore;
pub mod runner;
pub mod update;

pub use agent::{testing, AgentClient, AgentError, DbAgentClient};
pub use runner::Runner;

/// Shared context for every state machine.
///
/// Holds the [`Sealer`], so [`std::fmt::Debug`] is manual and redacts it;
/// the `log` closure must never be called with env values.
pub struct JobCtx {
    pub db: Db,
    pub agent: std::sync::Arc<dyn AgentClient>,
    pub sealer: Sealer,
    pub log: Box<dyn Fn(&str) + Send + Sync>,
    pub job_id: Option<String>,
    pub timeout: Duration,
}

impl JobCtx {
    /// Default agent-call timeout is 120 s (spec Review-Focus #3: the
    /// control plane times out a command, re-queues it, and the agent's
    /// idempotent handling makes the retry succeed).
    pub fn new(
        db: Db,
        agent: std::sync::Arc<dyn AgentClient>,
        sealer: Sealer,
        log: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            db,
            agent,
            sealer,
            log: Box::new(log),
            job_id: None,
            timeout: Duration::from_secs(120),
        }
    }

    pub fn with_job(mut self, job_id: &str) -> Self {
        self.job_id = Some(job_id.to_string());
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }
}

impl std::fmt::Debug for JobCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCtx")
            .field("db", &self.db)
            .field("agent", &"AgentClient")
            .field("sealer", &"[redacted]")
            .field("job_id", &self.job_id)
            .field("timeout", &self.timeout)
            .finish()
    }
}

/// Append one `step <name>: ok|error …` line to the context log and, when
/// the context carries a job id, to `jobs.log`. Never pass env values.
pub(crate) async fn log_step(ctx: &JobCtx, name: &str, ok: bool, detail: Option<&str>) {
    let line = match (ok, detail) {
        (true, _) => format!("step {name}: ok"),
        (false, Some(d)) => format!("step {name}: error {d}"),
        (false, None) => format!("step {name}: error"),
    };
    (ctx.log)(&line);
    if let Some(id) = &ctx.job_id {
        let _ = ctx.db.jobs().append_log(id, &format!("{line}\n")).await;
    }
}

/// Mark the site `failed` with the step name and return the error.
/// The message is an agent/validation one-liner, never an env value.
pub(crate) async fn fail(ctx: &JobCtx, site_id: &str, step: &str, err: AppError) -> AppError {
    let detail = err.message.clone();
    log_step(ctx, step, false, Some(&detail)).await;
    let _ = ctx
        .db
        .sites()
        .set_status(site_id, "failed", Some(step))
        .await;
    err
}

/// Call the agent, retrying exactly once on [`AgentError::Timeout`].
/// Safe because every agent command is idempotent (spec §4).
pub(crate) async fn call_agent(
    ctx: &JobCtx,
    server_id: &str,
    cmd: setu_proto::Command,
) -> Result<setu_proto::CommandResult, AgentError> {
    match ctx.agent.call(server_id, cmd.clone(), ctx.timeout).await {
        Err(AgentError::Timeout) => ctx.agent.call(server_id, cmd, ctx.timeout).await,
        other => other,
    }
}

/// Turn an agent result into its output, or the agent's error one-liner.
pub(crate) fn require_ok(result: setu_proto::CommandResult) -> Result<serde_json::Value, String> {
    if result.ok {
        Ok(result.output)
    } else {
        Err(result
            .error
            .unwrap_or_else(|| "agent command failed".to_string()))
    }
}

/// One agent step for flows that mark the site `failed` on error
/// (provision, restart): call, require `ok`, log, or fail the site.
pub(crate) async fn agent_step(
    ctx: &JobCtx,
    site_id: &str,
    step: &'static str,
    server_id: &str,
    cmd: setu_proto::Command,
) -> Result<setu_proto::CommandResult, AppError> {
    match call_agent(ctx, server_id, cmd).await {
        Ok(result) if result.ok => {
            log_step(ctx, step, true, None).await;
            Ok(result)
        }
        Ok(result) => {
            let msg = result
                .error
                .unwrap_or_else(|| "agent command failed".to_string());
            Err(fail(ctx, site_id, step, AppError::new(ErrorCode::Internal, msg)).await)
        }
        Err(e) => Err(fail(ctx, site_id, step, AppError::from(e)).await),
    }
}

/// Load a validated manifest struct from the stored `app_versions` JSON.
pub(crate) async fn load_manifest(
    db: &Db,
    app_id: &str,
    version: &str,
) -> Result<Manifest, AppError> {
    let row = db.apps().version(app_id, version).await?;
    serde_json::from_value(row.manifest).map_err(|e| {
        tracing::error!(error = %e, "stored manifest is corrupt");
        AppError::internal()
    })
}

#[derive(serde::Deserialize)]
struct LimitsJson {
    memory_mb: u32,
    cpu_percent: u32,
    disk_mb: u32,
}

/// Plan limits win; the manifest default is the fallback when the stored
/// plan JSON is unreadable.
pub(crate) fn plan_limits(plan: &Plan, manifest: &Manifest) -> setu_proto::Limits {
    if let Ok(limits) = serde_json::from_value::<LimitsJson>(plan.limits.clone()) {
        return setu_proto::Limits {
            memory_mb: limits.memory_mb,
            cpu_percent: limits.cpu_percent,
            disk_mb: limits.disk_mb,
        };
    }
    setu_proto::Limits {
        memory_mb: manifest.limits.default.memory_mb,
        cpu_percent: manifest.limits.default.cpu_percent,
        disk_mb: manifest.limits.default.disk_mb,
    }
}

/// A server counts as online when it reports `online` and was seen
/// within the last 60 s (mirrors `servers().online()`).
fn server_online(server: &setu_db::Server) -> bool {
    if server.status != "online" {
        return false;
    }
    match server.last_seen_at {
        Some(seen) => (chrono::Utc::now() - seen).num_seconds() < 60,
        None => false,
    }
}

/// Capacity check (spec §5 step 1): the summed plan memory of the
/// server's live sites plus this site must stay under 90 % of the
/// server's memory. Unknown capacity (or unloadable plans) skips the
/// check rather than blocking provisioning.
pub(crate) async fn check_capacity(
    db: &Db,
    site: &Site,
    plan: &Plan,
    manifest: &Manifest,
) -> Result<(), AppError> {
    let server = db.servers().get(&site.server_id).await?;
    if !server_online(&server) {
        return Err(AppError::validation(format!(
            "server {} ({}) is offline",
            server.name, server.id
        )));
    }
    let server_mem = match server.memory_mb {
        Some(m) if m > 0 => m as u64,
        _ => return Ok(()),
    };
    let need = plan_limits(plan, manifest).memory_mb as u64;
    let sites = db.sites().list().await?;
    let mut used = 0u64;
    for other in sites.iter().filter(|s| {
        s.server_id == site.server_id
            && s.id != site.id
            && s.status != "failed"
            && s.status != "deleting"
    }) {
        if let Ok(p) = db.plans().get(&other.plan_id).await {
            if let Ok(l) = serde_json::from_value::<LimitsJson>(p.limits) {
                used += u64::from(l.memory_mb);
            }
        }
    }
    if used + need > (server_mem as f64 * 0.9) as u64 {
        return Err(AppError::conflict(
            "server has insufficient memory capacity",
        ));
    }
    Ok(())
}

/// Site directory layout derived from the stored data dir:
/// `<site_dir>/data` is stored, siblings are `run` and `env`.
pub(crate) fn site_paths(site: &Site) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let data_dir = PathBuf::from(&site.data_dir);
    let site_dir = data_dir.parent().map_or_else(
        || PathBuf::from(format!("/srv/setu/sites/{}", site.id)),
        PathBuf::from,
    );
    let run_dir = site_dir.join("run");
    let env_file = site_dir.join("env");
    (site_dir, data_dir, run_dir, env_file)
}

fn random_db_password() -> String {
    let mut bytes = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    hex::encode(bytes)
}

/// A fully materialised site: agent spec plus resolved env.
/// `env_manifest` holds PLAINTEXT values: callers send them to the agent
/// via `WriteEnv` and store only the sealed copy in the DB.
pub(crate) struct BuiltSite {
    pub spec: setu_proto::SiteSpec,
    pub env_manifest: Vec<setu_manifest::EnvVar>,
    pub db_password: String,
}

/// Build the agent [`setu_proto::SiteSpec`] and resolve env per manifest.
/// Rejects `image` manifests: the phase-1 control plane cannot run them.
pub(crate) fn build_site(
    plan: &Plan,
    site: &Site,
    manifest: &Manifest,
) -> Result<BuiltSite, AppError> {
    let (binary, args) = match &manifest.run {
        Run::Binary { binary: run } => run
            .targets
            .get(&Target::X86_64Linux)
            .map(|target| (target, run.args.clone()))
            .ok_or_else(|| AppError::validation("run.binary must declare x86_64-linux"))?,
        Run::Image { .. } => {
            return Err(AppError::validation(
                "image runtime not available in this version",
            ));
        }
    };
    let (site_dir, data_dir, run_dir, env_file) = site_paths(site);
    let db_password = random_db_password();
    let db_url = format!(
        "postgres://{}:{}@127.0.0.1:5432/{}",
        site.db_name, db_password, site.db_name
    );
    let mut input = ResolveInput {
        site_url: format!("https://{}", site.domain),
        owner_email: site.owner_email.clone(),
        port: site.port as u16,
        run_dir: run_dir.clone(),
        data_dir: data_dir.clone(),
        db_url,
        prompts: BTreeMap::new(),
        rng: rand::rngs::StdRng::from_os_rng(),
    };
    let env_manifest = setu_manifest::resolve_env(manifest, &mut input).map_err(|e| match &e {
        setu_manifest::ResolveError::MissingPrompt(_) => {
            AppError::validation(format!("prompt values missing: {e}"))
        }
        setu_manifest::ResolveError::Invalid(_, _) => AppError::validation(e.to_string()),
    })?;
    let spec = setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: setu_common::short_id(&site.id),
        user: format!("setu-site-{}", site.id),
        data_dir,
        run_dir,
        port: site.port as u16,
        exec: site_dir.join("bin").join("current").join(&binary.exec),
        args,
        env_file,
        limits: plan_limits(plan, manifest),
        health_path: manifest.app.health.clone(),
        binary: Some(setu_proto::BinaryRef {
            url: binary.url.clone(),
            sha256: binary.sha256.clone(),
            exec: binary.exec.clone(),
            version: manifest.app.version.clone(),
        }),
    };
    Ok(BuiltSite {
        spec,
        env_manifest,
        db_password,
    })
}

/// Minimal spec when the manifest is unavailable (delete path): no binary,
/// default limits. Enough for `Stop`/`Remove`, which key off `site_id`.
pub(crate) fn minimal_spec(site: &Site) -> setu_proto::SiteSpec {
    let (site_dir, data_dir, run_dir, env_file) = site_paths(site);
    setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: setu_common::short_id(&site.id),
        user: format!("setu-site-{}", site.id),
        data_dir,
        run_dir,
        port: site.port as u16,
        exec: site_dir.join("bin").join("current").join("app"),
        args: Vec::new(),
        env_file,
        limits: setu_proto::Limits {
            memory_mb: 512,
            cpu_percent: 100,
            disk_mb: 5120,
        },
        health_path: "/healthz".to_string(),
        binary: None,
    }
}

fn to_proto_env(v: &setu_manifest::EnvVar) -> setu_proto::EnvVar {
    setu_proto::EnvVar {
        name: v.name.clone(),
        value: v.value.clone(),
        secret: v.secret,
        once: v.once,
    }
}

/// Everything before the first agent call: manifest + plan + capacity
/// checks, env resolution, and storing the SEALED env copy. Image
/// manifests are rejected here, before anything is issued.
pub(crate) struct Prep {
    pub manifest: Manifest,
    pub spec: setu_proto::SiteSpec,
    pub env_all: Vec<setu_proto::EnvVar>,
    pub env_persistent: Vec<setu_proto::EnvVar>,
    pub db_password: String,
    pub has_once: bool,
}

pub(crate) async fn prepare_site(ctx: &JobCtx, site: &Site) -> Result<Prep, AppError> {
    let manifest = load_manifest(&ctx.db, &site.app_id, &site.app_version).await?;
    if matches!(manifest.run, Run::Image { .. }) {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    let allowed: Vec<String> =
        serde_json::from_value(plan.allowed_app_ids.clone()).unwrap_or_default();
    if !allowed.is_empty() && !allowed.iter().any(|a| a == &site.app_id) {
        return Err(AppError::validation(format!(
            "plan does not allow app {}",
            site.app_id
        )));
    }
    check_capacity(&ctx.db, site, &plan, &manifest).await?;
    let built = build_site(&plan, site, &manifest)?;
    let sealed = sealed_env(&ctx.sealer, &built.env_manifest);
    ctx.db.sites().set_env(&site.id, &sealed).await?;
    let env_all: Vec<setu_proto::EnvVar> = built.env_manifest.iter().map(to_proto_env).collect();
    let env_persistent: Vec<setu_proto::EnvVar> = built
        .env_manifest
        .iter()
        .filter(|v| !v.once)
        .map(to_proto_env)
        .collect();
    let has_once = env_persistent.len() != env_all.len();
    Ok(Prep {
        manifest,
        spec: built.spec,
        env_all,
        env_persistent,
        db_password: built.db_password,
        has_once,
    })
}

/// Sealed DB copy of resolved env: secret values sealed with the panel
/// key, `secret`/`once` flags kept for the UI. Plain values stay plain.
pub(crate) fn sealed_env(sealer: &Sealer, vars: &[setu_manifest::EnvVar]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = vars
        .iter()
        .map(|v| {
            serde_json::json!({
                "name": v.name,
                "value": if v.secret { sealer.seal(&v.value) } else { v.value.clone() },
                "secret": v.secret,
                "once": v.once,
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}
