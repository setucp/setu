//! The agent loop (spec §5; the plan's `loop::run`, named `run_loop`
//! because `loop` is a keyword).
//!
//! One task long-polls `GET /agent/v1/commands?wait=30`. Each envelope
//! takes its place in its site's FIFO queue at once, then runs in its
//! own task together with its result post, so polling continues while a
//! 15-minute install runs; same-site order is the queue's. Results are
//! persisted before posting ([`crate::commands::handle`]); on start and
//! after any failed post, every persisted result the control plane never
//! acknowledged is posted again. Poll errors back off 1 s → 30 s.
//! Heartbeat ([`crate::heartbeat::run`]) and reconcile
//! ([`crate::reconcile`]) run as their own tasks.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use setu_proto::{CommandEnvelope, CommandResult, Heartbeat};

use crate::caddy::{CaddyAdmin, CaddySocket};
use crate::commands::Agent;
use crate::config::AgentConfig;
use crate::helper_client::{Helper, SudoHelper};
use crate::postgres::{PostgresAdmin, PsqlSudo};
use crate::runtime::NativeRuntime;

/// Production runtime wiring.
pub type ProdRuntime = NativeRuntime<SudoHelper, PsqlSudo, CaddySocket>;

/// Build the production runtime from config.
pub fn prod_runtime(config: &AgentConfig) -> ProdRuntime {
    NativeRuntime::new(
        SudoHelper::new(),
        PsqlSudo::system(),
        CaddySocket::new(&config.caddy_socket),
        config.clone(),
    )
}

/// The control plane's agent API, authenticated by the server token.
pub struct ControlPlane {
    client: reqwest::Client,
    base: String,
    token: String,
    wait_secs: u64,
}

impl std::fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlPlane")
            .field("base", &self.base)
            .field("token", &"[redacted]")
            .field("wait_secs", &self.wait_secs)
            .finish()
    }
}

impl ControlPlane {
    pub fn new(config: &AgentConfig) -> anyhow::Result<Self> {
        Ok(Self {
            client: config.http_client()?,
            base: config.base_url(),
            token: config.server_token.clone(),
            wait_secs: config.poll_wait_secs,
        })
    }

    /// `GET /agent/v1/commands?wait=<n>`: the next command or `None`.
    pub async fn poll(&self) -> anyhow::Result<Option<CommandEnvelope>> {
        let resp = self
            .client
            .get(format!("{}/agent/v1/commands", self.base))
            .query(&[("wait", self.wait_secs)])
            .bearer_auth(&self.token)
            .timeout(Duration::from_secs(self.wait_secs + 30))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("poll: {e}"))?;
        if resp.status() == reqwest::StatusCode::NO_CONTENT {
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!("poll: HTTP {}", resp.status());
        }
        let envelope: CommandEnvelope = resp
            .json()
            .await
            .map_err(|e| anyhow::anyhow!("poll decode: {e}"))?;
        Ok(Some(envelope))
    }

    /// `POST /agent/v1/commands/{id}/result`. A 404 (the command is
    /// gone: pruned, or not this server's) counts as acknowledged, so it
    /// is never retried forever.
    pub async fn post_result(&self, result: &CommandResult) -> anyhow::Result<()> {
        let resp = self
            .client
            .post(format!(
                "{}/agent/v1/commands/{}/result",
                self.base, result.id
            ))
            .bearer_auth(&self.token)
            .json(result)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("result post: {e}"))?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        anyhow::bail!("result post: HTTP {}", resp.status())
    }

    /// `POST /agent/v1/heartbeat`.
    pub async fn heartbeat(&self, hb: &Heartbeat) -> anyhow::Result<()> {
        let resp = self
            .client
            .post(format!("{}/agent/v1/heartbeat", self.base))
            .bearer_auth(&self.token)
            .json(hb)
            .timeout(Duration::from_secs(30))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("heartbeat: {e}"))?;
        if !resp.status().is_success() {
            anyhow::bail!("heartbeat: HTTP {}", resp.status());
        }
        Ok(())
    }
}

/// Backoff bounds for poll errors.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    pub min: Duration,
    pub max: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            min: Duration::from_secs(1),
            max: Duration::from_secs(30),
        }
    }
}

/// Post `result`; mark it acknowledged in state, or flag a re-post.
async fn post_and_mark<H, P, C>(
    agent: &Agent<H, P, C>,
    cp: &ControlPlane,
    result: &CommandResult,
    repost: &AtomicBool,
) -> bool {
    match cp.post_result(result).await {
        Ok(()) => {
            if let Err(e) = agent.runtime().state().mark_posted(&result.id) {
                tracing::warn!(cmd = %result.id, error = %e, "cannot mark result posted");
            }
            true
        }
        Err(e) => {
            tracing::warn!(cmd = %result.id, error = %e, "result post failed; will re-post");
            repost.store(true, Ordering::SeqCst);
            false
        }
    }
}

/// Post every persisted result the control plane never acknowledged,
/// oldest first; stops (and re-flags) at the first failure.
pub async fn repost_unposted<H, P, C>(
    agent: &Agent<H, P, C>,
    cp: &ControlPlane,
    repost: &AtomicBool,
) {
    for result in agent.runtime().state().unposted() {
        if !post_and_mark(agent, cp, &result, repost).await {
            break;
        }
    }
}

/// The long-poll loop. Never returns; drop/abort the task to stop it.
pub async fn run<H, P, C>(agent: Arc<Agent<H, P, C>>, cp: Arc<ControlPlane>, backoff: Backoff)
where
    H: Helper + 'static,
    P: PostgresAdmin + Send + Sync + 'static,
    C: CaddyAdmin + 'static,
{
    // Start by re-posting whatever a previous process left unposted.
    let repost = Arc::new(AtomicBool::new(true));
    let mut delay = backoff.min;
    loop {
        if repost.swap(false, Ordering::SeqCst) {
            repost_unposted(&agent, &cp, &repost).await;
        }
        match cp.poll().await {
            Ok(Some(envelope)) => {
                delay = backoff.min;
                let id = envelope.id.clone();
                let kind = envelope.command.kind();
                tracing::info!(cmd = %id, kind, "command received");
                // Takes the site slot now, in delivery order.
                let task = agent.spawn(envelope);
                let (agent, cp, repost) = (agent.clone(), cp.clone(), repost.clone());
                tokio::spawn(async move {
                    match task.await {
                        Ok(result) => {
                            tracing::info!(cmd = %id, kind, ok = result.ok, "command handled");
                            post_and_mark(&agent, &cp, &result, &repost).await;
                        }
                        Err(e) => tracing::error!(cmd = %id, error = %e, "command task failed"),
                    }
                });
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, backoff_s = delay.as_secs(), "poll failed");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(backoff.max);
            }
        }
    }
}

/// Reconcile now and then every `every`.
pub async fn reconcile_forever<H, P, C>(agent: Arc<Agent<H, P, C>>, every: Duration)
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    loop {
        match crate::reconcile::run_runtime(agent.runtime()).await {
            Ok(inv) => tracing::info!(
                units = inv.units.len(),
                routes = inv.routes.len(),
                "reconciled"
            ),
            Err(e) => tracing::warn!(error = %e, "reconcile failed"),
        }
        tokio::time::sleep(every).await;
    }
}

/// `setu agent run`: reconcile + heartbeat tasks, then the loop.
pub async fn run_forever(config: AgentConfig) -> anyhow::Result<()> {
    let cp = Arc::new(ControlPlane::new(&config)?);
    let agent = Agent::new(prod_runtime(&config));
    tokio::spawn(reconcile_forever(
        agent.clone(),
        Duration::from_secs(config.reconcile_secs.max(1)),
    ));
    tokio::spawn(crate::heartbeat::run(
        agent.clone(),
        cp.clone(),
        Duration::from_secs(config.heartbeat_secs.max(5)),
    ));
    run(
        agent,
        cp,
        Backoff {
            min: Duration::from_secs(config.backoff_min_secs.max(1)),
            max: Duration::from_secs(config.backoff_max_secs.max(1)),
        },
    )
    .await;
    Ok(())
}
