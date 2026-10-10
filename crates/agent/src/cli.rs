//! Clap surface mounted by the `setu` binary as `setu agent …` (one
//! binary, spec §1). `setu helper …` lives in [`crate::helper::cli`].
//!
//! The enum derives `Subcommand` so `crates/api/src/main.rs` embeds it
//! directly; `run_agent` executes the parsed command.

use std::path::PathBuf;

/// `--tls` modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum TlsMode {
    /// Caddy issues certificates from its internal CA.
    Internal,
}

/// `setu agent run|install|install-system|join|doctor`.
#[derive(Debug, Clone, clap::Subcommand)]
pub enum AgentCmd {
    /// Run the agent loop (long-poll, handle, heartbeat).
    Run {
        /// Agent config file (default `/etc/setu/agent.toml`).
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Install the agent on this server (root, idempotent), then join.
    /// The one-time join token is read from `SETU_JOIN_TOKEN` or stdin,
    /// never from argv.
    Install {
        /// Control-plane base URL (`http://` only for 127.0.0.1/localhost
        /// unless `--insecure`).
        #[arg(long)]
        control_plane: String,
        /// Allow plain http to a non-loopback control plane.
        #[arg(long)]
        insecure: bool,
        /// Extra root certificate (PEM) to trust for the control plane.
        #[arg(long)]
        ca: Option<PathBuf>,
        /// Where to write the agent config (default `/etc/setu/agent.toml`).
        #[arg(long)]
        config: Option<PathBuf>,
        /// `internal`: Caddy uses its internal CA instead of ACME.
        #[arg(long, value_enum)]
        tls: Option<TlsMode>,
        /// Route this host to the panel (control-plane box).
        #[arg(long)]
        panel_host: Option<String>,
        /// Panel upstream for `--panel-host`.
        #[arg(long, default_value = crate::caddy::DEFAULT_PANEL_UPSTREAM)]
        upstream: String,
    },
    /// Write the system artefacts only (root; not reachable through sudo):
    /// units, sudoers, firewall and Caddy, or with `--caddy-only` just
    /// the Caddy config and drop-in.
    InstallSystem {
        #[arg(long)]
        caddy_only: bool,
        #[arg(long, value_enum)]
        tls: Option<TlsMode>,
        #[arg(long)]
        panel_host: Option<String>,
        #[arg(long, default_value = crate::caddy::DEFAULT_PANEL_UPSTREAM)]
        upstream: String,
    },
    /// Join the control plane (no system setup); token from
    /// `SETU_JOIN_TOKEN` or stdin.
    Join {
        #[arg(long)]
        control_plane: String,
        #[arg(long)]
        insecure: bool,
        #[arg(long)]
        ca: Option<PathBuf>,
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
            insecure,
            ca,
            config,
            tls,
            panel_host,
            upstream,
        } => crate::install::install(&crate::install::InstallOpts {
            control_plane,
            join_token: crate::install::read_join_token().map_err(|e| anyhow::anyhow!("{e}"))?,
            config_path: config,
            insecure,
            ca,
            tls_internal: tls == Some(TlsMode::Internal),
            panel: panel_route(panel_host, upstream),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}")),
        AgentCmd::InstallSystem {
            caddy_only,
            tls,
            panel_host,
            upstream,
        } => crate::install::install_system_cmd(&crate::install_system::InstallSystemOpts {
            root: None,
            tls_internal: tls == Some(TlsMode::Internal),
            panel: panel_route(panel_host, upstream),
            caddy_only,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}")),
        AgentCmd::Join {
            control_plane,
            insecure,
            ca,
            config,
        } => crate::install::join(&crate::install::InstallOpts {
            control_plane,
            join_token: crate::install::read_join_token().map_err(|e| anyhow::anyhow!("{e}"))?,
            config_path: config,
            insecure,
            ca,
            tls_internal: false,
            panel: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}")),
        AgentCmd::Doctor { config } => crate::doctor::doctor(config.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("{e}")),
    }
}

fn panel_route(host: Option<String>, upstream: String) -> Option<crate::caddy::PanelRoute> {
    host.map(|host| crate::caddy::PanelRoute { host, upstream })
}
