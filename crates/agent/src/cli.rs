//! Clap surface mounted by the `setu` binary as `setu agent …` (one
//! binary, spec §1). `setu helper …` lives in [`crate::helper::cli`].
//!
//! The enum derives `Subcommand` so `crates/api/src/main.rs` embeds it
//! directly; `run_agent` executes the parsed command.

use std::path::PathBuf;

/// `setu agent run|install|join|doctor`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum AgentCmd {
    /// Run the agent loop (long-poll, handle, heartbeat).
    Run {
        /// Agent config file (default `/etc/setu/agent.toml`).
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Install the agent on this server (root, idempotent), then join.
    Install {
        /// Control-plane base URL.
        #[arg(long)]
        control_plane: String,
        /// One-time join token from `POST /servers`.
        #[arg(long)]
        join_token: String,
        /// Where to write the agent config (default `/etc/setu/agent.toml`).
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Join the control plane with a one-time token (no system setup).
    Join {
        #[arg(long)]
        control_plane: String,
        #[arg(long)]
        join_token: String,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Check every agent dependency, print a table, fail on any gap.
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

/// Execute a parsed `setu agent …` command.
pub async fn run_agent(cmd: AgentCmd) -> anyhow::Result<()> {
    match cmd {
        AgentCmd::Run { config } => {
            let path = config.as_deref();
            let cfg = crate::config::AgentConfig::load(path)
                .map_err(|e| anyhow::anyhow!("cannot load agent config: {e}"))?;
            if cfg.server_token.is_empty() {
                anyhow::bail!("no server token: run `setu agent join` first");
            }
            crate::run_loop::run_forever(cfg).await
        }
        AgentCmd::Install {
            control_plane,
            join_token,
            config,
        } => crate::install::install(&crate::install::InstallOpts {
            control_plane,
            join_token,
            config_path: config,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}")),
        AgentCmd::Join {
            control_plane,
            join_token,
            config,
        } => crate::install::join(&crate::install::InstallOpts {
            control_plane,
            join_token,
            config_path: config,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}")),
        AgentCmd::Doctor { config } => crate::install::doctor(config.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("{e}")),
    }
}
