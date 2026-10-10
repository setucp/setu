//! Heartbeat (spec §5): every 15 s, `boot_id`, load, free memory and
//! disk, unit states and per-site metrics.
//!
//! Metrics come from the helper's `unit show` (`MemoryCurrent`,
//! `CPUUsageNSec`, and `data_bytes`, which the helper measures as root);
//! the agent never runs `du` itself. Host figures: `/proc/loadavg`
//! (load1), `/proc/meminfo` (`MemAvailable`, fallback `MemFree`) and
//! `df -m` for disk.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use setu_proto::{Heartbeat, SiteMetric};

use crate::caddy::CaddyAdmin;
use crate::commands::Agent;
use crate::helper_client::Helper;
use crate::postgres::PostgresAdmin;
use crate::run_loop::ControlPlane;
use crate::runtime::NativeRuntime;

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

/// This agent process's boot id: the kernel's
/// `/proc/sys/kernel/random/boot_id` plus a nonce drawn once per process,
/// so both a host reboot and an agent restart change it (spec §5: the
/// control plane then requeues the server's delivered commands).
pub fn boot_id() -> String {
    static NONCE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let nonce = NONCE.get_or_init(setu_common::new_id);
    boot_id_from(Path::new("/proc/sys/kernel/random/boot_id"), nonce)
}

fn boot_id_from(kernel: &Path, nonce: &str) -> String {
    let kernel = std::fs::read_to_string(kernel)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if kernel.is_empty() {
        nonce.to_string()
    } else {
        format!("{kernel}-{nonce}")
    }
}

/// Collect one heartbeat for `rt`: unit state and metrics per site from
/// the helper's `unit show` plus the unit's cgroup files.
pub async fn collect_heartbeat<H, P, C>(rt: &NativeRuntime<H, P, C>) -> Heartbeat
where
    H: Helper,
    P: crate::postgres::PostgresAdmin + Send + Sync,
    C: crate::caddy::CaddyAdmin,
{
    let shorts: Vec<String> = rt.state().read(|st| st.sites.keys().cloned().collect());
    let mut units = Vec::new();
    let mut metrics = Vec::new();
    for short in &shorts {
        let status = match rt.status_inner(short).await {
            Ok(st) => st,
            Err(_) => crate::runtime::RuntimeStatus {
                sub: "unknown".to_string(),
                ..Default::default()
            },
        };
        let m = crate::metrics::read_site_metric(&status);
        units.push(setu_proto::UnitState {
            short_id: short.clone(),
            active: status.active,
            sub: status.sub,
            pid: status.pid,
            restarts: status.n_restarts,
        });
        metrics.push(SiteMetric {
            short_id: short.clone(),
            at: chrono::Utc::now(),
            cpu_seconds: m.cpu_seconds,
            memory_bytes: m.memory_bytes,
            disk_bytes: m.disk_bytes,
        });
    }
    Heartbeat {
        agent_version: env!("CARGO_PKG_VERSION").to_string(),
        boot_id: boot_id(),
        load1: read_load1(),
        mem_free_mb: read_mem_free_mb(),
        disk_free_mb: disk_free_mb(&rt.config().sites_root).await,
        units,
        metrics,
    }
}

/// Post a heartbeat every `every` until the task is dropped.
pub async fn run<H, P, C>(agent: Arc<Agent<H, P, C>>, cp: Arc<ControlPlane>, every: Duration)
where
    H: Helper,
    P: PostgresAdmin + Send + Sync,
    C: CaddyAdmin,
{
    loop {
        let hb = collect_heartbeat(agent.runtime()).await;
        if let Err(e) = cp.heartbeat(&hb).await {
            tracing::warn!(error = %e, "heartbeat failed");
        }
        tokio::time::sleep(every).await;
    }
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
    fn boot_id_joins_kernel_id_and_process_nonce() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f = dir.path().join("boot_id");
        std::fs::write(&f, "6f1c-kernel\n").expect("write");
        assert_eq!(boot_id_from(&f, "n1"), "6f1c-kernel-n1");
        assert_ne!(boot_id_from(&f, "n1"), boot_id_from(&f, "n2"));
        assert_eq!(boot_id_from(&dir.path().join("missing"), "n1"), "n1");
        assert_eq!(boot_id(), boot_id(), "stable within one process");
    }

    #[test]
    fn df_output_parses_last_number() {
        assert_eq!(parse_df_avail("Avail\n  123456\n"), 123456);
        assert_eq!(parse_df_avail("garbage"), 0);
    }
}
