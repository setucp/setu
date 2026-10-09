//! [`AgentClient`]: one typed call to a server's agent.
//!
//! Production path ([`DbAgentClient`]) enqueues into `agent_commands` and
//! blocks in `wait` (the Task 8 HTTP long-poll completes it); tests use
//! [`testing::FakeAgent`], a scripted in-memory agent.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use setu_common::{AppError, ErrorCode};
use setu_db::Db;
use setu_proto::{Command, CommandResult};

/// One typed agent call. Every command is idempotent, so callers may
/// retry freely after [`AgentError::Timeout`].
#[async_trait]
pub trait AgentClient: Send + Sync {
    async fn call(
        &self,
        server_id: &str,
        cmd: Command,
        timeout: Duration,
    ) -> Result<CommandResult, AgentError>;
}

/// Failures of one agent call.
#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    /// No result within the timeout: the command may still be running on
    /// the agent; re-queue/redeliver is safe (idempotent commands).
    #[error("agent timed out")]
    Timeout,
    /// The command could not be delivered or awaited at all.
    #[error("agent unreachable: {0}")]
    Unreachable(String),
    /// The agent ran the command and reported failure.
    #[error("agent command failed: {0}")]
    Failed(String),
}

impl From<AgentError> for AppError {
    fn from(e: AgentError) -> Self {
        match e {
            AgentError::Timeout => {
                AppError::agent_unreachable("agent timed out waiting for command")
            }
            AgentError::Unreachable(msg) => AppError::agent_unreachable(msg),
            // Operator-visible job errors keep the agent's one-line
            // message (agent logs never carry env values).
            AgentError::Failed(msg) => AppError::new(ErrorCode::Internal, msg),
        }
    }
}

/// Snake-case `kind` tag for a command, matching the proto `kind` tag.
/// [`testing::FakeAgent`] records these in its transcript.
pub fn command_kind(cmd: &Command) -> &'static str {
    match cmd {
        Command::CreateDatabase { .. } => "create_database",
        Command::DropDatabase { .. } => "drop_database",
        Command::Install { .. } => "install",
        Command::WriteUnit { .. } => "write_unit",
        Command::WriteEnv { .. } => "write_env",
        Command::Start { .. } => "start",
        Command::Stop { .. } => "stop",
        Command::Restart { .. } => "restart",
        Command::Status { .. } => "status",
        Command::Health { .. } => "health",
        Command::SetRoute { .. } => "set_route",
        Command::RemoveRoute { .. } => "remove_route",
        Command::Backup { .. } => "backup",
        Command::Restore { .. } => "restore",
        Command::Remove { .. } => "remove",
        Command::SwitchVersion { .. } => "switch_version",
        Command::FetchLogs { .. } => "fetch_logs",
        Command::Inventory => "inventory",
    }
}

/// Production client: enqueue the command and wait for the agent's
/// result (posted via the Task 8 HTTP endpoints). A wait timeout maps
/// to [`AgentError::Timeout`] so the caller retries; `requeue_stale`
/// redelivers the command to the agent.
pub struct DbAgentClient {
    db: Db,
    default_timeout: Duration,
}

impl DbAgentClient {
    pub fn new(db: Db, default_timeout: Duration) -> Self {
        Self {
            db,
            default_timeout,
        }
    }
}

#[async_trait]
impl AgentClient for DbAgentClient {
    async fn call(
        &self,
        server_id: &str,
        cmd: Command,
        timeout: Duration,
    ) -> Result<CommandResult, AgentError> {
        let envelope = self
            .db
            .agent_commands()
            .enqueue(server_id, &cmd)
            .await
            .map_err(|e| AgentError::Unreachable(e.message))?;
        let effective = if timeout.is_zero() {
            self.default_timeout
        } else {
            timeout
        };
        let result = self
            .db
            .agent_commands()
            .wait(&envelope.id, effective)
            .await
            .map_err(|e| match e.code {
                ErrorCode::AgentUnreachable => AgentError::Timeout,
                _ => AgentError::Unreachable(e.message),
            })?;
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

/// Scripted in-memory agent for state-machine tests.
pub mod testing {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    use setu_proto::EnvVar;

    struct FakeInner {
        transcript: Vec<String>,
        calls: HashMap<String, usize>,
        /// `kind -> (nth call, message)`: that call returns `ok: false`.
        fail_at: HashMap<String, (usize, String)>,
        timeout_once: HashSet<String>,
        timed_out: HashSet<String>,
        status_active: bool,
        write_envs: Vec<Vec<EnvVar>>,
        switch_versions: Vec<String>,
        log_lines: Vec<String>,
    }

    /// Scripted agent: records every call's kind name, returns `ok`
    /// results by default. Clone shares the transcript via `Arc<Mutex>`,
    /// so the handle kept by the test observes the calls made through
    /// the `Arc<dyn AgentClient>` held by [`crate::JobCtx`].
    #[derive(Clone)]
    pub struct FakeAgent {
        inner: Arc<Mutex<FakeInner>>,
    }

    impl FakeAgent {
        pub fn new() -> Self {
            Self {
                inner: Arc::new(Mutex::new(FakeInner {
                    transcript: Vec::new(),
                    calls: HashMap::new(),
                    fail_at: HashMap::new(),
                    timeout_once: HashSet::new(),
                    timed_out: HashSet::new(),
                    status_active: true,
                    write_envs: Vec::new(),
                    switch_versions: Vec::new(),
                    log_lines: vec![
                        "fake log line 1".to_string(),
                        "fake log line 2".to_string(),
                        "fake log line 3".to_string(),
                    ],
                })),
            }
        }

        /// Kind names in call order (see [`command_kind`]).
        pub fn transcript(&self) -> Vec<String> {
            self.inner
                .lock()
                .expect("fake agent lock")
                .transcript
                .clone()
        }

        /// `env` payload of each `WriteEnv` call, in order.
        pub fn write_env_calls(&self) -> Vec<Vec<EnvVar>> {
            self.inner
                .lock()
                .expect("fake agent lock")
                .write_envs
                .clone()
        }

        /// `version` payload of each `SwitchVersion` call, in order.
        pub fn switch_versions(&self) -> Vec<String> {
            self.inner
                .lock()
                .expect("fake agent lock")
                .switch_versions
                .clone()
        }

        pub fn call_count(&self, kind: &str) -> usize {
            self.inner
                .lock()
                .expect("fake agent lock")
                .calls
                .get(kind)
                .copied()
                .unwrap_or(0)
        }

        /// The nth call of `kind` (1-based) returns `ok: false`.
        pub fn fail_at(&self, kind: &str, nth: usize) {
            self.fail_with(kind, nth, "injected failure");
        }

        /// [`fail_at`](Self::fail_at) with an explicit error message.
        pub fn fail_with(&self, kind: &str, nth: usize, msg: &str) {
            self.inner
                .lock()
                .expect("fake agent lock")
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

        /// The next call of `kind` returns [`AgentError::Timeout`];
        /// the retry then succeeds (redelivery test).
        pub fn timeout_once(&self, kind: &str) {
            self.inner
                .lock()
                .expect("fake agent lock")
                .timeout_once
                .insert(kind.to_string());
        }

        /// What `Status` reports (`active: true` by default).
        pub fn set_status_active(&self, active: bool) {
            self.inner.lock().expect("fake agent lock").status_active = active;
        }

        /// What `FetchLogs` reports (three canned lines by default).
        pub fn set_log_lines(&self, lines: Vec<String>) {
            self.inner.lock().expect("fake agent lock").log_lines = lines;
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
            cmd: Command,
            _timeout: Duration,
        ) -> Result<CommandResult, AgentError> {
            let mut inner = self.inner.lock().expect("fake agent lock");
            let kind = command_kind(&cmd).to_string();
            let n = inner.calls.entry(kind.clone()).or_insert(0);
            *n += 1;
            let n = *n;
            inner.transcript.push(kind.clone());
            if inner.timeout_once.contains(&kind) && !inner.timed_out.contains(&kind) {
                inner.timed_out.insert(kind);
                return Err(AgentError::Timeout);
            }
            if let Some((nth, msg)) = inner.fail_at.get(&kind) {
                if *nth == n {
                    return Ok(CommandResult {
                        id: setu_common::new_id(),
                        ok: false,
                        output: serde_json::Value::Null,
                        log: "fake failure".to_string(),
                        error: Some(msg.clone()),
                    });
                }
            }
            match &cmd {
                Command::WriteEnv { env, .. } => inner.write_envs.push(env.clone()),
                Command::SwitchVersion { version, .. } => {
                    inner.switch_versions.push(version.clone());
                }
                _ => {}
            }
            let output = match &cmd {
                Command::Status { .. } => serde_json::json!({"active": inner.status_active}),
                Command::FetchLogs { .. } => serde_json::json!({"lines": inner.log_lines}),
                _ => serde_json::json!({}),
            };
            Ok(CommandResult {
                id: setu_common::new_id(),
                ok: true,
                output,
                log: "fake ok".to_string(),
                error: None,
            })
        }
    }
}
