//! `setu agent doctor` (spec §5): check every item `agent install` set
//! up, print a table, exit 1 on any failure.
//!
//! Rows: the helper binary (`/opt/setu/setu` root-owned 0755 in a
//! root-owned, not group/other-writable directory, so `setu` cannot
//! replace it), `visudo -cf /etc/sudoers.d/setu`, the template unit,
//! `caddy-api` active, its admin socket reachable, TCP 2019 closed,
//! `nft list table inet setu`, Postgres peer auth, the control plane's
//! `/healthz`, plus the state file, the server token and
//! `fs.protected_hardlinks = 1`. Everything
//! goes through [`SystemCommands`], [`CaddyAdmin`] and [`Probe`] so the
//! table is tested with fakes.

use std::path::Path;

use crate::caddy::{CaddyAdmin, CaddySocket};
use crate::config::AgentConfig;
use crate::helper_client::HELPER_PATH;
use crate::install::InstallError;
use crate::install_system::{RealCommands, SystemCommands};

/// The sudoers drop-in `visudo -cf` checks.
pub const SUDOERS_PATH: &str = "/etc/sudoers.d/setu";
/// The template unit `install_system` writes.
pub const SITE_TEMPLATE_PATH: &str = "/etc/systemd/system/setu-site@.service";

/// One `doctor` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DoctorRow {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
}

/// Owner, group and mode of a path (not following a final symlink).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    pub uid: u32,
    pub gid: u32,
    /// Permission bits incl. setuid/setgid/sticky (`& 0o7777`).
    pub mode: u32,
    pub is_file: bool,
    pub is_dir: bool,
}

/// Filesystem and network facts the doctor needs.
#[async_trait::async_trait]
pub trait Probe: Send + Sync {
    fn meta(&self, path: &Path) -> Option<FileMeta>;
    /// A sysctl's value (`fs.protected_hardlinks` → `/proc/sys/fs/…`).
    fn sysctl(&self, name: &str) -> Option<String>;
    async fn control_plane_ok(&self, config: &AgentConfig) -> bool;
    /// `/etc/os-release` text, when readable.
    fn os_release(&self) -> Option<String>;
}

/// The real host.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealProbe;

#[async_trait::async_trait]
impl Probe for RealProbe {
    fn meta(&self, path: &Path) -> Option<FileMeta> {
        use std::os::unix::fs::MetadataExt as _;
        let m = std::fs::symlink_metadata(path).ok()?;
        Some(FileMeta {
            uid: m.uid(),
            gid: m.gid(),
            mode: m.mode() & 0o7777,
            is_file: m.file_type().is_file(),
            is_dir: m.file_type().is_dir(),
        })
    }

    fn os_release(&self) -> Option<String> {
        std::fs::read_to_string("/etc/os-release").ok()
    }

    fn sysctl(&self, name: &str) -> Option<String> {
        if !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_')
        {
            return None;
        }
        let path = format!("/proc/sys/{}", name.replace('.', "/"));
        std::fs::read_to_string(path)
            .ok()
            .map(|v| v.trim().to_string())
    }

    async fn control_plane_ok(&self, config: &AgentConfig) -> bool {
        let Ok(client) = config.http_client() else {
            return false;
        };
        client
            .get(format!("{}/healthz", config.base_url()))
            .timeout(std::time::Duration::from_secs(5))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
    }
}

/// `/opt/setu/setu`: a regular file, root:root 0755, inside a
/// root-owned directory nobody else can write.
pub fn helper_is_safe(file: Option<FileMeta>, parent: Option<FileMeta>) -> bool {
    let file_ok = file.is_some_and(|m| m.is_file && m.uid == 0 && m.gid == 0 && m.mode == 0o755);
    let parent_ok = parent.is_some_and(|m| m.is_dir && m.uid == 0 && m.mode & 0o022 == 0);
    file_ok && parent_ok
}

/// Every check, in table order.
/// The `distro` check: this host's `os-release` names a supported
/// distribution (spec 0.1.2 §2).
fn distro_check(probe: &dyn Probe) -> (bool, String) {
    match probe.os_release().map(|t| crate::distro::Distro::parse(&t)) {
        Some(Ok(d)) => match crate::distro::plan_for(&d) {
            crate::distro::Support::Supported(_) => {
                (true, format!("supported {} {}", d.id, d.version_id))
            }
            crate::distro::Support::Unsupported { id, version } => {
                (false, format!("unsupported {id} {version}"))
            }
        },
        _ => (false, "cannot read /etc/os-release".to_string()),
    }
}

pub async fn doctor_rows(
    config: &AgentConfig,
    sys: &dyn SystemCommands,
    caddy: &dyn CaddyAdmin,
    probe: &dyn Probe,
) -> Vec<DoctorRow> {
    let mut rows = Vec::new();
    let mut row = |name: &'static str, ok: bool, detail: &str| {
        rows.push(DoctorRow {
            name,
            ok,
            detail: detail.to_string(),
        });
    };
    let helper = Path::new(HELPER_PATH);
    row(
        "helper",
        helper_is_safe(
            probe.meta(helper),
            helper.parent().and_then(|p| probe.meta(p)),
        ),
        "/opt/setu/setu root:root 0755, not writable by setu",
    );
    let (distro_ok, distro_detail) = distro_check(probe);
    row("distro", distro_ok, &distro_detail);
    row(
        "sudoers",
        sys.run("visudo", &["-cf", SUDOERS_PATH]).await.is_ok(),
        "visudo -cf /etc/sudoers.d/setu",
    );
    row(
        "site-template",
        probe
            .meta(Path::new(SITE_TEMPLATE_PATH))
            .is_some_and(|m| m.is_file),
        "setu-site@.service installed",
    );
    // Backups run as root over trees site users control; with
    // protected hardlinks a site user cannot hard-link a file it does not
    // own into its tree (the helper's backup also skips such files, M4).
    row(
        "protected-hardlinks",
        probe.sysctl("fs.protected_hardlinks").as_deref() == Some("1"),
        "sysctl fs.protected_hardlinks = 1",
    );
    row(
        "caddy-api",
        sys.run("systemctl", &["is-active", "--quiet", "caddy-api.service"])
            .await
            .is_ok(),
        "caddy-api.service active",
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
        "postgres",
        sys.run(
            "sudo",
            &["-n", "-u", "postgres", "psql", "-X", "-c", "select 1"],
        )
        .await
        .is_ok(),
        "peer auth: sudo -n -u postgres psql",
    );
    row(
        "state-file",
        crate::state::State::load(&config.state_path).is_ok(),
        &config.state_path.display().to_string(),
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
    row(
        "control-plane",
        probe.control_plane_ok(config).await,
        &format!("{}/healthz", config.base_url()),
    );
    rows
}

/// Print the table; `Err` (exit 1) when anything fails.
pub async fn doctor(config_path: Option<&Path>) -> Result<(), InstallError> {
    let config =
        AgentConfig::load(config_path).map_err(|e| InstallError::Step("config", e.to_string()))?;
    let rows = doctor_rows(
        &config,
        &RealCommands,
        &CaddySocket::new(&config.caddy_socket),
        &RealProbe,
    )
    .await;
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use super::*;
    use crate::caddy::FakeCaddy;

    /// Records commands; fails those whose line starts with a prefix.
    #[derive(Default)]
    struct Sys {
        fail: Vec<&'static str>,
        calls: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl SystemCommands for Sys {
        async fn run(&self, prog: &str, args: &[&str]) -> anyhow::Result<String> {
            let line = std::iter::once(prog)
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" ");
            self.calls.lock().expect("calls").push(line.clone());
            if self.fail.iter().any(|p| line.starts_with(p)) {
                anyhow::bail!("failed: {line}");
            }
            Ok(String::new())
        }
    }

    struct FakeProbe {
        metas: BTreeMap<&'static str, FileMeta>,
        sysctls: BTreeMap<&'static str, &'static str>,
        cp_ok: bool,
        os_release: Option<&'static str>,
    }

    #[async_trait::async_trait]
    impl Probe for FakeProbe {
        fn meta(&self, path: &Path) -> Option<FileMeta> {
            self.metas.get(path.to_str()?).copied()
        }
        fn sysctl(&self, name: &str) -> Option<String> {
            self.sysctls.get(name).map(|v| (*v).to_string())
        }
        async fn control_plane_ok(&self, _config: &AgentConfig) -> bool {
            self.cp_ok
        }
        fn os_release(&self) -> Option<String> {
            self.os_release.map(str::to_string)
        }
    }

    fn meta(uid: u32, mode: u32, is_dir: bool) -> FileMeta {
        FileMeta {
            uid,
            gid: uid,
            mode,
            is_file: !is_dir,
            is_dir,
        }
    }

    fn healthy() -> FakeProbe {
        FakeProbe {
            metas: BTreeMap::from([
                (HELPER_PATH, meta(0, 0o755, false)),
                ("/opt/setu", meta(0, 0o755, true)),
                (SITE_TEMPLATE_PATH, meta(0, 0o644, false)),
            ]),
            sysctls: BTreeMap::from([("fs.protected_hardlinks", "1")]),
            cp_ok: true,
            os_release: Some(include_str!("../tests/fixtures/os-release/ubuntu-24.04")),
        }
    }

    const NAMES: [&str; 13] = [
        "helper",
        "distro",
        "sudoers",
        "site-template",
        "protected-hardlinks",
        "caddy-api",
        "caddy-socket",
        "caddy-tcp-2019",
        "nftables",
        "postgres",
        "state-file",
        "server-token",
        "control-plane",
    ];

    #[tokio::test]
    async fn doctor_reports_each_check() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config = AgentConfig {
            state_path: dir.path().join("state.json"),
            server_token: "t".into(),
            ..AgentConfig::default()
        };
        let sys = Sys::default();
        let caddy = FakeCaddy::new();
        let rows = doctor_rows(&config, &sys, &caddy, &healthy()).await;
        let names: Vec<&str> = rows.iter().map(|r| r.name).collect();
        assert_eq!(names, NAMES);
        assert!(rows.iter().all(|r| r.ok), "{rows:?}");
        let calls = sys.calls.lock().expect("calls").clone();
        for want in [
            "visudo -cf /etc/sudoers.d/setu",
            "systemctl is-active --quiet caddy-api.service",
            "nft list table inet setu",
            "sudo -n -u postgres psql -X -c select 1",
        ] {
            assert!(calls.contains(&want.to_string()), "{want}: {calls:?}");
        }

        // Every check fails on its own fault.
        let sys = Sys {
            fail: vec!["visudo", "systemctl is-active", "nft", "sudo"],
            ..Sys::default()
        };
        caddy.admin().set_up(false);
        caddy.admin().set_tcp_admin_open(true);
        let mut probe = healthy();
        probe.cp_ok = false;
        probe.os_release = None;
        // Group-writable helper: `setu` could be in that group.
        probe.metas.insert(HELPER_PATH, meta(0, 0o775, false));
        probe.metas.remove(SITE_TEMPLATE_PATH);
        probe.sysctls.insert("fs.protected_hardlinks", "0");
        let config = AgentConfig {
            state_path: dir.path().join("state.json"),
            ..AgentConfig::default()
        };
        std::fs::write(&config.state_path, "not json").expect("write");
        let rows = doctor_rows(&config, &sys, &caddy, &probe).await;
        let failed: Vec<&str> = rows.iter().filter(|r| !r.ok).map(|r| r.name).collect();
        assert_eq!(failed, NAMES, "{rows:?}");
    }

    #[test]
    fn helper_must_be_root_0755_in_a_root_dir() {
        let dir_ok = Some(meta(0, 0o755, true));
        assert!(helper_is_safe(Some(meta(0, 0o755, false)), dir_ok));
        assert!(!helper_is_safe(Some(meta(999, 0o755, false)), dir_ok));
        assert!(!helper_is_safe(Some(meta(0, 0o4755, false)), dir_ok));
        assert!(!helper_is_safe(Some(meta(0, 0o757, false)), dir_ok));
        assert!(!helper_is_safe(
            Some(meta(0, 0o755, false)),
            Some(meta(0, 0o775, true))
        ));
        assert!(!helper_is_safe(
            Some(meta(0, 0o755, false)),
            Some(meta(999, 0o755, true))
        ));
        assert!(!helper_is_safe(None, dir_ok));
    }
}
