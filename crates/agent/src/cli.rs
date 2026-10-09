//! Clap surface mounted by the `setu` binary as `setu agent …` and
//! `setu helper …` (one binary, spec §1).
//!
//! The enums derive `Subcommand` so `crates/api/src/main.rs` embeds them
//! directly; `run_agent` / `run_helper` execute the parsed command.

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

/// `setu helper write-file`: the only privileged file writer (contents on
/// stdin; path allowlisted in `helper.rs`).
#[derive(Debug, Clone, clap::Subcommand)]
pub enum HelperCmd {
    /// Write stdin to `--path` with `--mode` and optional `--owner`.
    WriteFile {
        /// Destination path (must be allowlisted).
        #[arg(long)]
        path: PathBuf,
        /// Octal mode (`0600`, `0644`).
        #[arg(long, default_value = "0644")]
        mode: String,
        /// `chown` target (`user` or `user:group`).
        #[arg(long)]
        owner: Option<String>,
    },
    /// Run one allowlisted `systemctl` verb on one site unit.
    Systemctl {
        /// One of `start|stop|restart|show|daemon-reload`.
        #[arg(long)]
        verb: String,
        /// Site unit (`setu-site@<id>.service`; omit for `daemon-reload`).
        #[arg(long)]
        unit: Option<String>,
    },
    /// Print the journal tail for one site unit to stdout.
    Journal {
        /// Site unit (`setu-site@<id>.service`).
        #[arg(long)]
        unit: String,
        /// Number of lines (default 200, max 5000).
        #[arg(long, default_value = "200")]
        lines: u32,
    },
    /// Remove one site's systemd drop-in dir (always under
    /// `/etc/systemd/system`; no override — the path is sudo-pinned).
    RemoveDropin {
        /// Site unit (`setu-site@<id>.service`).
        #[arg(long)]
        unit: String,
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

/// Execute a parsed `setu helper …` command (contents arrive on stdin).
pub async fn run_helper(cmd: HelperCmd) -> anyhow::Result<()> {
    match cmd {
        HelperCmd::WriteFile { path, mode, owner } => {
            use tokio::io::AsyncReadExt;
            let mut contents = Vec::new();
            tokio::io::stdin()
                .read_to_end(&mut contents)
                .await
                .map_err(|e| anyhow::anyhow!("cannot read stdin: {e}"))?;
            crate::helper::write_file(&path, &contents, &mode, owner.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(())
        }
        HelperCmd::Systemctl { verb, unit } => {
            let out = crate::helper::systemctl(&verb, unit.as_deref())
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            print!("{out}");
            Ok(())
        }
        HelperCmd::Journal { unit, lines } => {
            let lines = crate::helper::journal(&unit, lines).map_err(|e| anyhow::anyhow!("{e}"))?;
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        HelperCmd::RemoveDropin { unit } => {
            crate::helper::remove_dropin(std::path::Path::new("/etc/systemd/system"), &unit)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // The unit set changed: reload so systemd drops the override.
            crate::helper::systemctl("daemon-reload", None).map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(())
        }
    }
}
