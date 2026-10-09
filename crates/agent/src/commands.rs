//! `commands::handle`: the spec §5 command semantics around the
//! per-variant executor ([`crate::dispatch::execute`]).
//!
//! 1. **Dedupe**: a command id already in `state.recent` returns the
//!    cached result without executing (a command delivered twice — the
//!    agent restarted between executing and posting — runs once).
//! 2. **Expiry**: a command whose `expires_at` has passed is rejected
//!    with `expired` (checked on arrival and again after waiting for the
//!    site's slot, so a command queued behind a long install never runs
//!    late).
//! 3. **Per-site FIFO**: a command with a site short id that is not
//!    read-only takes that site's slot; `status`, `fetch_logs` and
//!    `inventory` take none. Slots are queued in delivery order
//!    ([`Agent::spawn`] enqueues before the task starts), so one site's
//!    commands run in order while other sites run concurrently.
//! 4. **Redaction**: every secret value the command carried
//!    (`create_database` password, `write_env` values) is replaced by
//!    `***` in `log` and `error`.
//! 5. **Persist before post**: the result is saved to `state.json`
//!    before it is returned (and so before the loop posts it).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use setu_proto::{Command, CommandEnvelope, CommandResult};
use tokio::sync::oneshot;

use crate::caddy::CaddyAdmin;
use crate::helper_client::Helper;
use crate::postgres::PostgresAdmin;
use crate::runtime::NativeRuntime;

/// The error string of an expired command.
pub const EXPIRED: &str = "expired";

/// The tail of one site's queue: the signal the next command waits for.
struct Tail {
    seq: u64,
    done: oneshot::Receiver<()>,
}

#[derive(Default)]
struct SlotsInner {
    next: u64,
    tails: BTreeMap<String, Tail>,
}

/// Per-site FIFO slots: a chain of one-shot signals per short id. A
/// ticket is taken synchronously in delivery order and waits for the
/// previous ticket of the same site to finish.
#[derive(Clone, Default)]
pub struct SiteSlots {
    inner: Arc<Mutex<SlotsInner>>,
}

/// A place in one site's queue (not yet running).
pub struct Ticket {
    slots: SiteSlots,
    short: String,
    seq: u64,
    prev: Option<oneshot::Receiver<()>>,
    done: Option<oneshot::Sender<()>>,
}

/// Holding a site's slot; dropping it lets the next command run.
pub struct SlotGuard {
    slots: SiteSlots,
    short: String,
    seq: u64,
    _done: Option<oneshot::Sender<()>>,
}

impl SiteSlots {
    fn lock(&self) -> std::sync::MutexGuard<'_, SlotsInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Join the end of `short`'s queue.
    pub fn enqueue(&self, short: &str) -> Ticket {
        let (tx, rx) = oneshot::channel();
        let mut inner = self.lock();
        inner.next += 1;
        let seq = inner.next;
        let prev = inner
            .tails
            .insert(short.to_string(), Tail { seq, done: rx })
            .map(|t| t.done);
        Ticket {
            slots: self.clone(),
            short: short.to_string(),
            seq,
            prev,
            done: Some(tx),
        }
    }

    /// Sites with a queued or running command (tests, diagnostics).
    pub fn busy(&self) -> usize {
        self.lock().tails.len()
    }
}

impl Ticket {
    /// Wait for every earlier command of this site to finish.
    pub async fn acquire(mut self) -> SlotGuard {
        if let Some(prev) = self.prev.take() {
            // `Err` = the previous guard was dropped: same meaning.
            let _ = prev.await;
        }
        SlotGuard {
            slots: self.slots.clone(),
            short: std::mem::take(&mut self.short),
            seq: self.seq,
            _done: self.done.take(),
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        // A ticket dropped before acquiring (task aborted) must not
        // leave its successor waiting forever: chain the previous
        // signal on so the successor waits for it instead.
        if let (Some(prev), Some(done)) = (self.prev.take(), self.done.take()) {
            if let Ok(rt) = tokio::runtime::Handle::try_current() {
                rt.spawn(async move {
                    let _ = prev.await;
                    drop(done);
                });
            }
        }
        if !self.short.is_empty() {
            release(&self.slots, &self.short, self.seq);
        }
    }
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        release(&self.slots, &self.short, self.seq);
    }
}

/// Forget the site's tail when it is still ours (no one queued after).
fn release(slots: &SiteSlots, short: &str, seq: u64) {
    let mut inner = slots.lock();
    if inner.tails.get(short).is_some_and(|t| t.seq == seq) {
        inner.tails.remove(short);
    }
}

/// The command handler's dependencies: the runtime (helper, Postgres,
/// Caddy, state) and the per-site slots.
pub struct Agent<H, P, C> {
    rt: NativeRuntime<H, P, C>,
    slots: SiteSlots,
}

impl<H, P, C> Agent<H, P, C> {
    pub fn new(rt: NativeRuntime<H, P, C>) -> Arc<Self> {
        Arc::new(Self {
            rt,
            slots: SiteSlots::default(),
        })
    }

    pub fn runtime(&self) -> &NativeRuntime<H, P, C> {
        &self.rt
    }

    pub fn slots(&self) -> &SiteSlots {
        &self.slots
    }

    /// The slot this command must hold, if any.
    fn ticket(&self, env: &CommandEnvelope) -> Option<Ticket> {
        slot_key(&env.command).map(|short| self.slots.enqueue(short))
    }
}

/// The FIFO key: the site short id of a command that is not read-only.
pub fn slot_key(cmd: &Command) -> Option<&str> {
    if cmd.is_read_only() {
        None
    } else {
        cmd.site_short_id()
    }
}

impl<H, P, C> Agent<H, P, C>
where
    H: Helper + 'static,
    P: PostgresAdmin + Send + Sync + 'static,
    C: CaddyAdmin + 'static,
{
    /// Take the command's place in its site queue now (delivery order),
    /// then handle it in its own task.
    pub fn spawn(self: &Arc<Self>, env: CommandEnvelope) -> tokio::task::JoinHandle<CommandResult> {
        let ticket = self.ticket(&env);
        let agent = Arc::clone(self);
        tokio::spawn(async move { handle_with(&env, &agent, ticket).await })
    }
}

/// Handle one envelope (see the module docs). Equivalent to
/// [`Agent::spawn`] without the task: the slot is taken on the first
/// poll.
pub async fn handle<H, P, C>(env: &CommandEnvelope, deps: &Agent<H, P, C>) -> CommandResult
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    let ticket = deps.ticket(env);
    handle_with(env, deps, ticket).await
}

async fn handle_with<H, P, C>(
    env: &CommandEnvelope,
    deps: &Agent<H, P, C>,
    ticket: Option<Ticket>,
) -> CommandResult
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    let state = deps.rt.state();
    if let Some(cached) = state.cached(&env.id) {
        tracing::info!(cmd = %env.id, "duplicate command: returning the cached result");
        return cached;
    }
    let _guard = match ticket {
        Some(t) => Some(t.acquire().await),
        None => None,
    };
    // Re-check under the slot: a duplicate queued behind its twin.
    if let Some(cached) = state.cached(&env.id) {
        return cached;
    }
    let result = if env.expires_at <= chrono::Utc::now() {
        CommandResult {
            id: env.id.clone(),
            ok: false,
            output: serde_json::Value::Null,
            log: format!("{} not run: expired", env.command.kind()),
            error: Some(EXPIRED.to_string()),
        }
    } else {
        let raw = crate::dispatch::execute(&env.id, &env.command, &deps.rt).await;
        redact_result(raw, &secrets_of(&env.command))
    };
    if let Err(e) = state.record_result(&result, env.command.is_read_only()) {
        // Posting still beats dropping the result; dedupe is lost for
        // this id only.
        tracing::error!(cmd = %env.id, error = %e, "cannot persist result");
    }
    e2e_exit_hook(env.command.kind());
    result
}

/// Feature `e2e-hooks`: `SETU_AGENT_TEST_EXIT_AFTER=<kind>` exits the
/// process (code 0) after persisting a result of that kind and before
/// posting it — the e2e's "agent dies mid-provision" case.
#[cfg(feature = "e2e-hooks")]
fn e2e_exit_hook(kind: &str) {
    if std::env::var("SETU_AGENT_TEST_EXIT_AFTER").is_ok_and(|k| k == kind) {
        tracing::warn!(kind, "SETU_AGENT_TEST_EXIT_AFTER: exiting before post");
        std::process::exit(0);
    }
}

#[cfg(not(feature = "e2e-hooks"))]
fn e2e_exit_hook(_kind: &str) {}

/// Every secret value a command carries (non-empty only).
pub fn secrets_of(cmd: &Command) -> Vec<String> {
    let mut out: Vec<String> = match cmd {
        Command::CreateDatabase { password, .. } => vec![password.clone()],
        Command::WriteEnv { env, .. } => env.iter().map(|v| v.value.clone()).collect(),
        _ => Vec::new(),
    };
    out.retain(|s| !s.is_empty());
    // Longest first, so a value containing another is fully masked.
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out
}

/// `***` for each secret in `text`.
pub fn redact(text: &str, secrets: &[String]) -> String {
    secrets
        .iter()
        .fold(text.to_string(), |acc, s| acc.replace(s.as_str(), "***"))
}

/// Redact `log` and `error` of a result.
pub fn redact_result(mut result: CommandResult, secrets: &[String]) -> CommandResult {
    if secrets.is_empty() {
        return result;
    }
    result.log = redact(&result.log, secrets);
    result.error = result.error.map(|e| redact(&e, secrets));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_masks_every_carried_secret() {
        let cmd = Command::WriteEnv {
            site: "abcd1234".into(),
            env: vec![
                setu_proto::EnvVar {
                    name: "A".into(),
                    value: "topsecret".into(),
                    secret: true,
                    once: false,
                },
                setu_proto::EnvVar {
                    name: "B".into(),
                    value: String::new(),
                    secret: false,
                    once: false,
                },
            ],
        };
        let r = redact_result(
            CommandResult {
                id: "c".into(),
                ok: false,
                output: serde_json::Value::Null,
                log: "wrote topsecret".into(),
                error: Some("bad value topsecret".into()),
            },
            &secrets_of(&cmd),
        );
        assert_eq!(r.log, "wrote ***");
        assert_eq!(r.error.as_deref(), Some("bad value ***"));
    }

    #[tokio::test]
    async fn slots_run_one_site_in_ticket_order() {
        let slots = SiteSlots::default();
        let a1 = slots.enqueue("a");
        let a2 = slots.enqueue("a");
        let b1 = slots.enqueue("b");
        assert_eq!(slots.busy(), 2);
        // a2 cannot acquire while a1 is not done.
        let g_b = b1.acquire().await;
        let waiter = tokio::spawn(a2.acquire());
        tokio::time::sleep(std::time::Duration::from_millis(30)).await;
        assert!(!waiter.is_finished());
        let g_a1 = a1.acquire().await;
        drop(g_a1);
        let g_a2 = waiter.await.expect("join");
        drop((g_a2, g_b));
        assert_eq!(slots.busy(), 0, "tails released");
    }
}
