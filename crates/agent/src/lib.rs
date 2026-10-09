//! Server agent library, mounted by the `setu` binary as `setu agent …`
//! and `setu helper …` (one binary, spec §1).
//!
//! The agent runs unprivileged as `setu`: every privileged step is one
//! typed `setu helper <verb> --site <short>` through `sudo -n`
//! ([`helper_client::SudoHelper`], [`helper`]). It manages the local
//! Postgres through `sudo -u postgres` and Caddy through its admin
//! unix socket ([`caddy::CaddySocket`]).
//! Every command handler in [`dispatch::handle`] is idempotent, so the
//! control plane may redeliver freely.

pub mod backup;
pub mod caddy;
pub mod cli;
pub mod config;
pub mod dispatch;
pub mod download;
pub mod helper;
pub mod helper_client;
pub mod install;
pub mod install_system;
pub mod journal;
pub mod metrics;
pub mod paths;
pub mod postgres;
pub mod run_loop;
pub mod runtime;
pub mod state;

pub use caddy::{CaddyAdmin, CaddySocket, FakeCaddy, PanelRoute};
pub use cli::{run_agent, AgentCmd};
pub use config::AgentConfig;
pub use dispatch::{command_kind, handle};
pub use helper_client::{FakeHelper, Helper, HelperOutput, LocalHelper, SudoHelper};
pub use paths::Paths;
pub use postgres::{FakePostgres, PostgresAdmin, PsqlCli};
pub use runtime::{NativeRuntime, Runtime, RuntimeStatus};
pub use state::{SiteEntry, StateFile};
