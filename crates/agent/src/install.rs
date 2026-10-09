//! `agent install` / `agent join` / `agent doctor` (spec §5 "Agent
//! install", §4).
//!
//! Install runs as root and is idempotent: apt packages (Caddy from its
//! official repository), user `setu`, directories, `REVOKE CONNECT ON
//! DATABASE postgres FROM PUBLIC`, then [`install_system`] (units,
//! sudoers, firewall, Caddy on its admin socket), then join and start
//! `setu-agent.service`.
//!
//! [`install_system`]: crate::install_system::install_system

use std::path::{Path, PathBuf};

use setu_proto::{JoinRequest, JoinResponse};

use crate::caddy::{CaddyAdmin, CaddySocket, PanelRoute};
use crate::config::AgentConfig;
use crate::install_system::{InstallSystemOpts, RealCommands, SystemCommands};

/// Golden sudoers, embedded so `install` and `check-sudoers.sh` agree.
pub const SUDOERS_ASSET: &str = include_str!("../assets/sudoers");

/// Template site unit, installed by `install_system`.
pub const SITE_TEMPLATE_ASSET: &str = include_str!("../assets/setu-site@.service");

/// Agent's own unit.
pub const AGENT_UNIT_ASSET: &str = include_str!("../assets/setu-agent.service");

/// Options for `install` / `join`.
#[derive(Debug, Clone)]
pub struct InstallOpts {
    pub control_plane: String,
    pub join_token: String,
    pub config_path: Option<PathBuf>,
    /// `--tls internal`: Caddy issues from its internal CA.
    pub tls_internal: bool,
    /// `--panel-host` (+ `--upstream`): the `setu-panel` route.
    pub panel: Option<PanelRoute>,
}

/// Errors from install/join/doctor.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("must run as root (uid 0)")]
    NotRoot,
    #[error("step {0} failed: {1}")]
    Step(&'static str, String),
}

fn step(name: &'static str) -> impl FnOnce(anyhow::Error) -> InstallError {
    move |e| InstallError::Step(name, format!("{e:#}"))
}

/// Full install: packages, user, dirs, Postgres revoke, `install_system`,
/// then join, then start the agent (the token must exist before the
/// first start — no crash-loop).
pub async fn install(opts: &InstallOpts) -> Result<(), InstallError> {
    require_root()?;
    let sys = RealCommands;
    ensure_packages(&sys).await?;
    ensure_user(&sys).await?;
    ensure_dirs(&sys).await?;
    ensure_postgres().await?;
    crate::install_system::install_system(&InstallSystemOpts {
        root: None,
        tls_internal: opts.tls_internal,
        panel: opts.panel.clone(),
        caddy_only: false,
    })
    .await
    .map_err(step("install_system"))?;
    join_control_plane(opts).await?;
    sys.run("systemctl", &["enable", "setu-agent.service"])
        .await
        .map_err(step("enable agent"))?;
    sys.run("systemctl", &["restart", "setu-agent.service"])
        .await
        .map_err(step("start agent"))?;
    Ok(())
}

/// `setu agent install-system`: [`crate::install_system::install_system`]
/// alone (the installer's `--caddy-only --panel-host … --upstream …`).
pub async fn install_system_cmd(opts: &InstallSystemOpts) -> Result<(), InstallError> {
    require_root()?;
    crate::install_system::install_system(opts)
        .await
        .map_err(step("install_system"))
}

/// Join only (re-join a configured host).
pub async fn join(opts: &InstallOpts) -> Result<(), InstallError> {
    join_control_plane(opts).await
}

/// The Caddy apt repository, exactly as Caddy documents it
/// (caddyserver.com/docs/install, "Debian, Ubuntu, Raspbian"), followed
/// by the package set of spec §5. Each entry is one argv; the two
/// downloads are `sh -c` pipelines.
pub fn apt_commands() -> Vec<Vec<&'static str>> {
    vec![
        vec!["apt-get", "update"],
        vec![
            "apt-get",
            "install",
            "-y",
            "debian-keyring",
            "debian-archive-keyring",
            "apt-transport-https",
            "curl",
            "gnupg",
        ],
        vec![
            "sh",
            "-c",
            "curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/gpg.key' \
             | gpg --dearmor --yes -o /usr/share/keyrings/caddy-stable-archive-keyring.gpg",
        ],
        vec![
            "sh",
            "-c",
            "curl -1sLf 'https://dl.cloudsmith.io/public/caddy/stable/debian.deb.txt' \
             > /etc/apt/sources.list.d/caddy-stable.list",
        ],
        vec![
            "chmod",
            "o+r",
            "/usr/share/keyrings/caddy-stable-archive-keyring.gpg",
            "/etc/apt/sources.list.d/caddy-stable.list",
        ],
        vec!["apt-get", "update"],
        vec![
            "apt-get",
            "install",
            "-y",
            "sudo",
            "gnupg",
            "postgresql-16",
            "caddy",
            "nftables",
            "zstd",
        ],
    ]
}

async fn ensure_packages(sys: &dyn SystemCommands) -> Result<(), InstallError> {
    for argv in apt_commands() {
        sys.run(argv[0], &argv[1..]).await.map_err(step("apt"))?;
    }
    // policy-rc.d (containers, hardened hosts) can skip the deb's own
    // start; converge Postgres explicitly. Caddy is `install_system`'s.
    sys.run("systemctl", &["enable", "--now", "postgresql"])
        .await
        .map_err(step("postgresql"))?;
    Ok(())
}

async fn ensure_user(sys: &dyn SystemCommands) -> Result<(), InstallError> {
    if sys.run("id", &["setu"]).await.is_ok() {
        return Ok(());
    }
    sys.run(
        "useradd",
        &[
            "--system",
            "--home",
            "/var/lib/setu",
            "--shell",
            "/usr/sbin/nologin",
            "setu",
        ],
    )
    .await
    .map_err(step("useradd setu"))?;
    Ok(())
}

/// Directories as `(path, owner, group, mode)`, created in order.
/// `/etc/setu` is root's: it holds `nftables.conf`, which root loads.
/// Backups stay `setu:postgres` 2770 until WS4 moves dumps to stdout.
pub const DIRS: &[(&str, &str, &str, &str)] = &[
    ("/var/lib/setu", "setu", "setu", "0750"),
    ("/var/lib/setu/downloads", "setu", "setu", "0750"),
    ("/var/lib/setu/backups", "setu", "postgres", "2770"),
    ("/etc/setu", "root", "root", "0755"),
    ("/srv/setu", "root", "root", "0755"),
    ("/srv/setu/sites", "root", "root", "0755"),
];

async fn ensure_dirs(sys: &dyn SystemCommands) -> Result<(), InstallError> {
    for (dir, owner, group, mode) in DIRS {
        sys.run(
            "install",
            &["-d", "-o", owner, "-g", group, "-m", mode, dir],
        )
        .await
        .map_err(step("dirs"))?;
    }
    Ok(())
}

/// SQL piped to `psql` as `postgres` on a fresh box (spec §4 Postgres).
pub const REVOKE_SQL: &str = "REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;\n";

async fn ensure_postgres() -> Result<(), InstallError> {
    use tokio::io::AsyncWriteExt as _;
    let mut child = tokio::process::Command::new("sudo")
        .args([
            "-n",
            "-u",
            "postgres",
            "psql",
            "-v",
            "ON_ERROR_STOP=1",
            "-X",
            "-q",
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| InstallError::Step("postgres revoke", e.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(REVOKE_SQL.as_bytes())
            .await
            .map_err(|e| InstallError::Step("postgres revoke", e.to_string()))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| InstallError::Step("postgres revoke", e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(InstallError::Step(
            "postgres revoke",
            err.lines().next().unwrap_or("psql failed").to_string(),
        ));
    }
    Ok(())
}

/// One `doctor` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Every local check (control-plane reachability excluded).
pub async fn doctor_rows(
    config: &AgentConfig,
    sys: &dyn SystemCommands,
    caddy: &dyn CaddyAdmin,
) -> Vec<DoctorRow> {
    let mut rows = Vec::new();
    let mut row = |name: &'static str, ok: bool, detail: &str| {
        rows.push(DoctorRow {
            name,
            ok,
            detail: detail.to_string(),
        });
    };
    row(
        "systemd",
        sys.run("systemctl", &["--version"]).await.is_ok(),
        "systemctl --version",
    );
    row(
        "postgres",
        sys.run(
            "sudo",
            &["-n", "-u", "postgres", "psql", "-X", "-q", "-c", "SELECT 1"],
        )
        .await
        .is_ok(),
        "sudo -n -u postgres psql",
    );
    row(
        "caddy-socket",
        caddy.routes().await.is_ok(),
        &format!(
            "caddy admin socket reachable ({})",
            config.caddy_socket.display()
        ),
    );
    row(
        "caddy-tcp-2019",
        !caddy.tcp_admin_open().await,
        "TCP 2019 closed",
    );
    row(
        "nftables",
        sys.run("nft", &["list", "table", "inet", "setu"])
            .await
            .is_ok(),
        "nft table inet setu present",
    );
    row(
        "state-file",
        crate::state::StateFile::load(&config.state_path).is_ok(),
        &config.state_path.display().to_string(),
    );
    row(
        "site-template",
        Path::new("/etc/systemd/system/setu-site@.service").is_file(),
        "setu-site@.service installed",
    );
    row(
        "server-token",
        !config.server_token.is_empty(),
        if config.server_token.is_empty() {
            "missing: run `setu agent join`"
        } else {
            "present"
        },
    );
    rows
}

/// Print a dependency table; `Err` when anything fails.
pub async fn doctor(config_path: Option<&Path>) -> Result<(), InstallError> {
    let config =
        AgentConfig::load(config_path).map_err(|e| InstallError::Step("config", e.to_string()))?;
    let mut rows = doctor_rows(
        &config,
        &RealCommands,
        &CaddySocket::new(&config.caddy_socket),
    )
    .await;
    let cp_ok = reqwest::Client::new()
        .get(format!(
            "{}/healthz",
            config.control_plane_url.trim_end_matches('/')
        ))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());
    rows.push(DoctorRow {
        name: "control-plane",
        ok: cp_ok,
        detail: config.control_plane_url.clone(),
    });
    let mut failures = 0;
    for r in &rows {
        println!(
            "{:<16} {:<4} {}",
            r.name,
            if r.ok { "ok" } else { "FAIL" },
            r.detail
        );
        if !r.ok {
            failures += 1;
        }
    }
    if failures > 0 {
        return Err(InstallError::Step(
            "doctor",
            format!("{failures} checks failed"),
        ));
    }
    Ok(())
}

fn require_root() -> Result<(), InstallError> {
    if nix::unistd::Uid::effective().is_root() {
        Ok(())
    } else {
        Err(InstallError::NotRoot)
    }
}

fn hostname() -> String {
    std::process::Command::new("hostname")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

fn cpu_cores() -> u32 {
    std::fs::read_to_string("/proc/cpuinfo")
        .map(|t| t.lines().filter(|l| l.starts_with("processor")).count() as u32)
        .unwrap_or(1)
        .max(1)
}

fn memory_mb() -> u64 {
    crate::run_loop::read_mem_total_mb().max(1)
}

async fn join_control_plane(opts: &InstallOpts) -> Result<(), InstallError> {
    let base = opts.control_plane.trim_end_matches('/').to_string();
    let request = JoinRequest {
        // The token itself is never logged (proto Debug redacts).
        join_token: opts.join_token.clone(),
        hostname: hostname(),
        arch: std::env::consts::ARCH.to_string(),
        os: std::env::consts::OS.to_string(),
        cpu_cores: cpu_cores(),
        memory_mb: memory_mb(),
        disk_mb: crate::run_loop::disk_free_mb(Path::new("/")).await,
    };
    let resp = reqwest::Client::new()
        .post(format!("{base}/agent/v1/join"))
        .json(&request)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| InstallError::Step("join", e.to_string()))?;
    if resp.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Err(InstallError::Step(
            "join",
            "join token rejected (401)".to_string(),
        ));
    }
    if !resp.status().is_success() {
        return Err(InstallError::Step(
            "join",
            format!("HTTP {}", resp.status()),
        ));
    }
    let joined: JoinResponse = resp
        .json()
        .await
        .map_err(|e| InstallError::Step("join", e.to_string()))?;
    let path = opts
        .config_path
        .clone()
        .unwrap_or_else(AgentConfig::default_path);
    let mut config =
        AgentConfig::load(Some(&path)).map_err(|e| InstallError::Step("config", e.to_string()))?;
    config.control_plane_url = base;
    config.server_id = joined.server_id;
    config.server_token = joined.server_token;
    if opts.panel.is_some() {
        config.panel = opts.panel.clone();
    }
    config
        .save(&path)
        .map_err(|e| InstallError::Step("config", e.to_string()))?;
    // Token file must not be world-readable.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| InstallError::Step("config", e.to_string()))?;
    // Join runs as root but the agent runs as `setu`: it must read its
    // own token (the e2e once found a crash-loop on `Permission denied`).
    let path_str = path
        .to_str()
        .ok_or_else(|| InstallError::Step("config", "config path is not UTF-8".to_string()))?;
    RealCommands
        .run("chown", &["setu:setu", path_str])
        .await
        .map_err(step("config owner"))?;
    println!("joined as server {}", config.server_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::caddy::FakeCaddy;
    use crate::install_system::RecordingCommands;

    #[test]
    fn site_template_uses_instance_specifiers() {
        assert!(SITE_TEMPLATE_ASSET.contains("setu-site-%i"));
        assert!(SITE_TEMPLATE_ASSET.contains("/srv/setu/sites/%i"));
        assert!(SITE_TEMPLATE_ASSET.contains("NoNewPrivileges=yes"));
    }

    #[test]
    fn agent_unit_starts_the_one_binary() {
        assert!(AGENT_UNIT_ASSET.contains("/opt/setu/setu agent run"));
        // The agent escalates via the pinned sudo helper, which a
        // `NoNewPrivileges` directive would break (the e2e caught sudo
        // refusing at the first CreateDatabase). Sites keep it.
        assert!(
            !AGENT_UNIT_ASSET
                .lines()
                .any(|l| l.trim_start().starts_with("NoNewPrivileges=")),
            "agent unit must not set NoNewPrivileges"
        );
        // No mount-namespace hardening (ledger R69): sudo children
        // share it and the helper writes under /etc; DAC stops `setu`.
        assert!(!AGENT_UNIT_ASSET.contains("ProtectSystem="));
        assert!(!AGENT_UNIT_ASSET.contains("ReadWritePaths="));
    }

    #[test]
    fn apt_adds_the_caddy_repo_before_installing_the_spec_packages() {
        let cmds = apt_commands();
        let joined: Vec<String> = cmds.iter().map(|c| c.join(" ")).collect();
        let key = joined
            .iter()
            .position(|c| c.contains("caddy-stable-archive-keyring.gpg") && c.contains("gpg.key"))
            .expect("keyring step");
        let list = joined
            .iter()
            .position(|c| c.contains("/etc/apt/sources.list.d/caddy-stable.list"))
            .expect("list step");
        let install = joined.len() - 1;
        assert!(key < list && list < install, "{joined:?}");
        assert_eq!(joined[install - 1], "apt-get update");
        assert_eq!(
            joined[install],
            "apt-get install -y sudo gnupg postgresql-16 caddy nftables zstd"
        );
    }

    #[test]
    fn revoke_sql_targets_the_postgres_database() {
        assert_eq!(
            REVOKE_SQL,
            "REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;\n"
        );
    }

    #[test]
    fn dirs_are_spec_owned() {
        assert!(DIRS.contains(&("/var/lib/setu", "setu", "setu", "0750")));
        assert!(DIRS.contains(&("/srv/setu/sites", "root", "root", "0755")));
        assert!(DIRS.contains(&("/etc/setu", "root", "root", "0755")));
    }

    #[tokio::test]
    async fn doctor_checks_caddy_socket_tcp_admin_and_nft_table() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = AgentConfig {
            state_path: dir.path().join("state.json"),
            ..AgentConfig::default()
        };
        let sys = RecordingCommands::new();
        let caddy = FakeCaddy::new();
        let rows = doctor_rows(&config, &sys, &caddy).await;
        let get = |name: &str| rows.iter().find(|r| r.name == name).expect(name).ok;
        assert!(get("caddy-socket"));
        assert!(get("caddy-tcp-2019"));
        assert!(get("nftables"));
        assert!(sys
            .calls()
            .contains(&"nft list table inet setu".to_string()));

        caddy.admin().set_up(false);
        caddy.admin().set_tcp_admin_open(true);
        let rows = doctor_rows(&config, &sys, &caddy).await;
        let get = |name: &str| rows.iter().find(|r| r.name == name).expect(name).ok;
        assert!(!get("caddy-socket"));
        assert!(!get("caddy-tcp-2019"));
    }
}
