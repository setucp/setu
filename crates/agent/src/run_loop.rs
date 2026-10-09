//! Agent command loop: long-poll the control plane, handle commands,
//! post results, heartbeat every 15 s.
//!
//! Metrics sources (no new deps): `/proc/loadavg` (load1),
//! `/proc/meminfo` (`MemAvailable`, fallback `MemFree`), per-unit cgroup
//! files under `/sys/fs/cgroup/system.slice/setu-site@<id>.service/`
//! (`memory.current`, `cpu.stat`), per-site disk via `du -sb` cached for
//! 10 minutes. Host disk-free comes from `df -m` (a subprocess) rather
//! than a `statvfs` binding — Decision: avoids adding `libc`/`nix` for one
//! syscall; `df` output parsing isTotal-covered by a unit test.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use setu_proto::{CommandEnvelope, CommandResult, Heartbeat, SiteMetric, UnitState};

use crate::caddy::CaddyAdmin;
use crate::config::AgentConfig;
use crate::dispatch::handle;
use crate::postgres::PsqlCli;
use crate::runtime::NativeRuntime;
use crate::state::StateFile;
use crate::systemd::{unit_name, SystemctlCli, Systemd};

/// Production runtime wiring.
pub type ProdRuntime = NativeRuntime<SystemctlCli, PsqlCli, CaddyAdmin>;

/// Build the production runtime from config.
pub fn prod_runtime(config: &AgentConfig) -> ProdRuntime {
    NativeRuntime::new(
        SystemctlCli::new(config.unit_dir.clone()),
        PsqlCli::new(),
        CaddyAdmin::new(config.caddy_admin.clone()),
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

/// CPU seconds for a unit from `cpu.stat` (`usage_usec`), if present.
pub fn read_cpu_seconds(cgroup: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(cgroup.join("cpu.stat")).ok()?;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("usage_usec") {
            let usec: u64 = v.trim().parse().ok()?;
            return Some(usec as f64 / 1_000_000.0);
        }
    }
    None
}

/// Current memory bytes for a unit from `memory.current`, if present.
pub fn read_memory_bytes(cgroup: &Path) -> Option<u64> {
    std::fs::read_to_string(cgroup.join("memory.current"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// cgroup dir for a site unit:
/// `/sys/fs/cgroup/system.slice/setu-site@<id>.service`.
pub fn unit_cgroup_dir(site_id: &str) -> PathBuf {
    PathBuf::from("/sys/fs/cgroup/system.slice").join(format!("{}.service", unit_name(site_id)))
}

/// Disk usage cache: `du -sb` per site, refreshed at most every 10 min.
#[derive(Debug, Default)]
pub struct DiskCache {
    entries: HashMap<String, (u64, Instant)>,
}

impl DiskCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn disk_bytes(&mut self, site_id: &str, data_dir: &Path) -> u64 {
        if let Some((bytes, at)) = self.entries.get(site_id) {
            if at.elapsed() < Duration::from_secs(600) {
                return *bytes;
            }
        }
        let bytes = du_bytes(data_dir).await.unwrap_or(0);
        self.entries
            .insert(site_id.to_string(), (bytes, Instant::now()));
        bytes
    }
}

async fn du_bytes(path: &Path) -> Option<u64> {
    let out = tokio::process::Command::new("du")
        .arg("-sb")
        .arg(path)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()?
        .parse::<u64>()
        .ok()
}

/// Collect one heartbeat for `rt`: unit states from systemd, metrics from
/// cgroups + cached `du`.
pub async fn collect_heartbeat<S, P, C>(
    rt: &NativeRuntime<S, P, C>,
    disk_cache: &mut DiskCache,
) -> Heartbeat
where
    S: Systemd + Send + Sync,
    P: crate::postgres::PostgresAdmin + Send + Sync,
    C: crate::caddy::Caddy + Send + Sync,
{
    let state = StateFile::load(&rt.config().state_path).unwrap_or_default();
    let mut units = Vec::new();
    let mut metrics = Vec::new();
    for site_id in state.sites.keys() {
        let unit = unit_name(site_id);
        let (active, sub, pid) = match rt.systemd().show(&unit).await {
            Ok(p) => (p.active, p.sub, p.pid),
            Err(_) => (false, "unknown".to_string(), None),
        };
        units.push(UnitState {
            site_id: site_id.clone(),
            active,
            sub,
            pid,
        });
        let cg = unit_cgroup_dir(site_id);
        let data_dir = rt.config().sites_root.join(site_id).join("data");
        metrics.push(SiteMetric {
            site_id: site_id.clone(),
            at: chrono::Utc::now(),
            cpu_seconds: read_cpu_seconds(&cg).unwrap_or(0.0),
            memory_bytes: read_memory_bytes(&cg).unwrap_or(0),
            disk_bytes: disk_cache.disk_bytes(site_id, &data_dir).await,
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
    let mut disk_cache = DiskCache::new();
    let hb = collect_heartbeat(&rt, &mut disk_cache).await;
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
            let hb = collect_heartbeat(&rt, &mut disk_cache).await;
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

    #[test]
    fn cgroup_files_parse() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("cpu.stat"),
            "usage_usec 2500000\nuser_usec 1\n",
        )
        .expect("write");
        std::fs::write(dir.path().join("memory.current"), "134217728\n").expect("write");
        assert_eq!(read_cpu_seconds(dir.path()), Some(2.5));
        assert_eq!(read_memory_bytes(dir.path()), Some(134217728));
    }
}
