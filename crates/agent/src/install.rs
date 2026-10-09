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

/// Caddy's own Debian package from its GitHub release, used when Caddy's
/// apt repository cannot be reached (Cloudsmith answered `402 Access to
/// the repository has been restricted` for every path in October 2026;
/// ledger R116). Pinned by the SHA-512 the release's
/// `caddy_<v>_checksums.txt` publishes; x86_64 only (the only release
/// target).
pub const CADDY_DEB_URL: &str =
    "https://github.com/caddyserver/caddy/releases/download/v2.11.7/caddy_2.11.7_linux_amd64.deb";
/// SHA-512 of [`CADDY_DEB_URL`].
pub const CADDY_DEB_SHA512: &str = "47e8351c2317b427af14a103e763ca1118a3d2396a88b4c0669cdec9c4a68a957690194e2423a1633f53135741c33a41bdac2b55515b7d0f7adc8b733add50d9";
const CADDY_DEB_PATH: &str = "/var/cache/apt/archives/setu-caddy_2.11.7_amd64.deb";

/// The fallback after a failed repository install: drop the Caddy list
/// (a failing source breaks every later `apt-get update`), install the
/// spec packages except Caddy from the distribution, then Caddy's
/// release `.deb`, verified before `apt-get` sees it.
pub fn caddy_deb_fallback_commands() -> Vec<Vec<String>> {
    let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    vec![
        v(&["rm", "-f", "/etc/apt/sources.list.d/caddy-stable.list"]),
        v(&["apt-get", "update"]),
        v(&[
            "apt-get",
            "install",
            "-y",
            "sudo",
            "gnupg",
            "postgresql-16",
            "nftables",
            "zstd",
        ]),
        v(&[
            "curl",
            "-fsSL",
            "--proto",
            "=https",
            "--retry",
            "3",
            "-o",
            CADDY_DEB_PATH,
            CADDY_DEB_URL,
        ]),
        v(&[
            "sh",
            "-c",
            &format!("echo '{CADDY_DEB_SHA512}  {CADDY_DEB_PATH}' | sha512sum -c --quiet -"),
        ]),
        v(&["apt-get", "install", "-y", CADDY_DEB_PATH]),
        v(&["rm", "-f", CADDY_DEB_PATH]),
    ]
}

async fn ensure_packages(sys: &dyn SystemCommands) -> Result<(), InstallError> {
    let cmds = apt_commands();
    for (i, argv) in cmds.iter().enumerate() {
        if let Err(e) = sys.run(argv[0], &argv[1..]).await {
            // The first two commands use only the distribution archive:
            // their failure is not Caddy's repository.
            if i < 2 || std::env::consts::ARCH != "x86_64" {
                return Err(step("apt")(e));
            }
            tracing::warn!(error = %format!("{e:#}"), "Caddy apt repository failed; installing Caddy's release package");
            eprintln!("setu: Caddy's apt repository failed ({e:#}); using {CADDY_DEB_URL}");
            for argv in caddy_deb_fallback_commands() {
                let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
                sys.run(&argv[0], &args)
                    .await
                    .map_err(step("apt (caddy release package)"))?;
            }
            break;
        }
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

fn cpu_cores() -> u32 {
    std::fs::read_to_string("/proc/cpuinfo")
        .map(|t| t.lines().filter(|l| l.starts_with("processor")).count() as u32)
        .unwrap_or(1)
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
    let mut config =
        AgentConfig::load(Some(&path)).map_err(|e| InstallError::Step("config", e.to_string()))?;
    config.control_plane = base;
    config.server_id = joined.server_id;
    config.server_token = joined.server_token;
    config.insecure = opts.insecure;
    if let Some(ca) = &opts.ca {
        config.ca = Some(install_ca(ca, &path)?);
    }
    if opts.panel.is_some() {
        config.panel = opts.panel.clone();
    }
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

    #[tokio::test]
    async fn caddy_repo_failure_falls_back_to_the_verified_release_package() {
        let sys = crate::install_system::RecordingCommands::new()
            .failing_on("dl.cloudsmith.io/public/caddy/stable/debian.deb.txt");
        ensure_packages(&sys).await.expect("fallback succeeds");
        let calls = sys.calls();
        let fail = calls
            .iter()
            .position(|c| c.contains("debian.deb.txt"))
            .expect("repo step ran");
        let rest = &calls[fail + 1..];
        assert_eq!(rest[0], "rm -f /etc/apt/sources.list.d/caddy-stable.list");
        assert!(
            rest.contains(&"apt-get install -y sudo gnupg postgresql-16 nftables zstd".to_string())
        );
        let get = rest
            .iter()
            .position(|c| c.starts_with("curl ") && c.ends_with(CADDY_DEB_URL))
            .expect("download");
        let check = rest
            .iter()
            .position(|c| c.contains("sha512sum -c") && c.contains(CADDY_DEB_SHA512))
            .expect("verify");
        let inst = rest
            .iter()
            .position(|c| c == &format!("apt-get install -y {CADDY_DEB_PATH}"))
            .expect("install deb");
        assert!(get < check && check < inst, "{rest:?}");
        assert!(rest
            .iter()
            .all(|c| !c.contains("install -y sudo gnupg postgresql-16 caddy")));
        assert_eq!(
            calls.last().map(String::as_str),
            Some("systemctl enable --now postgresql")
        );
    }

    #[tokio::test]
    async fn distribution_apt_failure_is_not_retried() {
        let sys = crate::install_system::RecordingCommands::new().failing_on("debian-keyring");
        assert!(ensure_packages(&sys).await.is_err());
        assert!(sys.calls().iter().all(|c| !c.contains(CADDY_DEB_URL)));
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
