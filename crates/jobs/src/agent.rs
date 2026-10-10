//! [`AgentClient`]: one typed call to a server's agent.
//!
//! Production path ([`DbAgentClient`]) enqueues into `agent_commands`
//! with the command's own TTL ([`Command::default_timeout`]) and blocks
//! in `wait`; the agent long-polls and posts the result. Tests use
//! [`testing::FakeAgent`], a scripted in-memory agent.

use std::sync::Arc;

use async_trait::async_trait;
use setu_common::{AppError, ErrorCode};
use setu_db::{Db, WaitError};
use setu_proto::{Command, CommandResult};

/// One typed agent call. The TTL (`expires_at` and the wait) comes from
/// [`Command::default_timeout`]; `site_short` is the agent's per-site
/// FIFO key (`CommandEnvelope.site_short_id`).
#[async_trait]
pub trait AgentClient: Send + Sync {
    async fn call(
        &self,
        server_id: &str,
        site_short: Option<&str>,
        cmd: Command,
    ) -> Result<CommandResult, AgentError>;
}

/// Failures of one agent call.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// No result within the TTL: the command was cancelled, so it is
    /// never delivered late; the job's retry issues a fresh command.
    #[error("agent timed out")]
    Timeout,
    /// Someone else cancelled the command while we waited.
    #[error("agent command was cancelled")]
    Cancelled,
    /// The agent never picked the command up before `expires_at`.
    #[error("agent command expired before delivery")]
    Expired,
    /// The command could not be queued or awaited at all.
    #[error("agent unreachable: {0}")]
    Unreachable(String),
    /// The agent ran the command and reported failure.
    #[error("agent command failed: {0}")]
    Failed(String),
}

impl AgentError {
    /// Worth one fresh command (spec §5: the job's single retry).
    pub fn is_retryable(&self) -> bool {
        matches!(self, Self::Timeout | Self::Expired)
    }
}

impl From<AgentError> for AppError {
    fn from(e: AgentError) -> Self {
        match e {
            AgentError::Timeout => {
                AppError::agent_unreachable("agent timed out waiting for command")
            }
            AgentError::Cancelled => AppError::agent_unreachable("agent command was cancelled"),
            AgentError::Expired => {
                AppError::agent_unreachable("agent did not pick the command up in time")
            }
            AgentError::Unreachable(msg) => AppError::agent_unreachable(msg),
            // Operator-visible job errors keep the agent's one-line
            // message (agent logs never carry env values).
            AgentError::Failed(msg) => AppError::new(ErrorCode::Internal, msg),
        }
    }
}

/// Snake-case `kind` tag for a command (proto's [`Command::kind`]).
pub fn command_kind(cmd: &Command) -> &'static str {
    cmd.kind()
}

/// Production client: enqueue with the command's TTL, wait as long, and
/// on a wait timeout cancel the command (redacted, never delivered late,
/// spec §5) and report [`AgentError::Timeout`].
pub struct DbAgentClient {
    db: Db,
}

impl DbAgentClient {
    pub fn new(db: Db) -> Self {
        Self { db }
    }
}

#[async_trait]
impl AgentClient for DbAgentClient {
    async fn call(
        &self,
        server_id: &str,
        site_short: Option<&str>,
        cmd: Command,
    ) -> Result<CommandResult, AgentError> {
        let ttl = cmd.default_timeout();
        let envelope = self
            .db
            .commands()
            .enqueue(server_id, site_short, &cmd, ttl)
            .await
            .map_err(|e| AgentError::Unreachable(e.message))?;
        let result = match self.db.commands().wait(&envelope.id, ttl).await {
            Ok(result) => result,
            Err(WaitError::Timeout) => {
                if let Err(e) = self.db.commands().cancel(&envelope.id).await {
                    tracing::warn!(error = %e, "cancel after timeout failed");
                }
                return Err(AgentError::Timeout);
            }
            Err(WaitError::Cancelled) => return Err(AgentError::Cancelled),
            Err(WaitError::Expired) => return Err(AgentError::Expired),
            Err(WaitError::Db(e)) => return Err(AgentError::Unreachable(e.message)),
        };
        if result.ok {
            Ok(result)
        } else {
            Err(AgentError::Failed(
                result
                    .error
                    .unwrap_or_else(|| "agent command failed".to_string()),
            ))
        }
    }
}

/// Test doubles: [`FakeAgent`] and [`FixedClock`].
// Test double: a poisoned lock means a test already panicked (R146).
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub mod testing {
    use super::{async_trait, AgentClient, AgentError, Arc, Command, CommandResult};
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use setu_proto::EnvVar;

    pub use crate::clock::Clock;

    /// A clock the test sets and advances.
    #[derive(Debug, Clone)]
    pub struct FixedClock(Arc<Mutex<DateTime<Utc>>>);

    impl FixedClock {
        pub fn new(at: DateTime<Utc>) -> Self {
            Self(Arc::new(Mutex::new(at)))
        }

        pub fn set(&self, at: DateTime<Utc>) {
            *self.0.lock().expect("clock lock") = at;
        }

        pub fn advance(&self, by: chrono::Duration) {
            let mut at = self.0.lock().expect("clock lock");
            *at += by;
        }
    }

    impl Clock for FixedClock {
        fn now(&self) -> DateTime<Utc> {
            *self.0.lock().expect("clock lock")
        }
    }

    #[derive(Default)]
    struct FakeInner {
        transcript: Vec<String>,
        /// Serialized commands, in call order (for [`FakeAgent::saw_secret`]).
        commands: Vec<serde_json::Value>,
        ttls: Vec<(String, Duration)>,
        calls: HashMap<String, usize>,
        /// `kind -> (nth call, message)`: that call returns `ok: false`.
        fail_at: HashMap<String, (usize, String)>,
        /// `(kind, nth)`: that call returns [`AgentError::Timeout`].
        timeout_at: HashSet<(String, usize)>,
        delays: HashMap<String, Duration>,
        in_flight: HashMap<String, usize>,
        max_in_flight: HashMap<String, usize>,
        /// `Status` calls after this many report `active: false`.
        status_inactive_after: Option<usize>,
        listening_ports: Vec<u16>,
        write_envs: Vec<Vec<EnvVar>>,
        switch_versions: Vec<String>,
        log_lines: Vec<String>,
    }

    /// Scripted agent: records every call, returns `ok` results by
    /// default. Clones share state, so the handle kept by the test
    /// observes the calls made through the `Arc<dyn AgentClient>` held
    /// by [`crate::JobCtx`].
    #[derive(Clone)]
    pub struct FakeAgent {
        inner: Arc<Mutex<FakeInner>>,
    }

    /// Decrements the in-flight count even when the call is dropped
    /// mid-delay (an aborted runner).
    struct InFlight {
        inner: Arc<Mutex<FakeInner>>,
        kind: String,
    }

    impl Drop for InFlight {
        fn drop(&mut self) {
            if let Ok(mut inner) = self.inner.lock() {
                if let Some(n) = inner.in_flight.get_mut(&self.kind) {
                    *n = n.saturating_sub(1);
                }
            }
        }
    }

    impl FakeAgent {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(FakeInner {
                    log_lines: vec![
                        "fake log line 1".to_string(),
                        "fake log line 2".to_string(),
                        "fake log line 3".to_string(),
                    ],
                    ..FakeInner::default()
                })),
            }
        }

        fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
            self.inner.lock().expect("fake agent lock")
        }

        /// Kind names in call order (see [`Command::kind`]).
        pub fn transcript(&self) -> Vec<String> {
            self.lock().transcript.clone()
        }

        /// `(kind, TTL)` of every call, in order.
        pub fn ttls(&self) -> Vec<(String, Duration)> {
            self.lock().ttls.clone()
        }

        /// Kinds of the calls whose command carried `secret` anywhere.
        pub fn saw_secret(&self, secret: &str) -> Vec<String> {
            self.lock()
                .commands
                .iter()
                .filter(|c| c.to_string().contains(secret))
                .map(|c| c["kind"].as_str().unwrap_or_default().to_string())
                .collect()
        }

        /// `env` payload of each `WriteEnv` call, in order.
        pub fn write_env_calls(&self) -> Vec<Vec<EnvVar>> {
            self.lock().write_envs.clone()
        }

        /// `version` payload of each `SwitchVersion` call, in order.
        pub fn switch_versions(&self) -> Vec<String> {
            self.lock().switch_versions.clone()
        }

        pub fn call_count(&self, kind: &str) -> usize {
            self.lock().calls.get(kind).copied().unwrap_or(0)
        }

        /// Most calls of `kind` ever in flight at once.
        pub fn max_concurrent(&self, kind: &str) -> usize {
            self.lock().max_in_flight.get(kind).copied().unwrap_or(0)
        }

        /// The nth call of `kind` (1-based) returns `ok: false`.
        pub fn fail_at(&self, kind: &str, nth: usize) {
            self.fail_with(kind, nth, "injected failure");
        }

        /// [`fail_at`](Self::fail_at) with an explicit error message.
        pub fn fail_with(&self, kind: &str, nth: usize, msg: &str) {
            self.lock()
                .fail_at
                .insert(kind.to_string(), (nth, msg.to_string()));
        }

        /// The next call of `kind` returns `ok: false`.
        pub fn fail_next(&self, kind: &str) {
            let nth = self.call_count(kind) + 1;
            self.fail_at(kind, nth);
        }

        /// [`fail_next`](Self::fail_next) with an explicit message.
        pub fn fail_next_with(&self, kind: &str, msg: &str) {
            let nth = self.call_count(kind) + 1;
            self.fail_with(kind, nth, msg);
        }

        /// The nth call of `kind` (1-based) returns [`AgentError::Timeout`].
        pub fn timeout_at(&self, kind: &str, nth: usize) {
            self.lock().timeout_at.insert((kind.to_string(), nth));
        }

        /// The next call of `kind` times out; the retry then succeeds.
        pub fn timeout_once(&self, kind: &str) {
            let nth = self.call_count(kind) + 1;
            self.timeout_at(kind, nth);
        }

        /// Every call of `kind` takes `by` before answering.
        pub fn delay(&self, kind: &str, by: Duration) {
            self.lock().delays.insert(kind.to_string(), by);
        }

        /// What `Status` reports (`active: true` by default).
        pub fn set_status_active(&self, active: bool) {
            self.lock().status_inactive_after = if active { None } else { Some(0) };
        }

        /// The first `nth` `Status` calls report active, every later one
        /// inactive (the app died after that many checks).
        pub fn status_inactive_after(&self, nth: usize) {
            self.lock().status_inactive_after = Some(nth);
        }

        /// TCP ports `Inventory` reports in `LISTEN` (none by default).
        pub fn set_listening_ports(&self, ports: Vec<u16>) {
            self.lock().listening_ports = ports;
        }

        /// Calls of `kind` stop being delayed.
        pub fn clear_delay(&self, kind: &str) {
            self.lock().delays.remove(kind);
        }

        /// The serialized commands of `kind`, in call order (secrets
        /// included: tests only).
        pub fn commands_of(&self, kind: &str) -> Vec<serde_json::Value> {
            self.lock()
                .commands
                .iter()
                .filter(|c| c["kind"] == kind)
                .cloned()
                .collect()
        }

        /// What `FetchLogs` reports (three canned lines by default).
        pub fn set_log_lines(&self, lines: Vec<String>) {
            self.lock().log_lines = lines;
        }

        /// Record the call; the scripted reply plus how long to wait.
        fn script(&self, cmd: &Command) -> (Result<CommandResult, AgentError>, Option<Duration>) {
            let mut inner = self.lock();
            let kind = cmd.kind().to_string();
            let n = {
                let n = inner.calls.entry(kind.clone()).or_insert(0);
                *n += 1;
                *n
            };
            inner.transcript.push(kind.clone());
            inner
                .commands
                .push(serde_json::to_value(cmd).unwrap_or_default());
            inner.ttls.push((kind.clone(), cmd.default_timeout()));
            let delay = inner.delays.get(&kind).copied();
            if inner.timeout_at.contains(&(kind.clone(), n)) {
                return (Err(AgentError::Timeout), delay);
            }
            if let Some((nth, msg)) = inner.fail_at.get(&kind) {
                if *nth == n {
                    let reply = CommandResult {
                        id: setu_common::new_id(),
                        ok: false,
                        output: serde_json::Value::Null,
                        log: "fake failure".to_string(),
                        error: Some(msg.clone()),
                    };
                    return (Ok(reply), delay);
                }
            }
            match cmd {
                Command::WriteEnv { env, .. } => inner.write_envs.push(env.clone()),
                Command::SwitchVersion { version, .. } => {
                    inner.switch_versions.push(version.clone());
                }
                _ => {}
            }
            let output = match cmd {
                Command::Status { .. } => {
                    let active = inner.status_inactive_after.is_none_or(|after| n <= after);
                    serde_json::json!({"active": active})
                }
                Command::Backup { .. } => serde_json::json!({"size_bytes": 4096}),
                // Cursor `fake-<n>`: the lines after the first n.
                Command::FetchLogs { cursor, lines, .. } => {
                    let from = cursor
                        .as_deref()
                        .and_then(|c| c.strip_prefix("fake-"))
                        .and_then(|n| n.parse::<usize>().ok())
                        .unwrap_or(0)
                        .min(inner.log_lines.len());
                    let take = usize::try_from(*lines).unwrap_or(usize::MAX);
                    let page: Vec<String> =
                        inner.log_lines[from..].iter().take(take).cloned().collect();
                    let next = from + page.len();
                    serde_json::to_value(setu_proto::LogsOutput {
                        lines: page,
                        cursor: Some(format!("fake-{next}")),
                    })
                    .unwrap_or_default()
                }
                Command::Inventory => serde_json::to_value(setu_proto::InventoryReport {
                    listening_ports: inner.listening_ports.clone(),
                    ..Default::default()
                })
                .unwrap_or_default(),
                _ => serde_json::json!({}),
            };
            let reply = CommandResult {
                id: setu_common::new_id(),
                ok: true,
                output,
                log: "fake ok".to_string(),
                error: None,
            };
            (Ok(reply), delay)
        }
    }

    impl Default for FakeAgent {
        fn default() -> Self {
            Self::new()
        }
    }

    #[async_trait]
    impl AgentClient for FakeAgent {
        async fn call(
            &self,
            _server_id: &str,
            _site_short: Option<&str>,
            cmd: Command,
        ) -> Result<CommandResult, AgentError> {
            let (reply, delay) = self.script(&cmd);
            let kind = cmd.kind().to_string();
            let _guard = {
                let mut inner = self.lock();
                let now = {
                    let n = inner.in_flight.entry(kind.clone()).or_insert(0);
                    *n += 1;
                    *n
                };
                let max = inner.max_in_flight.entry(kind.clone()).or_insert(0);
                *max = (*max).max(now);
                InFlight {
                    inner: self.inner.clone(),
                    kind,
                }
            };
            if let Some(delay) = delay {
                tokio::time::sleep(delay).await;
            }
            reply
        }
    }
}
