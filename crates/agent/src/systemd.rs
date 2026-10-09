//! systemd adapter: a trait, the `sudo systemctl` impl, and an in-memory
//! fake for tests.
//!
//! Privileged unit/drop-in writes go through `setu helper write-file`
//! (pinned by path in sudoers); reads and lifecycle go through
//! `sudo systemctl`. The per-site drop-in carries the `ExecStart` override
//! plus the plan limits, so a plan change is a drop-in rewrite.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};

use setu_proto::{Limits, SiteSpec};

/// systemd unit name for a site.
pub fn unit_name(site_id: &str) -> String {
    format!("setu-site@{site_id}.service")
}

/// Drop-in path for a site under `unit_dir` (`/etc/systemd/system`).
pub fn dropin_path(unit_dir: &Path, site_id: &str) -> std::path::PathBuf {
    unit_dir
        .join(format!("{}.d", unit_name(site_id)))
        .join("override.conf")
}

/// Observed unit properties (`systemctl show`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitProps {
    pub active: bool,
    pub sub: String,
    pub pid: Option<u32>,
}

/// systemd surface the agent needs. Every method is idempotent-safe:
/// `start` on an active unit succeeds, `stop` on an inactive one succeeds.
#[async_trait::async_trait]
pub trait Systemd: Send + Sync {
    async fn daemon_reload(&self) -> anyhow::Result<()>;
    async fn start(&self, unit: &str) -> anyhow::Result<()>;
    async fn stop(&self, unit: &str) -> anyhow::Result<()>;
    async fn restart(&self, unit: &str) -> anyhow::Result<()>;
    async fn show(&self, unit: &str) -> anyhow::Result<UnitProps>;
    /// Write a full unit file; returns true when the bytes changed.
    async fn write_unit(&self, path: &Path, contents: &str) -> anyhow::Result<bool>;
    /// Write a per-site drop-in; returns true when the bytes changed.
    async fn write_dropin(&self, unit: &str, contents: &str) -> anyhow::Result<bool>;
    /// Remove a per-site drop-in (site delete).
    async fn remove_dropin(&self, unit: &str) -> anyhow::Result<()>;
    /// Last `lines` journal lines for a unit (oldest first).
    async fn fetch_logs(&self, unit: &str, lines: u32) -> anyhow::Result<Vec<String>>;
}

/// Render the per-site drop-in: `ExecStart` override plus plan limits.
///
/// Golden-tested: `MemoryMax=<mb>M`, `CPUQuota=<pct>%`.
pub fn render_dropin(site: &SiteSpec, sites_root: &Path) -> String {
    let site_dir = sites_root.join(&site.site_id);
    let mut cmd = site.exec.display().to_string();
    for arg in &site.args {
        cmd.push(' ');
        cmd.push_str(arg);
    }
    format!(
        "# Setu-managed drop-in for site {id}. Do not edit by hand.\n\
         [Service]\n\
         User={user}\n\
         WorkingDirectory={dir}\n\
         EnvironmentFile={env}\n\
         ExecStart=\n\
         ExecStart={cmd}\n\
         MemoryMax={mem}M\n\
         CPUQuota={cpu}%\n",
        id = site.site_id,
        user = site.user,
        dir = site_dir.display(),
        env = site.env_file.display(),
        cmd = cmd,
        mem = site.limits.memory_mb,
        cpu = site.limits.cpu_percent,
    )
}

/// Human memory from a `Limits` (kept next to the renderer for tests).
pub fn memory_max_line(limits: &Limits) -> String {
    format!("MemoryMax={}M", limits.memory_mb)
}

/// Production impl: every privileged operation goes through
/// `sudo <self-exe> helper …`, whose verbs, units and paths are
/// allowlisted in `helper.rs`. This keeps the sudoers file free of
/// argument wildcards (which `visudo` rejects) while scoping root to the
/// fixed helper surface.
#[derive(Debug, Clone)]
pub struct SystemctlCli {
    unit_dir: std::path::PathBuf,
}

impl SystemctlCli {
    pub fn new(unit_dir: std::path::PathBuf) -> Self {
        Self { unit_dir }
    }

    /// Run `sudo <self> helper <args…>`, piping `stdin` when given, and
    /// return stdout.
    async fn helper(&self, args: &[&str], stdin: Option<&str>) -> anyhow::Result<String> {
        use tokio::io::AsyncWriteExt;
        let exe =
            std::env::current_exe().map_err(|e| anyhow::anyhow!("cannot find own binary: {e}"))?;
        let mut child = tokio::process::Command::new("sudo")
            .arg(&exe)
            .arg("helper")
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| anyhow::anyhow!("cannot run helper: {e}"))?;
        if let Some(text) = stdin {
            if let Some(mut pipe) = child.stdin.take() {
                pipe.write_all(text.as_bytes())
                    .await
                    .map_err(|e| anyhow::anyhow!("cannot pipe to helper: {e}"))?;
            }
        }
        let out = child
            .wait_with_output()
            .await
            .map_err(|e| anyhow::anyhow!("helper failed: {e}"))?;
        if !out.status.success() {
            anyhow::bail!(
                "helper {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    async fn ctl(&self, verb: &str, unit: &str) -> anyhow::Result<()> {
        self.helper(&["systemctl", "--verb", verb, "--unit", unit], None)
            .await
            .map(|_| ())?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl Systemd for SystemctlCli {
    async fn daemon_reload(&self) -> anyhow::Result<()> {
        self.helper(&["systemctl", "--verb", "daemon-reload"], None)
            .await
            .map(|_| ())?;
        Ok(())
    }

    async fn start(&self, unit: &str) -> anyhow::Result<()> {
        self.ctl("start", unit).await
    }

    async fn stop(&self, unit: &str) -> anyhow::Result<()> {
        self.ctl("stop", unit).await
    }

    async fn restart(&self, unit: &str) -> anyhow::Result<()> {
        self.ctl("restart", unit).await
    }

    async fn show(&self, unit: &str) -> anyhow::Result<UnitProps> {
        let out = self
            .helper(&["systemctl", "--verb", "show", "--unit", unit], None)
            .await?;
        parse_show(&out)
    }

    async fn write_unit(&self, path: &Path, contents: &str) -> anyhow::Result<bool> {
        let changed = crate::helper::is_changed(path, contents.as_bytes());
        if changed {
            crate::helper::sudo_write_file(
                &std::env::current_exe()
                    .map_err(|e| anyhow::anyhow!("cannot find own binary: {e}"))?,
                path,
                contents.as_bytes(),
                "0644",
                None,
            )
            .await?;
            self.daemon_reload().await?;
        }
        Ok(changed)
    }

    async fn write_dropin(&self, unit: &str, contents: &str) -> anyhow::Result<bool> {
        let site_id = unit
            .strip_prefix("setu-site@")
            .and_then(|s| s.strip_suffix(".service"))
            .ok_or_else(|| anyhow::anyhow!("not a setu site unit: {unit}"))?;
        let path = dropin_path(&self.unit_dir, site_id);
        let changed = crate::helper::is_changed(&path, contents.as_bytes());
        if changed {
            crate::helper::sudo_write_file(
                &std::env::current_exe()
                    .map_err(|e| anyhow::anyhow!("cannot find own binary: {e}"))?,
                &path,
                contents.as_bytes(),
                "0644",
                None,
            )
            .await?;
            self.daemon_reload().await?;
        }
        Ok(changed)
    }

    async fn remove_dropin(&self, unit: &str) -> anyhow::Result<()> {
        self.helper(
            &[
                "remove-dropin",
                "--unit",
                unit,
                "--unit-dir",
                self.unit_dir.to_str().unwrap_or("/etc/systemd/system"),
            ],
            None,
        )
        .await
        .map(|_| ())?;
        Ok(())
    }

    async fn fetch_logs(&self, unit: &str, lines: u32) -> anyhow::Result<Vec<String>> {
        let out = self
            .helper(
                &[
                    "journal",
                    "--unit",
                    unit,
                    "--lines",
                    &lines.clamp(1, 5000).to_string(),
                ],
                None,
            )
            .await?;
        Ok(out.lines().map(str::to_string).collect())
    }
}

fn parse_show(text: &str) -> anyhow::Result<UnitProps> {
    let mut active_state = "";
    let mut sub = "";
    let mut pid: Option<u32> = None;
    for line in text.lines() {
        if let Some(v) = line.strip_prefix("ActiveState=") {
            active_state = v;
        } else if let Some(v) = line.strip_prefix("SubState=") {
            sub = v;
        } else if let Some(v) = line.strip_prefix("MainPID=") {
            pid = v.parse::<u32>().ok().filter(|p| *p != 0);
        }
    }
    Ok(UnitProps {
        active: active_state == "active",
        sub: sub.to_string(),
        pid,
    })
}

#[derive(Debug, Default)]
struct FakeInner {
    calls: Vec<String>,
    dropins: HashMap<String, String>,
    unit_files: HashMap<String, String>,
    active: HashSet<String>,
    daemon_reloads: usize,
    logs: HashMap<String, Vec<String>>,
}

/// In-memory `Systemd` for tests: records calls, tracks active units.
#[derive(Debug, Clone, Default)]
pub struct FakeSystemd {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeSystemd {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake systemd lock")
    }

    /// Call transcript (`"start <unit>"`, `"daemon-reload"`, …).
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    /// Last drop-in bytes written for a unit, if any.
    pub fn dropin_contents(&self, unit: &str) -> Option<String> {
        self.lock().dropins.get(unit).cloned()
    }

    /// Last full unit file written to a path, if any.
    pub fn unit_contents(&self, path: &Path) -> Option<String> {
        self.lock()
            .unit_files
            .get(&path.display().to_string())
            .cloned()
    }

    pub fn daemon_reload_count(&self) -> usize {
        self.lock().daemon_reloads
    }

    /// Pretend canned journal lines for a unit.
    pub fn set_logs(&self, unit: &str, lines: Vec<String>) {
        self.lock().logs.insert(unit.to_string(), lines);
    }
}

#[async_trait::async_trait]
impl Systemd for FakeSystemd {
    async fn daemon_reload(&self) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.daemon_reloads += 1;
        inner.calls.push("daemon-reload".to_string());
        Ok(())
    }

    async fn start(&self, unit: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("start {unit}"));
        inner.active.insert(unit.to_string());
        Ok(())
    }

    async fn stop(&self, unit: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("stop {unit}"));
        inner.active.remove(unit);
        Ok(())
    }

    async fn restart(&self, unit: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.calls.push(format!("restart {unit}"));
        inner.active.insert(unit.to_string());
        Ok(())
    }

    async fn show(&self, unit: &str) -> anyhow::Result<UnitProps> {
        let inner = self.lock();
        let active = inner.active.contains(unit);
        Ok(UnitProps {
            active,
            sub: if active {
                "running".to_string()
            } else {
                "dead".to_string()
            },
            pid: if active { Some(1234) } else { None },
        })
    }

    async fn write_unit(&self, path: &Path, contents: &str) -> anyhow::Result<bool> {
        let mut inner = self.lock();
        let key = path.display().to_string();
        if inner.unit_files.get(&key).is_some_and(|c| c == contents) {
            return Ok(false);
        }
        inner.unit_files.insert(key.clone(), contents.to_string());
        inner.calls.push(format!("write-unit {key}"));
        inner.daemon_reloads += 1;
        inner.calls.push("daemon-reload".to_string());
        Ok(true)
    }

    async fn write_dropin(&self, unit: &str, contents: &str) -> anyhow::Result<bool> {
        let mut inner = self.lock();
        if inner.dropins.get(unit).is_some_and(|c| c == contents) {
            return Ok(false);
        }
        inner.dropins.insert(unit.to_string(), contents.to_string());
        inner.calls.push(format!("write-dropin {unit}"));
        inner.daemon_reloads += 1;
        inner.calls.push("daemon-reload".to_string());
        Ok(true)
    }

    async fn remove_dropin(&self, unit: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.dropins.remove(unit);
        inner.calls.push(format!("remove-dropin {unit}"));
        Ok(())
    }

    async fn fetch_logs(&self, unit: &str, lines: u32) -> anyhow::Result<Vec<String>> {
        let inner = self.lock();
        let all = inner.logs.get(unit).cloned().unwrap_or_default();
        let n = lines as usize;
        Ok(if all.len() > n {
            all[all.len() - n..].to_vec()
        } else {
            all
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_show_active() {
        let p = parse_show("ActiveState=active\nSubState=running\nMainPID=42\n").expect("parse");
        assert!(p.active);
        assert_eq!(p.sub, "running");
        assert_eq!(p.pid, Some(42));
    }

    #[test]
    fn parse_show_inactive() {
        let p = parse_show("ActiveState=inactive\nSubState=dead\nMainPID=0\n").expect("parse");
        assert!(!p.active);
        assert_eq!(p.pid, None);
    }

    #[test]
    fn dropin_paths_are_per_site() {
        let unit_dir = Path::new("/etc/systemd/system");
        assert_eq!(
            dropin_path(unit_dir, "abc").to_str().expect("utf8"),
            "/etc/systemd/system/setu-site@abc.service.d/override.conf"
        );
    }
}
