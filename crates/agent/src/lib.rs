//! Server agent library, mounted by the `setu` binary as `setu agent …`
//! and `setu helper …` (one binary, spec §1).
//!
//! The agent supervises sites as systemd template units, manages the
//! local Postgres through `sudo -u postgres`, and writes Caddy routes
//! through its admin API. Every command handler in [`dispatch::handle`]
//! is idempotent, so the control plane may redeliver freely.

pub mod backup;
pub mod caddy;
pub mod cli;
pub mod config;
pub mod dispatch;
pub mod download;
pub mod helper;
pub mod install;
pub mod postgres;
pub mod run_loop;
pub mod runtime;
pub mod state;
pub mod systemd;

pub use caddy::{Caddy, CaddyAdmin, CaddyFake};
pub use cli::{run_agent, run_helper, AgentCmd, HelperCmd};
pub use config::AgentConfig;
pub use dispatch::{command_kind, handle};
pub use postgres::{FakePostgres, PostgresAdmin, PsqlCli};
pub use runtime::{NativeRuntime, Runtime, RuntimeStatus};
pub use state::{SiteEntry, StateFile};
pub use systemd::{FakeSystemd, SystemctlCli, Systemd};
