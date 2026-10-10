//! Per-site metrics from the unit's cgroup. The cgroup path comes from
//! `systemctl show -p ControlGroup` (instances live under
//! `system-setu\x2dsite.slice`), never guessed (spec §4).

use std::path::{Component, Path, PathBuf};

/// `/sys/fs/cgroup` + `ControlGroup`; `None` for an empty or odd value.
pub fn cgroup_path(control_group: &str) -> Option<PathBuf> {
    let cg = Path::new(control_group);
    if !control_group.starts_with('/')
        || cg
            .components()
            .any(|c| matches!(c, Component::ParentDir | Component::CurDir))
    {
        return None;
    }
    Some(Path::new("/sys/fs/cgroup").join(control_group.trim_start_matches('/')))
}

/// CPU seconds from `cpu.stat` (`usage_usec`).
pub fn read_cpu_seconds(cgroup: &Path) -> Option<f64> {
    let text = std::fs::read_to_string(cgroup.join("cpu.stat")).ok()?;
    text.lines().find_map(|line| {
        let v = line.strip_prefix("usage_usec")?;
        let usec: u64 = v.trim().parse().ok()?;
        Some(usec as f64 / 1_000_000.0)
    })
}

/// Bytes from `memory.current`.
pub fn read_memory_bytes(cgroup: &Path) -> Option<u64> {
    std::fs::read_to_string(cgroup.join("memory.current"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// One site's metric: the helper's `unit show` values (`CPUUsageNSec`,
/// `MemoryCurrent`, `data_bytes`) first, the unit's cgroup files (read
/// as the agent, never `du`) when systemd does not track a value.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SiteMetric {
    pub cpu_seconds: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

pub fn read_site_metric(status: &crate::runtime::RuntimeStatus) -> SiteMetric {
    let cg = status.control_group.as_deref().and_then(cgroup_path);
    let cpu = status
        .cpu_nsec
        .map(|n| n as f64 / 1e9)
        .or_else(|| cg.as_deref().and_then(read_cpu_seconds))
        .unwrap_or(0.0);
    let mem = status
        .memory_bytes
        .or_else(|| cg.as_deref().and_then(read_memory_bytes))
        .unwrap_or(0);
    SiteMetric {
        cpu_seconds: cpu,
        memory_bytes: mem,
        disk_bytes: status.data_bytes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cgroup_path_handles_escaped_slice() {
        // The revision 1 bug appended `.service` to a unit name that
        // already had it; the path now comes from ControlGroup verbatim.
        let cg = "/system.slice/system-setu\\x2dsite.slice/setu-site@abcd1234.service";
        assert_eq!(
            cgroup_path(cg).expect("path"),
            PathBuf::from(
                "/sys/fs/cgroup/system.slice/system-setu\\x2dsite.slice/setu-site@abcd1234.service"
            )
        );
        assert!(cgroup_path("").is_none());
        assert!(cgroup_path("relative/x").is_none());
        assert!(cgroup_path("/system.slice/../../etc").is_none());
    }

    #[test]
    fn show_values_win_with_cgroup_fallback() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("cpu.stat"), "usage_usec 2500000\n").expect("write");
        std::fs::write(dir.path().join("memory.current"), "134217728\n").expect("write");
        assert_eq!(read_cpu_seconds(dir.path()), Some(2.5));
        assert_eq!(read_memory_bytes(dir.path()), Some(134217728));
        let st = crate::runtime::RuntimeStatus {
            control_group: Some("/nonexistent.slice/x.service".into()),
            cpu_nsec: Some(3_000_000_000),
            memory_bytes: Some(42),
            data_bytes: 7,
            ..Default::default()
        };
        let m = read_site_metric(&st);
        assert_eq!((m.cpu_seconds, m.memory_bytes, m.disk_bytes), (3.0, 42, 7));
        let st = crate::runtime::RuntimeStatus {
            control_group: Some("/x.service".into()),
            ..Default::default()
        };
        assert_eq!(
            read_site_metric(&st).memory_bytes,
            0,
            "no show value, no cgroup"
        );
    }
}
