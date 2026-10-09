//! Agent command loop: long-poll the control plane, handle commands,
//! post results, heartbeat every 15 s.
//!
//! Metrics sources: `/proc/loadavg` (load1), `/proc/meminfo`
//! (`MemAvailable`, fallback `MemFree`), per-site unit state, cgroup
//! (from `ControlGroup`) and `data_bytes` through the helper's
//! `unit show` (see `metrics`). Host disk-free comes from `df -m`.

use std::path::Path;
use std::time::{Duration, Instant};

use setu_proto::{CommandEnvelope, CommandResult, Heartbeat, SiteMetric, UnitState};

use crate::caddy::CaddySocket;
use crate::config::AgentConfig;
use crate::dispatch::handle;
use crate::helper_client::{Helper, SudoHelper};
use crate::postgres::PsqlSudo;
use crate::runtime::NativeRuntime;
use crate::state::StateFile;

/// Production runtime wiring.
pub type ProdRuntime = NativeRuntime<SudoHelper, PsqlSudo, CaddySocket>;

/// Build the production runtime from config.
pub fn prod_runtime(config: &AgentConfig) -> ProdRuntime {
    NativeRuntime::new(
        SudoHelper::new(),
        PsqlSudo::system(),
        CaddySocket::new(&config.caddy_socket),
        config.clone(),
    )
}

/// Read the 1-minute load average from `/proc/loadavg`.
pub fn read_load1() -> f64 {
    read_load1_from(Path::new("/proc/loadavg")).unwrap_or(0.0)
}

fn read_load1_from(path: &Path) -> Option<f64> {
    std::fs::read_to_string(path)
        .ok()?
        .split_whitespace()
        .next()?
        .parse::<f64>()
        .ok()
}

/// Free memory (MiB) from `/proc/meminfo`: `MemAvailable`, else `MemFree`.
pub fn read_mem_free_mb() -> u64 {
    read_mem_from(Path::new("/proc/meminfo")).unwrap_or(0)
}

/// Total memory (MiB) from `/proc/meminfo` (`MemTotal`), for join reports.
pub fn read_mem_total_mb() -> u64 {
    std::fs::read_to_string(Path::new("/proc/meminfo"))
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| {
                line.strip_prefix("MemTotal:")
                    .and_then(first_kb)
                    .map(|kb| kb / 1024)
            })
        })
        .unwrap_or(0)
}

fn read_mem_from(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut available = None;
    let mut free = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("MemAvailable:") {
            available = first_kb(v);
        } else if let Some(v) = line.strip_prefix("MemFree:") {
            free = first_kb(v);
        }
    }
    available.or(free).map(|kb| kb / 1024)
}

fn first_kb(s: &str) -> Option<u64> {
    s.split_whitespace().next()?.parse::<u64>().ok()
}

/// Free disk (MiB) on the filesystem holding `path`, via `df -m`.
/// Decision: subprocess instead of a `statvfs` binding (no `libc`/`nix`
/// dep for one syscall); returns 0 when `df` is unavailable.
pub async fn disk_free_mb(path: &Path) -> u64 {
    let out = tokio::process::Command::new("df")
        .arg("-m")
        .arg("--output=avail")
        .arg(path)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => parse_df_avail(&String::from_utf8_lossy(&o.stdout)),
        _ => 0,
    }
}

fn parse_df_avail(text: &str) -> u64 {
    text.lines()
        .rev()
        .find_map(|l| l.trim().parse::<u64>().ok())
        .unwrap_or(0)
}

/// Collect one heartbeat for `rt`: unit state and metrics per site from
/// the helper's `unit show` plus the unit's cgroup files.
pub async fn collect_heartbeat<H, P, C>(rt: &NativeRuntime<H, P, C>) -> Heartbeat
where
    H: Helper,
    P: crate::postgres::PostgresAdmin + Send + Sync,
    C: crate::caddy::CaddyAdmin,
{
    let state = StateFile::load(&rt.config().state_path).unwrap_or_default();
    let mut units = Vec::new();
    let mut metrics = Vec::new();
    for site_id in state.sites.keys() {
        let status = match rt.status_inner(site_id).await {
            Ok(st) => st,
            Err(_) => crate::runtime::RuntimeStatus {
                sub: "unknown".to_string(),
                ..Default::default()
            },
        };
        let m = crate::metrics::read_site_metric(&status);
        units.push(UnitState {
            site_id: site_id.clone(),
            active: status.active,
            sub: status.sub,
            pid: status.pid,
        });
        metrics.push(SiteMetric {
            site_id: site_id.clone(),
            at: chrono::Utc::now(),
            cpu_seconds: m.cpu_seconds,
            memory_bytes: m.memory_bytes,
            disk_bytes: m.disk_bytes,
        });
    }
    Heartbeat {
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        load1: read_load1(),
        mem_free_mb: read_mem_free_mb(),
        disk_free_mb: disk_free_mb(&rt.config().sites_root).await,
        units,
        metrics,
    }
}

/// Run the agent: reconcile once, then long-poll + heartbeat until the
/// process is stopped. Reconnects with backoff 1 s → 30 s.
pub async fn run_forever(config: AgentConfig) -> anyhow::Result<()> {
    let rt = prod_runtime(&config);
    // Reconcile: report what we know; the control plane replays from there.
    let hb = collect_heartbeat(&rt).await;
    tracing::info!(
        units = hb.units.len(),
        load1 = hb.load1,
        "agent reconciled state"
    );

    let client = reqwest::Client::new();
    let base = config.control_plane_url.trim_end_matches('/').to_string();
    let mut backoff = Duration::from_secs(config.backoff_min_secs.max(1));
    let max_backoff = Duration::from_secs(config.backoff_max_secs.max(1));
    let mut last_heartbeat = Instant::now() - Duration::from_secs(3600);

    loop {
        // Heartbeat every `heartbeat_secs`, piggybacked on the loop.
        if last_heartbeat.elapsed() >= Duration::from_secs(config.heartbeat_secs.max(5)) {
            let hb = collect_heartbeat(&rt).await;
            if let Err(e) = post_heartbeat(&client, &base, &config, &hb).await {
                tracing::warn!(error = %e, "heartbeat failed");
            } else {
                last_heartbeat = Instant::now();
            }
        }

        match poll_command(&client, &base, &config).await {
            Ok(Some(envelope)) => {
                backoff = Duration::from_secs(config.backoff_min_secs.max(1));
                let result = handle(&envelope.id, &envelope.command, &rt).await;
                tracing::info!(cmd = %envelope.id, ok = result.ok, "handled command");
                if let Err(e) = post_result(&client, &base, &config, &result).await {
                    tracing::warn!(error = %e, cmd = %envelope.id, "result post failed");
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(error = %e, backoff_s = backoff.as_secs(), "poll failed");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(max_backoff);
            }
        }
    }
}

async fn poll_command(
    client: &reqwest::Client,
    base: &str,
    config: &AgentConfig,
) -> anyhow::Result<Option<CommandEnvelope>> {
    let resp = client
        .get(format!("{base}/agent/v1/commands"))
        .query(&[("wait", config.poll_wait_secs)])
        .bearer_auth(&config.server_token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("poll: {e}"))?;
    if resp.status() == reqwest::StatusCode::NO_CONTENT {
        return Ok(None);
    }
    if !resp.status().is_success() {
        anyhow::bail!("poll: HTTP {}", resp.status());
    }
    let envelope: CommandEnvelope = resp
        .json()
        .await
        .map_err(|e| anyhow::anyhow!("poll decode: {e}"))?;
    Ok(Some(envelope))
}

async fn post_result(
    client: &reqwest::Client,
    base: &str,
    config: &AgentConfig,
    result: &CommandResult,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{base}/agent/v1/commands/{}/result", result.id))
        .bearer_auth(&config.server_token)
        .json(result)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("result post: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("result post: HTTP {}", resp.status());
    }
    Ok(())
}

async fn post_heartbeat(
    client: &reqwest::Client,
    base: &str,
    config: &AgentConfig,
    hb: &Heartbeat,
) -> anyhow::Result<()> {
    let resp = client
        .post(format!("{base}/agent/v1/heartbeat"))
        .bearer_auth(&config.server_token)
        .json(hb)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("heartbeat: {e}"))?;
    if !resp.status().is_success() {
        anyhow::bail!("heartbeat: HTTP {}", resp.status());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loadavg_parses_first_field() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("loadavg");
        std::fs::write(&f, "0.42 0.31 0.15 1/200 1234\n").expect("write");
        assert_eq!(read_load1_from(&f), Some(0.42));
    }

    #[test]
    fn meminfo_prefers_available_then_free() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("meminfo");
        std::fs::write(
            &f,
            "MemTotal:        8192000 kB\nMemFree:          512000 kB\nMemAvailable:    1024000 kB\n",
        )
        .expect("write");
        assert_eq!(read_mem_from(&f), Some(1000));
        std::fs::write(&f, "MemTotal: 8192000 kB\nMemFree: 512000 kB\n").expect("write");
        assert_eq!(read_mem_from(&f), Some(500));
    }

    #[test]
    fn df_output_parses_last_number() {
        assert_eq!(parse_df_avail("Avail\n  123456\n"), 123456);
        assert_eq!(parse_df_avail("garbage"), 0);
    }
}
