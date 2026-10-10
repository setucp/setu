//! Server agent library, mounted by the `setu` binary as `setu agent …`
//! and `setu helper …` (one binary, spec §1).
//!
//! The agent runs unprivileged as `setu`: every privileged step is one
//! typed `setu helper <verb> --site <short>` through `sudo -n`
//! ([`helper_client::SudoHelper`], [`helper`]). It manages the local
//! Postgres through `sudo -u postgres` and Caddy through its admin
//! unix socket ([`caddy::CaddySocket`]).
//! [`commands::handle`] applies the spec §5 command semantics (expiry,
//! dedupe, per-site FIFO, redaction, persist-before-post) around the
//! idempotent executor [`dispatch::execute`]; [`run_loop`] is the
//! concurrent long-poll loop, [`heartbeat`] and [`reconcile`] its
//! periodic tasks.

pub mod backup;
pub mod caddy;
pub mod cli;
pub mod commands;
pub mod config;
pub mod dispatch;
pub mod doctor;
pub mod download;
pub mod heartbeat;
pub mod helper;
pub mod helper_client;
pub mod install;
pub mod install_system;
pub mod journal;
pub mod metrics;
pub mod netstat;
pub mod paths;
pub mod postgres;
pub mod reconcile;
pub mod run_loop;
pub mod runtime;
pub mod state;
pub mod stream;

pub use caddy::{CaddyAdmin, CaddySocket, FakeCaddy, PanelRoute};
pub use cli::{run_agent, AgentCmd};
pub use commands::{handle, Agent};
pub use config::AgentConfig;
pub use dispatch::{command_kind, execute};
pub use helper_client::{FakeHelper, Helper, HelperIo, HelperOutput, LocalHelper, SudoHelper};
pub use paths::Paths;
pub use postgres::{
    CommandRunner, FakePostgres, FakeRunner, PostgresAdmin, PsqlSudo, RunOutput, TokioRunner,
};
pub use runtime::{NativeRuntime, Runtime, RuntimeStatus};
pub use state::{RecentResult, SiteRecord, State, StateStore};
