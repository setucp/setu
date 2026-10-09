//! `agent install` / `agent join` / `agent doctor`.
//!
//! Install runs once as root and is idempotent: user `setu`, dirs,
//! `postgresql-16` + `caddy` via apt when missing (never upgraded),
//! Caddy admin API on `127.0.0.1:2019`, sudoers from the golden asset,
//! `setu-agent.service`, join with the one-time token, then
//! `/etc/setu/agent.toml` (mode 0600).

use std::path::{Path, PathBuf};

use setu_proto::{JoinRequest, JoinResponse};

use crate::config::AgentConfig;

/// Golden sudoers, embedded so `install` and `check-sudoers.sh` agree.
pub const SUDOERS_ASSET: &str = include_str!("../assets/sudoers");

/// Template site unit, installed once by `install`.
pub const SITE_TEMPLATE_ASSET: &str = include_str!("../assets/setu-site@.service");

/// Agent's own unit.
pub const AGENT_UNIT_ASSET: &str = include_str!("../assets/setu-agent.service");

/// Options for `install` / `join`.
#[derive(Debug, Clone)]
pub struct InstallOpts {
    pub control_plane: String,
    pub join_token: String,
    pub config_path: Option<PathBuf>,
}

/// Errors from install/join/doctor.
#[derive(Debug, thiserror::Error)]
pub enum InstallError {
    #[error("must run as root (uid 0)")]
    NotRoot,
    #[error("step {0} failed: {1}")]
    Step(&'static str, String),
}

/// Full install: system setup, then join.
pub async fn install(opts: &InstallOpts) -> Result<(), InstallError> {
    require_root()?;
    system_setup().await?;
    join_control_plane(opts).await?;
    Ok(())
}

/// Join only (re-join a configured host).
pub async fn join(opts: &InstallOpts) -> Result<(), InstallError> {
    join_control_plane(opts).await
}

/// System setup without joining (dirs, packages, units, sudoers).
async fn system_setup() -> Result<(), InstallError> {
    ensure_user().await?;
    ensure_dirs().await?;
    ensure_packages().await?;
    ensure_caddy_admin().await?;
    write_sudoers().await?;
    write_units().await?;
    Ok(())
}

/// Print a dependency table; `Err` when anything fails.
pub async fn doctor(config_path: Option<&Path>) -> Result<(), InstallError> {
    let config =
        AgentConfig::load(config_path).map_err(|e| InstallError::Step("config", e.to_string()))?;
    let mut failures = 0;
    let mut row = |name: &str, ok: bool, detail: &str| {
        println!(
            "{:<14} {:<4} {detail}",
            name,
            if ok { "ok" } else { "FAIL" }
        );
        if !ok {
            failures += 1;
        }
    };

    row(
        "systemd",
        command_ok("systemctl", &["--version"]).await,
        "systemctl --version",
    );
    row(
        "postgres",
        command_ok("sudo", &["-u", "postgres", "psql", "-c", "SELECT 1"]).await,
        "sudo -u postgres psql",
    );
    let caddy_ok = reqwest::Client::new()
        .get(format!(
            "{}/config/",
            config.caddy_admin.trim_end_matches('/')
        ))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());
    row("caddy-admin", caddy_ok, &config.caddy_admin);
    let cp_ok = reqwest::Client::new()
        .get(format!(
            "{}/healthz",
            config.control_plane_url.trim_end_matches('/')
        ))
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .is_ok_and(|r| r.status().is_success());
    row("control-plane", cp_ok, &config.control_plane_url);
    let state_ok = crate::state::StateFile::load(&config.state_path).is_ok();
    row(
        "state-file",
        state_ok,
        &config.state_path.display().to_string(),
    );
    let unit_ok = Path::new("/etc/systemd/system/setu-site@.service").is_file();
    row("site-template", unit_ok, "setu-site@.service installed");
    row(
        "server-token",
        !config.server_token.is_empty(),
        if config.server_token.is_empty() {
            "missing: run `setu agent join`"
        } else {
            "present"
        },
    );

    if failures > 0 {
        return Err(InstallError::Step(
            "doctor",
            format!("{failures} checks failed"),
        ));
    }
    Ok(())
}

async fn command_ok(prog: &str, args: &[&str]) -> bool {
    tokio::process::Command::new(prog)
        .args(args)
        .output()
        .await
        .is_ok_and(|o| o.status.success())
}

fn require_root() -> Result<(), InstallError> {
    let out = std::process::Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| InstallError::Step("id -u", e.to_string()))?;
    if String::from_utf8_lossy(&out.stdout).trim() == "0" {
        Ok(())
    } else {
        Err(InstallError::NotRoot)
    }
}

async fn run(prog: &str, args: &[&str], step: &'static str) -> Result<String, InstallError> {
    let out = tokio::process::Command::new(prog)
        .args(args)
        .output()
        .await
        .map_err(|e| InstallError::Step(step, e.to_string()))?;
    if !out.status.success() {
        return Err(InstallError::Step(
            step,
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

async fn ensure_user() -> Result<(), InstallError> {
    // `id setu` succeeds when the user exists: create only then.
    if tokio::process::Command::new("id")
        .arg("setu")
        .output()
        .await
        .is_ok_and(|o| o.status.success())
    {
        return Ok(());
    }
    run(
        "useradd",
        &[
            "--system",
            "--home",
            "/var/lib/setu",
            "--shell",
            "/usr/sbin/nologin",
            "setu",
        ],
        "useradd setu",
    )
    .await?;
    Ok(())
}

async fn ensure_dirs() -> Result<(), InstallError> {
    for dir in [
        "/var/lib/setu",
        "/var/lib/setu/backups",
        "/srv/setu/sites",
        "/etc/setu",
    ] {
        run(
            "install",
            &["-d", "-o", "setu", "-g", "setu", "-m", "0755", dir],
            "dirs",
        )
        .await?;
    }
    Ok(())
}

fn package_missing(pkg: &str) -> bool {
    std::process::Command::new("dpkg")
        .args(["-s", pkg])
        .output()
        .is_ok_and(|o| !o.status.success())
}

/// apt-install missing packages only; never upgrades what is there.
/// Decision: a hosting agent must not surprise-upgrade Postgres/Caddy on
/// a live box; version bumps are an operator action.
async fn ensure_packages() -> Result<(), InstallError> {
    let mut missing = Vec::new();
    for pkg in ["postgresql-16", "caddy"] {
        if package_missing(pkg) {
            missing.push(pkg);
        }
    }
    if missing.is_empty() {
        return Ok(());
    }
    run("apt-get", &["update"], "apt-get update").await?;
    let mut args = vec!["install", "-y", "--no-upgrade"];
    args.extend(missing);
    run("apt-get", &args, "apt-get install").await?;
    Ok(())
}

/// Make sure Caddy's admin API listens on 127.0.0.1:2019 (best effort:
//  skipped when already reachable).
async fn ensure_caddy_admin() -> Result<(), InstallError> {
    let probe = reqwest::Client::new()
        .get("http://127.0.0.1:2019/config/")
        .timeout(std::time::Duration::from_secs(3))
        .send()
        .await;
    if probe.is_ok_and(|r| r.status().is_success()) {
        return Ok(());
    }
    // Point Caddy at a minimal config with the admin API enabled; the
    // agent manages routes through it from here on.
    let config = serde_json::json!({
        "admin": { "listen": "127.0.0.1:2019" },
        "apps": { "http": { "servers": { "srv0": {
            "listen": [":80", ":443"],
            "routes": [],
        } } } },
    });
    let status = reqwest::Client::new()
        .post("http://127.0.0.1:2019/load")
        .json(&config)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| InstallError::Step("caddy admin", e.to_string()))?;
    if !status.status().is_success() {
        return Err(InstallError::Step(
            "caddy admin",
            format!("HTTP {}", status.status()),
        ));
    }
    Ok(())
}

async fn write_sudoers() -> Result<(), InstallError> {
    // `visudo -cf` validates when available; otherwise the asset is
    // written as-is (it is the golden file CI checks).
    let path = "/etc/sudoers.d/setu";
    std::fs::write(path, SUDOERS_ASSET)
        .map_err(|e| InstallError::Step("sudoers", e.to_string()))?;
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o440))
            .map_err(|e| InstallError::Step("sudoers", e.to_string()))?;
    }
    if command_ok("visudo", &["-cf", path]).await {
        Ok(())
    } else if which("visudo") {
        Err(InstallError::Step(
            "sudoers",
            "visudo -cf rejected the file".to_string(),
        ))
    } else {
        Ok(())
    }
}

fn which(prog: &str) -> bool {
    std::process::Command::new("which")
        .arg(prog)
        .output()
        .is_ok_and(|o| o.status.success())
}

async fn write_units() -> Result<(), InstallError> {
    std::fs::write(
        "/etc/systemd/system/setu-site@.service",
        SITE_TEMPLATE_ASSET,
    )
    .map_err(|e| InstallError::Step("site template", e.to_string()))?;
    let agent_unit = AGENT_UNIT_ASSET;
    std::fs::write("/etc/systemd/system/setu-agent.service", agent_unit)
        .map_err(|e| InstallError::Step("agent unit", e.to_string()))?;
    run("systemctl", &["daemon-reload"], "daemon-reload").await?;
    run(
        "systemctl",
        &["enable", "--now", "setu-agent.service"],
        "enable agent",
    )
    .await?;
    Ok(())
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
    config
        .save(&path)
        .map_err(|e| InstallError::Step("config", e.to_string()))?;
    // Token file must not be world-readable.
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| InstallError::Step("config", e.to_string()))?;
    println!("joined as server {}", config.server_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sudoers_asset_has_required_entries() {
        // The whole root surface is the pinned helper (no argument
        // wildcards: sudoers forbids them, so units are allowlisted in
        // the helper itself). Postgres admin is per-binary, postgres user.
        for needle in [
            "setu ALL=(root) NOPASSWD: /opt/setu/setu helper",
            "setu ALL=(postgres) NOPASSWD: /usr/bin/psql",
            "setu ALL=(postgres) NOPASSWD: /usr/bin/pg_dump",
            "setu ALL=(postgres) NOPASSWD: /usr/bin/pg_restore",
        ] {
            assert!(
                SUDOERS_ASSET.contains(needle),
                "sudoers asset must pin `{needle}`"
            );
        }
        // No wildcards anywhere (rejected by visudo in argument position,
        // and a sign of an overbroad rule everywhere else) and no
        // blanket shell/editor/package access or direct systemctl and
        // journalctl rules (those go through the helper). Comments are
        // prose; only rule lines count.
        let rules: Vec<&str> = SUDOERS_ASSET
            .lines()
            .filter(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with('#')
            })
            .collect();
        for bad in [
            "*",
            "/bin/sh",
            "/bin/bash",
            "/usr/bin/apt",
            "systemctl",
            "journalctl",
            "rm ",
        ] {
            assert!(
                !rules.iter().any(|l| l.contains(bad)),
                "sudoers rules must not contain `{bad}`"
            );
        }
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
    }
}
