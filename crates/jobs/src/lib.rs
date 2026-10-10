//! Site state machines over an [`AgentClient`] (spec §5, plan Task 6).
//!
//! Each public `run` function drives one job kind to completion:
//! [`provision`], [`update`], [`backup`], [`restore`], [`delete`],
//! [`delete_backup`], [`env_update`], [`lifecycle`], with [`runner`]
//! claiming jobs from the Postgres queue. Every step runs through
//! [`JobCtx::step`]: it stores the step as the job's cursor, then logs
//! `step <name>: ok` / `step <name>: error <msg>` (into `jobs.log` when
//! the context carries a job id) with every registered secret replaced
//! by `***`. A reclaimed job resumes from its cursor ([`Steps`]).
//!
//! Retry policy: agent calls go through [`call_agent`], which issues
//! exactly one fresh command after [`AgentError::Timeout`] or
//! [`AgentError::Expired`] (the timed-out one was cancelled, spec §5) —
//! the agent is idempotent, so a repeated step is safe. Any other failure
//! aborts the state machine; provision marks the site `failed` with the
//! step name, update rolls back to the old version.

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rand::SeedableRng;
use setu_common::{AppError, ErrorCode, Sealer};
use setu_db::{Db, Plan, Site};
use setu_manifest::{Manifest, ResolveInput, Target};
use tokio::sync::Semaphore;
pub mod agent;
pub mod backup;
pub mod clock;
pub mod delete;
pub mod delete_backup;
pub mod env_update;
pub mod lifecycle;
pub mod provision;
pub mod restore;
pub mod runner;
pub mod scheduler;
pub mod update;

pub use agent::{testing, AgentClient, AgentError, DbAgentClient};
pub use clock::{Clock, SystemClock};
pub use runner::{Runner, RunnerConfig};
pub use scheduler::{Scheduler, SchedulerConfig};

/// At most this many `install` steps per server at once (spec §6).
pub const INSTALLS_PER_SERVER: usize = 2;

/// Per-server `install` semaphores shared by every job of one runner.
#[derive(Clone)]
pub struct InstallSlots {
    per_server: usize,
    map: Arc<Mutex<HashMap<String, Arc<Semaphore>>>>,
}

impl InstallSlots {
    pub fn new(per_server: usize) -> Self {
        Self {
            per_server: per_server.max(1),
            map: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// The server's semaphore (created on first use).
    pub fn installs_for(&self, server_id: &str) -> Arc<Semaphore> {
        let mut map = self.map.lock().expect("install slots lock");
        map.entry(server_id.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(self.per_server)))
            .clone()
    }
}

impl Default for InstallSlots {
    fn default() -> Self {
        Self::new(INSTALLS_PER_SERVER)
    }
}

/// Default pause between `set_route` and the provision `verify` status
/// check (spec §6: "`status` is active 3 s after the route").
pub const VERIFY_DELAY: Duration = Duration::from_secs(3);

/// Shared context for every state machine.
///
/// Holds the [`Sealer`] and the secrets registered for redaction, so
/// [`std::fmt::Debug`] is manual and redacts them; the `log` closure
/// only ever receives redacted lines.
pub struct JobCtx {
    pub db: Db,
    pub agent: Arc<dyn AgentClient>,
    pub sealer: Sealer,
    pub clock: Arc<dyn Clock>,
    pub installs: InstallSlots,
    pub log: Box<dyn Fn(&str) + Send + Sync>,
    pub job_id: Option<String>,
    /// The runner worker holding the job's lease; every job-row write
    /// carries it, and the repo refuses writes from a lost lease (M2).
    pub worker: Option<String>,
    /// Set once a job-row write found the lease gone: every later step
    /// fails at once with `conflict` (a lost lease is fatal for the run).
    lease_lost: std::sync::atomic::AtomicBool,
    /// Pause before the provision `verify` step ([`VERIFY_DELAY`]).
    pub verify_delay: Duration,
    /// Plaintext secrets of this run, scrubbed from every log line and
    /// error message ([`JobCtx::redact`]).
    secrets: Mutex<Vec<String>>,
}

impl JobCtx {
    /// A context on the system clock with its own install slots (direct
    /// `run` callers and tests; the runner builds its own).
    pub fn new(
        db: Db,
        agent: Arc<dyn AgentClient>,
        sealer: Sealer,
        log: impl Fn(&str) + Send + Sync + 'static,
    ) -> Self {
        Self::build(
            db,
            agent,
            sealer,
            Arc::new(SystemClock),
            InstallSlots::default(),
            Box::new(log),
        )
    }

    /// Every field explicit (the runner's constructor).
    pub fn build(
        db: Db,
        agent: Arc<dyn AgentClient>,
        sealer: Sealer,
        clock: Arc<dyn Clock>,
        installs: InstallSlots,
        log: Box<dyn Fn(&str) + Send + Sync>,
    ) -> Self {
        Self {
            db,
            agent,
            sealer,
            clock,
            installs,
            log,
            job_id: None,
            worker: None,
            lease_lost: std::sync::atomic::AtomicBool::new(false),
            verify_delay: VERIFY_DELAY,
            secrets: Mutex::new(Vec::new()),
        }
    }

    /// Bind the context to job `job_id`, leased to `worker`.
    pub fn with_job(mut self, job_id: &str, worker: &str) -> Self {
        self.job_id = Some(job_id.to_string());
        self.worker = Some(worker.to_string());
        self
    }

    /// True once a write found that this run no longer holds the lease.
    pub fn lease_lost(&self) -> bool {
        self.lease_lost.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn mark_lease_lost(&self) {
        if !self
            .lease_lost
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            tracing::warn!(job_id = ?self.job_id, "job lease lost: stopping this run");
        }
    }

    /// `(job id, worker)` when the context drives a leased job.
    fn leased(&self) -> Option<(&str, &str)> {
        Some((
            self.job_id.as_deref()?,
            self.worker.as_deref().unwrap_or(""),
        ))
    }

    fn lost_error() -> AppError {
        AppError::conflict("job lease lost: another runner owns it")
    }

    pub fn with_verify_delay(mut self, delay: Duration) -> Self {
        self.verify_delay = delay;
        self
    }

    /// The server's `install` semaphore (spec §6: two per server).
    pub fn installs_for(&self, server_id: &str) -> Arc<Semaphore> {
        self.installs.installs_for(server_id)
    }

    /// Register a plaintext secret: from now on it is replaced by `***`
    /// in every log line and step error of this context.
    pub fn add_secret(&self, secret: &str) {
        if secret.len() < 4 {
            return; // too short to scrub without mangling ordinary text
        }
        let mut secrets = self.secrets.lock().expect("secrets lock");
        if !secrets.iter().any(|s| s == secret) {
            secrets.push(secret.to_string());
        }
    }

    /// `text` with every registered secret replaced by `***`.
    pub fn redact(&self, text: &str) -> String {
        let secrets = self.secrets.lock().expect("secrets lock");
        secrets
            .iter()
            .fold(text.to_string(), |acc, s| acc.replace(s.as_str(), "***"))
    }

    /// Append one line (redacted) to the context log and, when the
    /// context carries a job id, to `jobs.log`.
    pub async fn log(&self, line: &str) {
        let line = self.redact(line);
        (self.log)(&line);
        if let Some((id, worker)) = self.leased() {
            match self
                .db
                .jobs()
                .append_log(id, worker, &format!("{line}\n"))
                .await
            {
                Ok(true) => {}
                Ok(false) => self.mark_lease_lost(),
                Err(e) => tracing::warn!(job_id = %id, error = %e, "job log append failed"),
            }
        }
    }

    /// The job's step cursor (`payload.step`); `None` without a job.
    pub async fn cursor(&self) -> Option<String> {
        let id = self.job_id.as_ref()?;
        self.db.jobs().get_step(id).await.ok().flatten()
    }

    /// One payload key of the job; `None` without a job or key.
    pub async fn payload_get(&self, key: &str) -> Option<serde_json::Value> {
        let id = self.job_id.as_ref()?;
        let job = self.db.jobs().get(id).await.ok()?;
        job.payload.get(key).cloned().filter(|v| !v.is_null())
    }

    /// [`payload_get`](Self::payload_get) for a string value.
    pub async fn payload_str(&self, key: &str) -> Option<String> {
        self.payload_get(key)
            .await
            .and_then(|v| v.as_str().map(str::to_string))
    }

    /// Set one payload key (a no-op without a job: direct `run` callers
    /// never resume, so they keep their state in locals).
    pub async fn payload_set(&self, key: &str, value: serde_json::Value) -> Result<(), AppError> {
        let Some((id, worker)) = self.leased() else {
            return Ok(());
        };
        let job = self.db.jobs().get(id).await?;
        let mut payload = match job.payload {
            serde_json::Value::Object(map) => map,
            _ => serde_json::Map::new(),
        };
        payload.insert(key.to_string(), value);
        let set = self
            .db
            .jobs()
            .set_payload(id, worker, &serde_json::Value::Object(payload))
            .await;
        if set.as_ref().is_err_and(|e| e.code == ErrorCode::Conflict) {
            self.mark_lease_lost();
        }
        set
    }

    /// Run one named step: store it as the job's cursor first (a resumed
    /// job re-runs it — every step is idempotent), run `fut`, log `step
    /// <name>: ok` or `step <name>: error <msg>` with secrets redacted
    /// (the returned error is redacted too).
    pub async fn step<T, F>(&self, name: &str, fut: F) -> Result<T, AppError>
    where
        F: Future<Output = Result<T, AppError>>,
    {
        if self.lease_lost() {
            return Err(Self::lost_error());
        }
        if let Some((id, worker)) = self.leased() {
            if let Err(e) = self.db.jobs().set_step(id, worker, name).await {
                if e.code == ErrorCode::Conflict {
                    self.mark_lease_lost();
                }
                return Err(e);
            }
        }
        match fut.await {
            Ok(v) => {
                self.log(&format!("step {name}: ok")).await;
                Ok(v)
            }
            Err(mut e) => {
                e.message = self.redact(&e.message);
                self.log(&format!("step {name}: error {}", e.message)).await;
                Err(e)
            }
        }
    }

    /// Call the agent and require `ok`: the output, or the agent's
    /// one-line error as `internal` (redacted by the step). `install`
    /// holds one of the server's install slots.
    pub(crate) async fn agent_ok(
        &self,
        server_id: &str,
        cmd: setu_proto::Command,
    ) -> Result<serde_json::Value, AppError> {
        let called = if matches!(cmd, setu_proto::Command::Install { .. }) {
            call_install(self, server_id, cmd).await
        } else {
            call_agent(self, server_id, cmd).await
        };
        match called {
            Ok(result) => require_ok(result).map_err(|msg| AppError::new(ErrorCode::Internal, msg)),
            Err(e) => Err(AppError::from(e)),
        }
    }
}

impl std::fmt::Debug for JobCtx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobCtx")
            .field("db", &self.db)
            .field("agent", &"AgentClient")
            .field("sealer", &"[redacted]")
            .field("secrets", &"[redacted]")
            .field("job_id", &self.job_id)
            .field("worker", &self.worker)
            .finish()
    }
}

/// Named steps resumed from a stored cursor (spec §6): steps before the
/// cursor are complete and skipped; the cursor step itself re-runs (it
/// may have been interrupted, and every step is idempotent).
#[derive(Debug, Clone)]
pub struct Steps {
    names: &'static [&'static str],
    from: usize,
}

impl Steps {
    /// Resume `names` from `cursor` (`None`, or a name not in `names`,
    /// starts from the first step).
    pub fn run_from(cursor: Option<&str>, names: &'static [&'static str]) -> Self {
        let from = cursor
            .and_then(|c| names.iter().position(|n| *n == c))
            .unwrap_or(0);
        Self { names, from }
    }

    /// Whether the run resumes past the first step.
    pub fn resuming(&self) -> bool {
        self.from > 0
    }

    /// Whether `name` still has to run.
    pub fn due(&self, name: &str) -> bool {
        let idx = self.names.iter().position(|n| *n == name);
        debug_assert!(idx.is_some(), "unknown step {name}");
        idx.is_some_and(|i| i >= self.from)
    }

    /// [`JobCtx::step`] when `name` is due; `Ok(None)` when it was
    /// completed before the cursor (`fut` is dropped unpolled).
    pub async fn run<T, F>(&self, ctx: &JobCtx, name: &str, fut: F) -> Result<Option<T>, AppError>
    where
        F: Future<Output = Result<T, AppError>>,
    {
        if !self.due(name) {
            return Ok(None);
        }
        ctx.step(name, fut).await.map(Some)
    }
}

/// Mark the site `failed` with the step name and return the error.
pub(crate) async fn mark_failed(
    ctx: &JobCtx,
    site_id: &str,
    step: &str,
    err: AppError,
) -> AppError {
    let _ = ctx
        .db
        .sites()
        .set_status(site_id, "failed", Some(step))
        .await;
    err
}

/// Call the agent; after a timeout (the command was cancelled) or an
/// expiry, issue exactly one fresh command. Safe because every agent
/// command is idempotent (spec §5).
pub(crate) async fn call_agent(
    ctx: &JobCtx,
    server_id: &str,
    cmd: setu_proto::Command,
) -> Result<setu_proto::CommandResult, AgentError> {
    let short = cmd.site_short_id().map(str::to_string);
    match ctx
        .agent
        .call(server_id, short.as_deref(), cmd.clone())
        .await
    {
        Err(e) if e.is_retryable() => ctx.agent.call(server_id, short.as_deref(), cmd).await,
        other => other,
    }
}

/// [`call_agent`] for an `install`, holding one of the server's install
/// slots for the duration (spec §6: at most two at once per server).
pub(crate) async fn call_install(
    ctx: &JobCtx,
    server_id: &str,
    cmd: setu_proto::Command,
) -> Result<setu_proto::CommandResult, AgentError> {
    let slots = ctx.installs_for(server_id);
    let _permit = slots
        .acquire_owned()
        .await
        .map_err(|_| AgentError::Unreachable("install slots closed".to_string()))?;
    call_agent(ctx, server_id, cmd).await
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

/// `data/` and `run/` of a site, for env resolution only (`from =
/// "dir"`/`"run_dir"` values the app reads). Nothing path-like is sent
/// to the agent: it derives every path from the short id (spec §4).
pub(crate) fn site_dirs(short_id: &str) -> (PathBuf, PathBuf) {
    let site_dir = PathBuf::from(format!("/srv/setu/sites/{short_id}"));
    (site_dir.join("data"), site_dir.join("run"))
}

/// A fresh database role password (48 hex characters: no quoting in
/// URLs or env files).
fn random_db_password() -> String {
    let mut bytes = [0u8; 24];
    rand::RngCore::fill_bytes(&mut rand::rng(), &mut bytes);
    hex::encode(bytes)
}

fn proto_workdir(w: setu_manifest::Workdir) -> setu_proto::Workdir {
    match w {
        setu_manifest::Workdir::ExecDir => setu_proto::Workdir::ExecDir,
        setu_manifest::Workdir::Bin => setu_proto::Workdir::Bin,
        setu_manifest::Workdir::Data => setu_proto::Workdir::Data,
    }
}

/// The manifest's x86_64 artefact; image manifests are refused.
fn binary_target(manifest: &Manifest) -> Result<&setu_manifest::BinaryTarget, AppError> {
    if manifest.run.is_image() {
        return Err(AppError::validation(
            "image runtime not available in this version",
        ));
    }
    manifest
        .run
        .target(Target::X86_64Linux)
        .ok_or_else(|| AppError::validation("run.binary must declare x86_64-linux"))
}

/// The [`setu_proto::BinaryRef`] an `install` of this manifest fetches.
pub(crate) fn binary_ref(manifest: &Manifest) -> Result<setu_proto::BinaryRef, AppError> {
    let target = binary_target(manifest)?;
    Ok(setu_proto::BinaryRef {
        version: manifest.app.version.clone(),
        url: target.url.clone(),
        sha256: target.sha256.clone(),
        exec: target.exec.clone(),
    })
}

/// The agent [`setu_proto::SiteSpec`] of `site` running `manifest`'s
/// version: no paths (spec §5), workdir from the manifest, dirs from
/// `needs.dirs`, limits from the plan.
pub(crate) fn build_spec(
    plan: &Plan,
    site: &Site,
    manifest: &Manifest,
) -> Result<setu_proto::SiteSpec, AppError> {
    let target = binary_target(manifest)?;
    Ok(setu_proto::SiteSpec {
        site_id: site.id.clone(),
        short_id: site.short_id.clone(),
        version: manifest.app.version.clone(),
        port: u16::try_from(site.port).map_err(|_| AppError::internal())?,
        exec: target.exec.clone(),
        args: manifest.run.args.clone(),
        workdir: proto_workdir(manifest.run.workdir),
        dirs: manifest.needs.dirs.clone(),
        limits: plan_limits(plan, manifest),
        health: manifest.app.health.clone(),
        startup_grace_s: manifest.app.startup_grace_s,
    })
}

/// Resolution inputs for a site (real RNG: `generate` values are drawn
/// here and nowhere else).
pub(crate) fn resolve_input(
    short_id: &str,
    port: u16,
    domain: &str,
    owner_email: &str,
    db_url: String,
    prompts: &BTreeMap<String, String>,
) -> ResolveInput {
    let (data_dir, run_dir) = site_dirs(short_id);
    ResolveInput {
        site_url: format!("https://{domain}"),
        owner_email: owner_email.to_string(),
        port,
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

/// The site's env, generated exactly once at `POST /sites` (spec §3):
/// every rule of `manifest` resolved against the real allocation (port,
/// short id, database), `generate` values from the OS RNG, a fresh
/// database role password inside `postgres_url`, prompt answers from
/// the operator. Returns the SEALED storage form for `sites.env`
/// (secrets `v1:`, the rest `plain:`). Provision only ever reads it.
pub fn resolve_site_env(
    manifest: &Manifest,
    alloc: &setu_db::Allocation,
    domain: &str,
    owner_email: &str,
    prompts: &BTreeMap<String, String>,
    sealer: &Sealer,
) -> Result<serde_json::Value, AppError> {
    let port = u16::try_from(alloc.port).map_err(|_| AppError::internal())?;
    let db_url = format!(
        "postgres://{}:{}@127.0.0.1:5432/{}",
        alloc.db_role,
        random_db_password(),
        alloc.db_name
    );
    let mut input = resolve_input(&alloc.short_id, port, domain, owner_email, db_url, prompts);
    let vars = setu_manifest::resolve_env(manifest, &mut input).map_err(resolve_error)?;
    Ok(sealed_env(sealer, &vars))
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

/// [`to_proto_envs`] without `once` values (every `write_env` except the
/// first boot's, spec §2).
pub(crate) fn without_once(vars: &[setu_manifest::EnvVar]) -> Vec<setu_proto::EnvVar> {
    vars.iter().filter(|v| !v.once).map(to_proto_env).collect()
}

/// Plaintext of a stored env array (`sites.env`, or a sealed copy in a
/// job payload), in stored order. Every value is prefixed (`v1:` sealed,
/// `plain:` not secret); corrupt or unprefixed storage is `internal`
/// (ciphertext never surfaces).
pub(crate) fn open_env(
    env: &serde_json::Value,
    sealer: &Sealer,
) -> Result<Vec<setu_manifest::EnvVar>, AppError> {
    let Some(arr) = env.as_array() else {
        return Ok(Vec::new());
    };
    let mut out = Vec::with_capacity(arr.len());
    for v in arr {
        let Some(name) = v.get("name").and_then(|n| n.as_str()) else {
            continue;
        };
        let value = v.get("value").and_then(|x| x.as_str()).unwrap_or("");
        let plain = sealer.open(value).map_err(|e| {
            tracing::error!(error = %e, "stored env failed to open");
            AppError::internal()
        })?;
        out.push(setu_manifest::EnvVar {
            name: name.to_string(),
            value: plain,
            secret: v.get("secret").and_then(|b| b.as_bool()).unwrap_or(false),
            once: v.get("once").and_then(|b| b.as_bool()).unwrap_or(false),
        });
    }
    Ok(out)
}

/// The site's stored env in plaintext, every secret registered with the
/// context for redaction.
pub(crate) fn site_env(ctx: &JobCtx, site: &Site) -> Result<Vec<setu_manifest::EnvVar>, AppError> {
    let vars = open_env(&site.env, &ctx.sealer)?;
    register_secrets(ctx, &vars);
    Ok(vars)
}

/// Register every secret value (and the password inside a database
/// URL) for redaction.
pub(crate) fn register_secrets(ctx: &JobCtx, vars: &[setu_manifest::EnvVar]) {
    for v in vars.iter().filter(|v| v.secret) {
        ctx.add_secret(&v.value);
        if let Some(pw) = url_password(&v.value) {
            ctx.add_secret(pw);
        }
    }
}

/// The password of a `postgres://role:password@host/db` URL.
pub(crate) fn url_password(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("postgres://")?;
    let (userinfo, _) = rest.split_once('@')?;
    let (_, pw) = userinfo.split_once(':')?;
    Some(pw).filter(|p| !p.is_empty())
}

/// The stored `postgres_url` value (the variable whose manifest rule is
/// `from = "postgres_url"`), if the manifest has one.
pub(crate) fn stored_db_url<'a>(
    manifest: &Manifest,
    vars: &'a [setu_manifest::EnvVar],
) -> Option<&'a str> {
    vars.iter()
        .find(|v| {
            matches!(manifest.env.get(&v.name),
                Some(setu_manifest::EnvSpec::From(f)) if f.from == setu_manifest::EnvSource::PostgresUrl)
        })
        .map(|v| v.value.as_str())
}

/// The database role password provision converges on: the one inside
/// the stored `postgres_url` (generated at creation, so a retry sends
/// the same password); a manifest without a `postgres_url` rule has no
/// reader of the password, so a fresh one is fine (ledger R92).
pub(crate) fn db_password(manifest: &Manifest, vars: &[setu_manifest::EnvVar]) -> String {
    stored_db_url(manifest, vars)
        .and_then(url_password)
        .map(str::to_string)
        .unwrap_or_else(random_db_password)
}

/// Stored DB copy of resolved env: secret values sealed (`v1:`), the rest
/// `plain:`, `secret`/`once` flags kept for the UI.
pub(crate) fn sealed_env(sealer: &Sealer, vars: &[setu_manifest::EnvVar]) -> serde_json::Value {
    env_json(vars, |v| sealer.store(&v.value, v.secret))
}

/// Every value sealed (`v1:`), secret or not: the job-payload copy of an
/// env (spec §6: the old env is stored sealed in the update payload).
pub(crate) fn fully_sealed_env(
    sealer: &Sealer,
    vars: &[setu_manifest::EnvVar],
) -> serde_json::Value {
    env_json(vars, |v| sealer.seal(&v.value))
}

fn env_json(
    vars: &[setu_manifest::EnvVar],
    value: impl Fn(&setu_manifest::EnvVar) -> String,
) -> serde_json::Value {
    serde_json::Value::Array(
        vars.iter()
            .map(|v| {
                serde_json::json!({
                    "name": v.name,
                    "value": value(v),
                    "secret": v.secret,
                    "once": v.once,
                })
            })
            .collect(),
    )
}
