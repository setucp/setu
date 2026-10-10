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

use crate::caddy::PanelRoute;
use crate::config::AgentConfig;
use crate::distro::{plan_for, Distro, KeyFetch, PackagePlan, Support};
use crate::install_system::{InstallSystemOpts, RealCommands, SystemCommands};

/// Golden sudoers, embedded so `install` and `check-sudoers.sh` agree.
pub const SUDOERS_ASSET: &str = include_str!("../assets/sudoers");

/// Template site unit, installed by `install_system`.
pub const SITE_TEMPLATE_ASSET: &str = include_str!("../assets/setu-site@.service");

/// Agent's own unit.
pub const AGENT_UNIT_ASSET: &str = include_str!("../assets/setu-agent.service");

/// Options for `install` / `join`. `join_token` comes from
/// `SETU_JOIN_TOKEN` or stdin, never argv (finding 5): see
/// [`read_join_token`].
#[derive(Clone)]
pub struct InstallOpts {
    pub control_plane: String,
    pub join_token: String,
    pub config_path: Option<PathBuf>,
    /// `--insecure`: plain http to a non-loopback control plane.
    pub insecure: bool,
    /// `--ca <file>`: extra root certificate (PEM) for the control plane.
    pub ca: Option<PathBuf>,
    /// `--tls internal`: Caddy issues from its internal CA.
    pub tls_internal: bool,
    /// `--panel-host` (+ `--upstream`): the `setu-panel` route.
    pub panel: Option<PanelRoute>,
    /// `--force-distro`: on an unsupported distribution, use the Debian 12 plan.
    pub force_distro: bool,
}

impl std::fmt::Debug for InstallOpts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallOpts")
            .field("control_plane", &self.control_plane)
            .field("join_token", &"[redacted]")
            .field("config_path", &self.config_path)
            .field("insecure", &self.insecure)
            .field("ca", &self.ca)
            .field("tls_internal", &self.tls_internal)
            .field("panel", &self.panel)
            .field("force_distro", &self.force_distro)
            .finish()
    }
}

/// The join token: `SETU_JOIN_TOKEN`, else the first line of stdin
/// (never argv, where `ps` and shell history would show it).
pub fn read_join_token() -> Result<String, InstallError> {
    if let Ok(t) = std::env::var("SETU_JOIN_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(t.trim().to_string());
        }
    }
    let mut line = String::new();
    std::io::stdin()
        .read_line(&mut line)
        .map_err(|e| InstallError::Step("join token", e.to_string()))?;
    join_token_from(&line)
}

/// A token line from stdin: trimmed, non-empty.
pub fn join_token_from(line: &str) -> Result<String, InstallError> {
    let t = line.trim();
    if t.is_empty() {
        return Err(InstallError::Step(
            "join token",
            "set SETU_JOIN_TOKEN or pipe the token on stdin".to_string(),
        ));
    }
    Ok(t.to_string())
}

/// `https://` always; `http://` only to `127.0.0.1`/`localhost` (any
/// loopback) unless `--insecure` (spec §5).
pub fn check_control_plane(url: &str, insecure: bool) -> Result<String, InstallError> {
    crate::download::check_url(url, &crate::download::DownloadPolicy { insecure })
        .map_err(|e| InstallError::Step("control plane", e.to_string()))?;
    Ok(url.trim_end_matches('/').to_string())
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
    // Before any system change: a bad URL or CA fails fast.
    check_control_plane(&opts.control_plane, opts.insecure)?;
    crate::config::http_client(opts.ca.as_deref()).map_err(step("ca"))?;
    let plan = host_plan(opts.force_distro)?;
    let sys = RealCommands;
    ensure_packages(&sys, &plan, &crate::distro::HttpsFetch).await?;
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
    // Standalone (the installer's `--caddy-only`): Caddy may not be
    // installed yet; `install` installs it with the other packages.
    ensure_caddy_package(&RealCommands).await?;
    crate::install_system::install_system(opts)
        .await
        .map_err(step("install_system"))
}

/// The package plan for this host (spec 0.1.2 §2). An unsupported
/// distribution is refused unless `force`, which uses the Debian 12 plan
/// and says so.
pub fn host_plan(force: bool) -> Result<PackagePlan, InstallError> {
    let distro = Distro::detect().map_err(step("distribution"))?;
    plan_on(&distro, force)
}

/// [`host_plan`] for a given distribution.
pub fn plan_on(distro: &Distro, force: bool) -> Result<PackagePlan, InstallError> {
    match plan_for(distro) {
        Support::Supported(plan) => Ok(plan),
        Support::Unsupported { id, version } if !force => Err(InstallError::Step(
            "distribution",
            format!(
                "unsupported distribution {id} {version}; Setu supports Ubuntu 22.04/24.04 and Debian 12/13 (--force-distro uses the Debian 12 plan)"
            ),
        )),
        Support::Unsupported { id, version } => {
            let debian12 = Distro {
                id: "debian".to_string(),
                version_id: "12".to_string(),
                codename: Some("bookworm".to_string()),
            };
            match plan_for(&debian12) {
                Support::Supported(plan) => {
                    println!("using the Debian 12 plan for {id} {version} (--force-distro)");
                    Ok(plan)
                }
                Support::Unsupported { .. } => Err(InstallError::Step(
                    "distribution",
                    "no Debian 12 plan".to_string(),
                )),
            }
        }
    }
}

/// Join only (re-join a configured host).
pub async fn join(opts: &InstallOpts) -> Result<(), InstallError> {
    join_control_plane(opts).await?;
    // The running agent still holds the token this join just invalidated:
    // restart it (a no-op when it is stopped) (R148).
    RealCommands
        .run("systemctl", &["try-restart", "setu-agent.service"])
        .await
        .map_err(step("restart agent"))?;
    Ok(())
}

/// `--control-plane` for `join`: the flag, else agent.toml's when this
/// host has joined before (a re-join after `rotate-token`, R148).
pub fn rejoin_control_plane(
    cli: Option<String>,
    existing: &AgentConfig,
) -> Result<String, InstallError> {
    match cli {
        Some(url) => Ok(url),
        None if !existing.server_id.is_empty() => Ok(existing.control_plane.clone()),
        None => Err(InstallError::Step(
            "control plane",
            "no agent.toml with a server id: pass --control-plane".to_string(),
        )),
    }
}

/// `(insecure, ca)` for a join: flags add to agent.toml's settings and
/// never remove them, so a re-join without `--ca` keeps the configured
/// CA and a configured `insecure` stays on (I1).
pub fn rejoin_transport(
    insecure: bool,
    ca: Option<PathBuf>,
    existing: &AgentConfig,
) -> (bool, Option<PathBuf>) {
    (
        insecure || existing.insecure,
        ca.or_else(|| existing.ca.clone()),
    )
}

/// agent.toml after a (re-)join: identity and control plane replaced,
/// `--insecure` only ever switched on, `--ca` / `--panel-host` only when
/// given; everything else (paths, intervals, panel route) kept (R148).
pub fn joined_config(
    mut config: AgentConfig,
    base: String,
    joined: JoinResponse,
    insecure: bool,
    ca: Option<PathBuf>,
    panel: Option<PanelRoute>,
) -> AgentConfig {
    config.control_plane = base;
    config.server_id = joined.server_id;
    config.server_token = joined.server_token;
    config.insecure = config.insecure || insecure;
    if ca.is_some() {
        config.ca = ca;
    }
    if panel.is_some() {
        config.panel = panel;
    }
    config
}

/// `apt-get install -y` and the plan's distribution packages (no Caddy).
fn install_argv(plan: &PackagePlan) -> Vec<&'static str> {
    let mut argv: Vec<&'static str> = vec!["apt-get", "install", "-y"];
    argv.extend(plan.packages.iter().copied());
    argv
}

/// The Caddy apt repository, exactly as Caddy documents it
/// (caddyserver.com/docs/install, "Debian, Ubuntu, Raspbian"). Each entry
/// is one argv; the two downloads are `sh -c` pipelines.
fn caddy_repo_steps() -> Vec<Vec<&'static str>> {
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
    ]
}

/// The Caddy repository steps, then the distribution packages of `plan`
/// with Caddy (spec §5 with the 0.1.2 plan).
pub fn apt_commands(plan: &PackagePlan) -> Vec<Vec<&'static str>> {
    let mut cmds = caddy_repo_steps();
    let mut install = install_argv(plan);
    install.push("caddy");
    cmds.push(install);
    cmds
}

/// Caddy's own Debian package from its GitHub release, used when Caddy's
/// apt repository cannot be reached (Cloudsmith answered `402 Access to
/// the repository has been restricted` for every path in October 2026;
/// ledger R116). Each entry is pinned by the SHA-512 that the release's
/// `caddy_<v>_checksums.txt` publishes (0.1.2 I1).
#[derive(Debug, PartialEq, Eq)]
pub struct CaddyDeb {
    pub url: &'static str,
    pub path: &'static str,
    pub sha512: &'static str,
}

/// The pinned release packages, keyed by `std::env::consts::ARCH`.
static CADDY_DEBS: [(&str, CaddyDeb); 2] = [
    (
        "x86_64",
        CaddyDeb {
            url: "https://github.com/caddyserver/caddy/releases/download/v2.11.7/caddy_2.11.7_linux_amd64.deb",
            path: "/var/cache/apt/archives/setu-caddy_2.11.7_amd64.deb",
            sha512: "47e8351c2317b427af14a103e763ca1118a3d2396a88b4c0669cdec9c4a68a957690194e2423a1633f53135741c33a41bdac2b55515b7d0f7adc8b733add50d9",
        },
    ),
    (
        "aarch64",
        CaddyDeb {
            url: "https://github.com/caddyserver/caddy/releases/download/v2.11.7/caddy_2.11.7_linux_arm64.deb",
            path: "/var/cache/apt/archives/setu-caddy_2.11.7_arm64.deb",
            sha512: "ac32f03f0eea04021f2d4e5d89bfd01b84ead115f2eba9d2a0d06447e86d4af932899e56f090993a566d0925dbd1063002929b395541a8c5f5e51d7039f7375a",
        },
    ),
];

/// The pinned Caddy release package for `arch` (`std::env::consts::ARCH`
/// names); an error for an architecture without one.
pub fn caddy_deb_for_arch(arch: &str) -> Result<&'static CaddyDeb, InstallError> {
    CADDY_DEBS
        .iter()
        .find(|(a, _)| *a == arch)
        .map(|(_, deb)| deb)
        .ok_or_else(|| {
            InstallError::Step(
                "caddy fallback",
                format!("no Caddy fallback package for {arch}"),
            )
        })
}

/// The fallback after a failed repository install: drop the Caddy list
/// (a failing source breaks every later `apt-get update`), install the
/// spec packages except Caddy from the distribution, then Caddy's
/// release `.deb` (`deb`, see [`caddy_deb_for_arch`]), verified before
/// `apt-get` sees it.
pub fn caddy_deb_fallback_commands(plan: &PackagePlan, deb: &CaddyDeb) -> Vec<Vec<String>> {
    let mut cmds = caddy_only_fallback_commands(deb);
    let packages = install_argv(plan).into_iter().map(str::to_string).collect();
    cmds.insert(2, packages);
    cmds
}

/// The release-package part of the fallback (download, verify, install,
/// remove), shared by both fallbacks.
fn caddy_deb_steps(deb: &CaddyDeb) -> Vec<Vec<String>> {
    let v = |a: &[&str]| {
        a.iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };
    vec![
        v(&[
            "curl", "-fsSL", "--proto", "=https", "--retry", "3", "-o", deb.path, deb.url,
        ]),
        v(&[
            "sh",
            "-c",
            &format!(
                "echo '{}  {}' | sha512sum -c --quiet -",
                deb.sha512, deb.path
            ),
        ]),
        v(&["apt-get", "install", "-y", deb.path]),
        v(&["rm", "-f", deb.path]),
    ]
}

/// The fallback for Caddy alone (`ensure_caddy_package`): Caddy's
/// release package, no distribution packages.
fn caddy_only_fallback_commands(deb: &CaddyDeb) -> Vec<Vec<String>> {
    let v = |a: &[&str]| {
        a.iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
    };
    let mut cmds = vec![
        v(&["rm", "-f", "/etc/apt/sources.list.d/caddy-stable.list"]),
        v(&["apt-get", "update"]),
    ];
    cmds.extend(caddy_deb_steps(deb));
    cmds
}

/// [`apt_commands`] with the last step installing Caddy alone (the
/// installer brings Postgres and the rest itself).
pub fn caddy_apt_commands() -> Vec<Vec<&'static str>> {
    let mut cmds = caddy_repo_steps();
    cmds.push(vec!["apt-get", "install", "-y", "caddy"]);
    cmds
}

/// Run `cmds`; when a step from the Caddy keyring on fails, run the
/// fallback that `fallback` builds for this host's pinned Caddy package
/// instead of the rest (ledger R116). The first two commands use only the
/// distribution archive: their failure is not Caddy's repository.
async fn apt_with_caddy_fallback(
    sys: &dyn SystemCommands,
    cmds: &[Vec<&str>],
    fallback: impl FnOnce(&CaddyDeb) -> Vec<Vec<String>>,
) -> Result<(), InstallError> {
    for (i, argv) in cmds.iter().enumerate() {
        if let Err(e) = sys.run(argv[0], &argv[1..]).await {
            if i < 2 {
                return Err(step("apt")(e));
            }
            let deb = match caddy_deb_for_arch(std::env::consts::ARCH) {
                Ok(deb) => deb,
                Err(no_fallback) => {
                    return Err(InstallError::Step("apt", format!("{e:#}; {no_fallback}")));
                }
            };
            tracing::warn!(error = %format!("{e:#}"), url = deb.url, "Caddy apt repository failed; installing Caddy's release package");
            for argv in fallback(deb) {
                let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
                sys.run(&argv[0], &args)
                    .await
                    .map_err(step("apt (caddy release package)"))?;
            }
            break;
        }
    }
    Ok(())
}

/// Install the `caddy` package unless dpkg already has it: the official
/// repository, else the verified release `.deb` (without the
/// distribution packages of [`caddy_deb_fallback_commands`]).
pub async fn ensure_caddy_package(sys: &dyn SystemCommands) -> Result<(), InstallError> {
    let status = sys
        .run("dpkg-query", &["-W", "-f=${Status}", "caddy"])
        .await
        .unwrap_or_default();
    if status.contains("install ok installed") {
        return Ok(());
    }
    apt_with_caddy_fallback(sys, &caddy_apt_commands(), caddy_only_fallback_commands).await
}

async fn ensure_packages(
    sys: &dyn SystemCommands,
    plan: &PackagePlan,
    fetch: &dyn KeyFetch,
) -> Result<(), InstallError> {
    // The tools the repository steps and the keyring fetch rely on come
    // first (0.1.2 M2): CA certificates for HTTPS, curl and gnupg for the
    // Caddy steps.
    sys.run(
        "apt-get",
        &["install", "-y", "ca-certificates", "curl", "gnupg"],
    )
    .await
    .map_err(step("apt packages"))?;
    if let Err(e) = crate::distro::ensure_repos(plan, sys, fetch).await {
        // A list written before the failure breaks every later
        // `apt-get update`; take it back out.
        let _ = sys.run("rm", &["-f", crate::distro::PGDG_LIST_PATH]).await;
        return Err(step("apt repositories")(e));
    }
    apt_with_caddy_fallback(sys, &apt_commands(plan), |deb| {
        caddy_deb_fallback_commands(plan, deb)
    })
    .await?;
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
/// Backups are the agent's own (`setu` 0750): `pg_dump` writes to
/// stdout and the agent writes the file (ledger R80).
pub const DIRS: &[(&str, &str, &str, &str)] = &[
    ("/var/lib/setu", "setu", "setu", "0750"),
    ("/var/lib/setu/downloads", "setu", "setu", "0750"),
    ("/var/lib/setu/backups", "setu", "setu", "0750"),
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

/// SQL piped to `psql` as `postgres` on a fresh box (spec §4 Postgres):
/// the `postgres` database always, the control plane's `setu` database
/// when it exists (single-node install).
pub const REVOKE_SQL: &str = "REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;\n\
SELECT 'REVOKE CONNECT ON DATABASE setu FROM PUBLIC' \
WHERE EXISTS (SELECT 1 FROM pg_database WHERE datname = 'setu')\\gexec\n";

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

#[allow(clippy::cast_possible_truncation)] // reviewed: kept as-is (R150)
fn cpu_cores() -> u32 {
    std::fs::read_to_string("/proc/cpuinfo")
        .map_or(1, |t| {
            t.lines().filter(|l| l.starts_with("processor")).count() as u32
        })
        .max(1)
}

fn memory_mb() -> u64 {
    crate::heartbeat::read_mem_total_mb().max(1)
}

/// POST the join once; on a network error (no HTTP answer) retry once
/// with the **same** request, so the same nonce (spec §5: a lost
/// response is recoverable within 60 s, a different nonce is refused).
pub async fn join_request(
    client: &reqwest::Client,
    base: &str,
    request: &JoinRequest,
) -> Result<JoinResponse, InstallError> {
    let send = || {
        client
            .post(format!("{base}/agent/v1/join"))
            .json(request)
            .timeout(std::time::Duration::from_secs(15))
            .send()
    };
    let resp = match send().await {
        Ok(r) => r,
        Err(first) => {
            tracing::warn!(error = %first, "join: network error, retrying with the same nonce");
            send()
                .await
                .map_err(|e| InstallError::Step("join", e.to_string()))?
        }
    };
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
    resp.json()
        .await
        .map_err(|e| InstallError::Step("join", e.to_string()))
}

/// Copy `--ca` next to the config (`ca.pem`, 0644: a public cert the
/// agent must read as `setu`) and return the copy's path.
fn install_ca(src: &Path, config_path: &Path) -> Result<PathBuf, InstallError> {
    use std::os::unix::fs::PermissionsExt as _;
    let err = |e: std::io::Error| InstallError::Step("ca", e.to_string());
    let pem = std::fs::read(src).map_err(err)?;
    reqwest::Certificate::from_pem(&pem).map_err(|e| InstallError::Step("ca", e.to_string()))?;
    let dest = config_path
        .parent()
        .unwrap_or_else(|| Path::new("/etc/setu"))
        .join("ca.pem");
    crate::config::atomic_write(&dest, &pem).map_err(err)?;
    std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o644)).map_err(err)?;
    Ok(dest)
}

async fn join_control_plane(opts: &InstallOpts) -> Result<(), InstallError> {
    let base = check_control_plane(&opts.control_plane, opts.insecure)?;
    let client = crate::config::http_client(opts.ca.as_deref()).map_err(step("ca"))?;
    let request = JoinRequest {
        // The token itself is never logged (proto Debug redacts).
        join_token: opts.join_token.clone(),
        // One nonce per install, reused by the retry.
        nonce: setu_common::random_hex(16),
        hostname: hostname(),
        arch: std::env::consts::ARCH.to_string(),
        os: std::env::consts::OS.to_string(),
        cpu_cores: cpu_cores(),
        memory_mb: memory_mb(),
        disk_mb: crate::heartbeat::disk_free_mb(Path::new("/")).await,
    };
    let joined = join_request(&client, &base, &request).await?;
    let path = opts
        .config_path
        .clone()
        .unwrap_or_else(AgentConfig::default_path);
    let config =
        AgentConfig::load(Some(&path)).map_err(|e| InstallError::Step("config", e.to_string()))?;
    let ca = match &opts.ca {
        Some(src) => Some(install_ca(src, &path)?),
        None => None,
    };
    let config = joined_config(config, base, joined, opts.insecure, ca, opts.panel.clone());
    // Created 0600 from the start (temp + rename), then handed to `setu`.
    config
        .save(&path)
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

    /// Review Focus 2: a re-join changes the identity only.
    #[test]
    fn joined_config_keeps_agent_toml_except_identity() {
        let existing = AgentConfig {
            control_plane: "https://panel.example.com".into(),
            server_id: "srv-old".into(),
            server_token: "setu_srv_old".into(),
            insecure: true,
            ca: Some(PathBuf::from("/etc/setu/ca.pem")),
            panel: Some(PanelRoute {
                host: "panel.example.com".into(),
                upstream: "127.0.0.1:7000".into(),
            }),
            reconcile_secs: 77,
            ..AgentConfig::default()
        };
        let joined = JoinResponse {
            server_id: "srv-old".into(),
            server_token: "setu_srv_new".into(),
        };
        let out = joined_config(
            existing.clone(),
            "https://panel.example.com".into(),
            joined,
            false,
            None,
            None,
        );
        assert_eq!(out.server_token, "setu_srv_new");
        assert_eq!(out.server_id, "srv-old");
        assert!(out.insecure, "never switched off by a re-join");
        assert_eq!(out.ca, existing.ca);
        assert_eq!(
            out.panel.as_ref().map(|p| p.host.as_str()),
            Some("panel.example.com")
        );
        assert_eq!(out.reconcile_secs, 77);
        assert_eq!(out.state_path, existing.state_path);
    }

    #[test]
    fn rejoin_control_plane_defaults_to_agent_toml() {
        let mut cfg = AgentConfig::default();
        assert!(
            rejoin_control_plane(None, &cfg).is_err(),
            "nothing joined yet"
        );
        cfg.server_id = "srv1".into();
        cfg.control_plane = "https://panel.example.com".into();
        assert_eq!(
            rejoin_control_plane(None, &cfg).unwrap(),
            "https://panel.example.com"
        );
        assert_eq!(
            rejoin_control_plane(Some("https://other.example.com".into()), &cfg).unwrap(),
            "https://other.example.com"
        );
    }

    /// I1: a re-join without `--ca` / `--insecure` keeps agent.toml's CA
    /// and insecure setting, both for the URL check and the HTTP client.
    #[test]
    fn rejoin_reuses_ca_and_insecure_from_agent_toml() {
        let existing = AgentConfig {
            control_plane: "http://panel.internal.example".into(),
            server_id: "srv1".into(),
            insecure: true,
            ca: Some(PathBuf::from("/nonexistent-setu-test/ca.pem")),
            ..AgentConfig::default()
        };
        let (insecure, ca) = rejoin_transport(false, None, &existing);
        assert!(insecure, "insecure is read from agent.toml");
        assert_eq!(ca, existing.ca, "a configured ca is never dropped");
        // The URL check accepts plain http only because of the kept flag.
        assert!(check_control_plane(&existing.control_plane, insecure).is_ok());
        assert!(check_control_plane(&existing.control_plane, false).is_err());
        // The client is built with the kept CA: a missing file fails there.
        let err = crate::config::http_client(ca.as_deref()).expect_err("ca is read");
        assert!(err.to_string().contains("/nonexistent-setu-test/ca.pem"));
        // Flags can only add to the file, never switch it off.
        let (insecure, ca) = rejoin_transport(
            false,
            Some(PathBuf::from("/x.pem")),
            &AgentConfig::default(),
        );
        assert!(!insecure);
        assert_eq!(ca, Some(PathBuf::from("/x.pem")));
    }

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

    /// The pinned Caddy package of the architecture the tests run on.
    fn host_caddy() -> &'static CaddyDeb {
        caddy_deb_for_arch(std::env::consts::ARCH).expect("the host has a Caddy package")
    }

    fn ubuntu_2404_plan() -> PackagePlan {
        plan_on(
            &Distro {
                id: "ubuntu".into(),
                version_id: "24.04".into(),
                codename: Some("noble".into()),
            },
            false,
        )
        .expect("supported")
    }

    /// Spec 0.1.2 §2 / Review Focus 3: an unsupported distribution is
    /// refused unless forced, and the forced plan is Debian 12's.
    #[test]
    fn unsupported_distribution_is_refused_unless_forced() {
        let fedora =
            Distro::parse(include_str!("../tests/fixtures/os-release/fedora-40")).expect("parses");
        let err = plan_on(&fedora, false).expect_err("refused");
        assert!(
            err.to_string()
                .contains("unsupported distribution fedora 40"),
            "{err}"
        );
        let forced = plan_on(&fedora, true).expect("forced");
        let debian12 = plan_on(
            &Distro::parse(include_str!("../tests/fixtures/os-release/debian-12")).expect("parses"),
            false,
        )
        .expect("supported");
        assert_eq!(forced, debian12);
    }

    #[test]
    fn apt_installs_the_plan_packages_on_debian_13() {
        let plan = plan_on(
            &Distro::parse(include_str!("../tests/fixtures/os-release/debian-13")).expect("parses"),
            false,
        )
        .expect("supported");
        let cmds = apt_commands(&plan);
        assert_eq!(
            cmds.last().map(|c| c.join(" ")),
            Some("apt-get install -y sudo gnupg ca-certificates curl postgresql-17 nftables zstd caddy".to_string())
        );
    }

    #[test]
    fn apt_adds_the_caddy_repo_before_installing_the_spec_packages() {
        let cmds = apt_commands(&ubuntu_2404_plan());
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
            "apt-get install -y sudo gnupg ca-certificates curl postgresql-16 nftables zstd caddy"
        );
    }

    #[test]
    fn caddy_fallback_package_matches_arch() {
        let amd = caddy_deb_for_arch("x86_64").expect("x86_64 has a package");
        assert!(
            amd.url.ends_with("/caddy_2.11.7_linux_amd64.deb"),
            "{}",
            amd.url
        );
        assert!(
            amd.path.ends_with("setu-caddy_2.11.7_amd64.deb"),
            "{}",
            amd.path
        );
        let arm = caddy_deb_for_arch("aarch64").expect("aarch64 has a package");
        assert!(
            arm.url.ends_with("/caddy_2.11.7_linux_arm64.deb"),
            "{}",
            arm.url
        );
        assert!(
            arm.path.ends_with("setu-caddy_2.11.7_arm64.deb"),
            "{}",
            arm.path
        );
        assert_eq!(amd.sha512.len(), 128);
        assert_eq!(arm.sha512.len(), 128);
        assert_ne!(amd.sha512, arm.sha512);
        let err = caddy_deb_for_arch("riscv64").expect_err("no riscv64 package");
        assert!(
            err.to_string()
                .contains("no Caddy fallback package for riscv64"),
            "{err}"
        );
    }

    /// Serves `bytes` as every keyring, after recording the fetch in the
    /// same command log as the commands.
    struct LoggingFetch {
        sys: crate::install_system::RecordingCommands,
        bytes: Vec<u8>,
    }

    #[async_trait::async_trait]
    impl crate::distro::KeyFetch for LoggingFetch {
        async fn fetch(&self, url: &str) -> anyhow::Result<Vec<u8>> {
            self.sys.run("fetch", &[url]).await?;
            Ok(self.bytes.clone())
        }
    }

    #[tokio::test]
    async fn ca_certificates_installed_before_pgdg_repo() {
        let sys = crate::install_system::RecordingCommands::new();
        let plan = plan_on(
            &Distro {
                id: "debian".into(),
                version_id: "12".into(),
                codename: Some("bookworm".into()),
            },
            false,
        )
        .expect("supported");
        let fetch = LoggingFetch {
            sys: sys.clone(),
            bytes: Vec::new(),
        };
        // The keyring does not verify, so the repository step fails; only
        // the order of the first commands matters here.
        assert!(ensure_packages(&sys, &plan, &fetch).await.is_err());
        let calls = sys.calls();
        let certs = calls
            .iter()
            .position(|c| c == "apt-get install -y ca-certificates curl gnupg")
            .expect("ca-certificates installed");
        let key = calls
            .iter()
            .position(|c| c == &format!("fetch {}", plan.repos[0].keyring_url))
            .expect("pgdg keyring fetched");
        assert!(certs < key, "{calls:?}");
    }

    #[tokio::test]
    async fn failed_repo_setup_removes_the_pgdg_list() {
        let sys = crate::install_system::RecordingCommands::new().failing_on("apt-get update");
        let mut plan = ubuntu_2404_plan();
        plan.repos = vec![crate::distro::AptRepo {
            list_line: "deb https://apt.example.test noble-pgdg main".into(),
            keyring_url: "https://keys.example.test/pgdg.asc".into(),
            // sha256 of b"key"
            keyring_sha256: "2c70e12b7a0646f92279f427c7b38e7334d8e5389cff167a1dc30e73f826b683",
            keyring_path: crate::distro::PGDG_KEYRING_PATH,
        }];
        let fetch = LoggingFetch {
            sys: sys.clone(),
            bytes: b"key".to_vec(),
        };
        assert!(ensure_packages(&sys, &plan, &fetch).await.is_err());
        let calls = sys.calls();
        let list = calls
            .iter()
            .position(|c| {
                c.starts_with("install -D -m 0644 ") && c.ends_with(crate::distro::PGDG_LIST_PATH)
            })
            .expect("pgdg list written");
        let removed = calls
            .iter()
            .position(|c| c == &format!("rm -f {}", crate::distro::PGDG_LIST_PATH))
            .expect("pgdg list removed after the failure");
        assert!(list < removed, "{calls:?}");
    }

    #[tokio::test]
    async fn caddy_repo_failure_falls_back_to_the_verified_release_package() {
        let sys = crate::install_system::RecordingCommands::new()
            .failing_on("dl.cloudsmith.io/public/caddy/stable/debian.deb.txt");
        ensure_packages(&sys, &ubuntu_2404_plan(), &crate::distro::HttpsFetch)
            .await
            .expect("fallback succeeds");
        let calls = sys.calls();
        let fail = calls
            .iter()
            .position(|c| c.contains("debian.deb.txt"))
            .expect("repo step ran");
        let rest = &calls[fail + 1..];
        assert_eq!(rest[0], "rm -f /etc/apt/sources.list.d/caddy-stable.list");
        assert!(rest.contains(
            &"apt-get install -y sudo gnupg ca-certificates curl postgresql-16 nftables zstd"
                .to_string()
        ));
        let get = rest
            .iter()
            .position(|c| c.starts_with("curl ") && c.ends_with(host_caddy().url))
            .expect("download");
        let check = rest
            .iter()
            .position(|c| c.contains("sha512sum -c") && c.contains(host_caddy().sha512))
            .expect("verify");
        let inst = rest
            .iter()
            .position(|c| c == &format!("apt-get install -y {}", host_caddy().path))
            .expect("install deb");
        assert!(get < check && check < inst, "{rest:?}");
        assert!(
            rest.iter()
                .all(|c| !c
                    .contains("install -y sudo gnupg ca-certificates curl postgresql-16 caddy"))
        );
        assert_eq!(
            calls.last().map(String::as_str),
            Some("systemctl enable --now postgresql")
        );
    }

    #[tokio::test]
    async fn caddy_package_is_installed_alone_when_missing() {
        let sys = crate::install_system::RecordingCommands::new();
        ensure_caddy_package(&sys).await.expect("installs");
        let calls = sys.calls();
        assert_eq!(calls[0], "dpkg-query -W -f=${Status} caddy");
        assert!(calls.iter().any(|c| c.contains("caddy-stable.list")));
        assert_eq!(
            calls.last().map(String::as_str),
            Some("apt-get install -y caddy")
        );
        assert!(calls.iter().all(|c| !c.contains("postgresql")));
    }

    #[tokio::test]
    async fn caddy_package_falls_back_to_the_release_deb_alone() {
        let sys = crate::install_system::RecordingCommands::new()
            .failing_on("dl.cloudsmith.io/public/caddy/stable/debian.deb.txt");
        ensure_caddy_package(&sys).await.expect("fallback succeeds");
        let calls = sys.calls();
        assert!(calls
            .iter()
            .any(|c| c == &format!("apt-get install -y {}", host_caddy().path)));
        assert!(calls.iter().all(|c| !c.contains("postgresql")));
    }

    #[tokio::test]
    async fn installed_caddy_package_is_left_alone() {
        let sys = crate::install_system::RecordingCommands::new()
            .answering("dpkg-query", "install ok installed");
        ensure_caddy_package(&sys).await.expect("no-op");
        assert_eq!(sys.calls(), vec!["dpkg-query -W -f=${Status} caddy"]);
    }

    #[tokio::test]
    async fn distribution_apt_failure_is_not_retried() {
        let sys = crate::install_system::RecordingCommands::new().failing_on("debian-keyring");
        assert!(
            ensure_packages(&sys, &ubuntu_2404_plan(), &crate::distro::HttpsFetch)
                .await
                .is_err()
        );
        assert!(sys.calls().iter().all(|c| !c.contains(host_caddy().url)));
    }

    #[test]
    fn revoke_sql_targets_the_postgres_and_setu_databases() {
        assert!(REVOKE_SQL.starts_with("REVOKE CONNECT ON DATABASE postgres FROM PUBLIC;\n"));
        assert!(REVOKE_SQL.contains("'REVOKE CONNECT ON DATABASE setu FROM PUBLIC'"));
        assert!(REVOKE_SQL.contains("datname = 'setu'"));
        assert!(REVOKE_SQL.trim_end().ends_with("\\gexec"));
    }

    #[test]
    fn install_accepts_loopback_http_without_insecure() {
        for ok in [
            "http://127.0.0.1:7000",
            "http://localhost:7000/",
            "http://[::1]:7000",
            "https://panel.example.com",
        ] {
            check_control_plane(ok, false).unwrap_or_else(|e| panic!("{ok}: {e}"));
        }
        assert_eq!(
            check_control_plane("http://127.0.0.1:7000/", false).expect("ok"),
            "http://127.0.0.1:7000"
        );
        for bad in [
            "http://10.0.0.5:7000",
            "http://panel.example.com",
            "ftp://x",
            "nope",
        ] {
            assert!(check_control_plane(bad, false).is_err(), "{bad}");
        }
        check_control_plane("http://10.0.0.5:7000", true).expect("--insecure");
    }

    #[test]
    fn join_token_line_is_trimmed_and_required() {
        assert_eq!(join_token_from("setu_join_x\n").expect("ok"), "setu_join_x");
        assert!(join_token_from("  \n").is_err());
        let opts = InstallOpts {
            control_plane: "https://x".into(),
            join_token: "setu_join_SECRET".into(),
            config_path: None,
            insecure: false,
            ca: None,
            tls_internal: false,
            panel: None,
            force_distro: false,
        };
        assert!(!format!("{opts:?}").contains("SECRET"));
    }

    #[test]
    fn dirs_are_spec_owned() {
        assert!(DIRS.contains(&("/var/lib/setu", "setu", "setu", "0750")));
        assert!(DIRS.contains(&("/srv/setu/sites", "root", "root", "0755")));
        assert!(DIRS.contains(&("/etc/setu", "root", "root", "0755")));
        assert!(DIRS.contains(&("/var/lib/setu/backups", "setu", "setu", "0750")));
    }
}
