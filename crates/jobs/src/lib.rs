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
use setu_manifest::{Manifest, ResolveInput, Target};
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

/// The plan's limits (plan columns are required since ledger R45's
/// schema, so the manifest default is no longer a fallback).
pub(crate) fn plan_limits(plan: &Plan, _manifest: &Manifest) -> setu_proto::Limits {
    let pos = |v: i32| u32::try_from(v).unwrap_or(0);
    setu_proto::Limits {
        memory_mb: pos(plan.memory_mb),
        cpu_percent: pos(plan.cpu_percent),
        disk_mb: pos(plan.disk_mb),
    }
}

/// Provision-time guard: the site's server must be online. Capacity is
/// admitted once, by `sites().create` (85 % rule, spec §3); WS6 owns the
/// provision `validate` step.
pub(crate) async fn check_capacity(
    db: &Db,
    site: &Site,
    _plan: &Plan,
    _manifest: &Manifest,
) -> Result<(), AppError> {
    let server = db.servers().get(&site.server_id).await?;
    if server.status(chrono::Utc::now()) != setu_db::ServerStatus::Online {
        return Err(AppError::validation(format!(
            "server {} ({}) is offline",
            server.name, server.id
        )));
    }
    Ok(())
}

/// Site directory layout, derived from the short id only (spec §4):
/// `/srv/setu/sites/<short>/{data,run,env}`. WS5 drops paths from the
/// protocol entirely.
pub(crate) fn site_paths(site: &Site) -> (PathBuf, PathBuf, PathBuf, PathBuf) {
    let site_dir = PathBuf::from(format!("/srv/setu/sites/{}", site.short_id));
    let data_dir = site_dir.join("data");
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

/// Build just the agent [`setu_proto::SiteSpec`] without resolving env
/// (backup/restore/delete paths: no prompt answers available on those
/// jobs and none needed — the agent works from paths + db name).
pub(crate) fn build_spec(
    plan: &Plan,
    site: &Site,
    manifest: &Manifest,
) -> Result<setu_proto::SiteSpec, AppError> {
    if manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let target = manifest
        .run
        .target(Target::X86_64Linux)
        .ok_or_else(|| AppError::validation("run.binary must declare x86_64-linux"))?;
    let (binary_exec, args) = (target.exec.clone(), manifest.run.args.clone());
    let (site_dir, data_dir, run_dir, env_file) = site_paths(site);
    Ok(setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: site.short_id.clone(),
        user: format!("setu-site-{}", site.short_id),
        data_dir,
        run_dir,
        port: site.port as u16,
        exec: site_dir.join("bin").join("current").join(&binary_exec),
        args,
        env_file,
        limits: plan_limits(plan, manifest),
        health_path: manifest.app.health.clone(),
        binary: Some(setu_proto::BinaryRef {
            url: target.url.clone(),
            sha256: target.sha256.clone(),
            exec: binary_exec,
            version: manifest.app.version.clone(),
        }),
    })
}

/// Build the agent [`setu_proto::SiteSpec`] and resolve env per manifest.
/// Rejects `image` manifests: the phase-1 control plane cannot run them.
/// `prompts` carries the operator-supplied prompt answers (the API stores
/// them on the provision job's payload); direct `run` callers pass an
/// empty map, preserving the old resolve behaviour.
pub(crate) fn build_site(
    plan: &Plan,
    site: &Site,
    manifest: &Manifest,
    prompts: &BTreeMap<String, String>,
) -> Result<BuiltSite, AppError> {
    if manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let binary = manifest
        .run
        .target(Target::X86_64Linux)
        .ok_or_else(|| AppError::validation("run.binary must declare x86_64-linux"))?;
    let args = manifest.run.args.clone();
    let (site_dir, data_dir, run_dir, env_file) = site_paths(site);
    let db_password = random_db_password();
    let db_url = format!(
        "postgres://{}:{}@127.0.0.1:5432/{}",
        site.db_role, db_password, site.db_name
    );
    let mut input = resolve_input(site, db_url, prompts);
    let env_manifest = setu_manifest::resolve_env(manifest, &mut input).map_err(resolve_error)?;
    let spec = setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: site.short_id.clone(),
        user: format!("setu-site-{}", site.short_id),
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

/// Resolution inputs for `site` (paths from the stored data dir).
pub(crate) fn resolve_input(
    site: &Site,
    db_url: String,
    prompts: &BTreeMap<String, String>,
) -> ResolveInput {
    let (_, data_dir, run_dir, _) = site_paths(site);
    ResolveInput {
        site_url: format!("https://{}", site.domain),
        owner_email: site.owner_email.clone(),
        port: site.port as u16,
        run_dir,
        data_dir,
        db_url,
        prompts: prompts.clone(),
        rng: rand::rngs::StdRng::from_os_rng(),
    }
}

pub(crate) fn resolve_error(e: setu_manifest::ResolveError) -> AppError {
    match &e {
        setu_manifest::ResolveError::MissingPrompt(_) => {
            AppError::validation(format!("prompt values missing: {e}"))
        }
        setu_manifest::ResolveError::Invalid(_, _) => AppError::validation(e.to_string()),
    }
}

/// Minimal spec when the manifest is unavailable (delete path): no binary,
/// default limits. Enough for `Stop`/`Remove`, which key off `site_id`.
pub(crate) fn minimal_spec(site: &Site) -> setu_proto::SiteSpec {
    let (site_dir, data_dir, run_dir, env_file) = site_paths(site);
    setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: site.short_id.clone(),
        user: format!("setu-site-{}", site.short_id),
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

/// Slice form shared with the agent calls.
pub(crate) fn to_proto_envs(vars: &[setu_manifest::EnvVar]) -> Vec<setu_proto::EnvVar> {
    vars.iter().map(to_proto_env).collect()
}

/// Plaintext of the stored env: name → (value, secret, once). Every
/// stored value is prefixed (`v1:` sealed, `plain:` not secret); corrupt
/// or unprefixed storage is `internal` (ciphertext never surfaces).
pub(crate) fn stored_plaintext(
    site: &Site,
    sealer: &Sealer,
) -> Result<BTreeMap<String, (String, bool, bool)>, AppError> {
    let mut out = BTreeMap::new();
    let Some(arr) = site.env.as_array() else {
        return Ok(out);
    };
    for v in arr {
        let Some(name) = v.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let value = v
            .get("value")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        let secret = v.get("secret").and_then(|b| b.as_bool()).unwrap_or(false);
        let once = v.get("once").and_then(|b| b.as_bool()).unwrap_or(false);
        let plain = sealer.open(&value).map_err(|e| {
            tracing::error!(error = %e, "stored env failed to open");
            AppError::internal()
        })?;
        out.insert(name.to_string(), (plain, secret, once));
    }
    Ok(out)
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
    if manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    let plan = ctx.db.plans().get(&site.plan_id).await?;
    if !plan.allows(&site.app_id) {
        return Err(AppError::validation(format!(
            "plan does not allow app {}",
            site.app_id
        )));
    }
    check_capacity(&ctx.db, site, &plan, &manifest).await?;
    let prompts = job_prompts(ctx).await;
    let built = build_site(&plan, site, &manifest, &prompts)?;
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

/// Prompt answers stored on the provision job's payload by `POST /sites`
/// (Task 9). Direct `run` callers carry no job id, so this is empty for
/// them and resolution behaves exactly as before.
pub(crate) async fn job_prompts(ctx: &JobCtx) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let Some(job_id) = &ctx.job_id else {
        return out;
    };
    let Ok(job) = ctx.db.jobs().get(job_id).await else {
        return out;
    };
    let Some(obj) = job.payload.get("prompts").and_then(|p| p.as_object()) else {
        return out;
    };
    for (k, v) in obj {
        if let Some(s) = v.as_str() {
            out.insert(k.clone(), s.to_string());
        }
    }
    out
}

/// Stored DB copy of resolved env: secret values sealed (`v1:`), the rest
/// `plain:`, `secret`/`once` flags kept for the UI.
pub(crate) fn sealed_env(sealer: &Sealer, vars: &[setu_manifest::EnvVar]) -> serde_json::Value {
    let arr: Vec<serde_json::Value> = vars
        .iter()
        .map(|v| {
            serde_json::json!({
                "name": v.name,
                "value": sealer.store(&v.value, v.secret),
                "secret": v.secret,
                "once": v.once,
            })
        })
        .collect();
    serde_json::Value::Array(arr)
}
