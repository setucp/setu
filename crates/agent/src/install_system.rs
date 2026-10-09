//! `install_system` (spec §4): the system artefacts the agent and the
//! panel need, written as real root by `setu agent install`, `setu agent
//! install-system` and the installer. It is **not** a helper verb: the
//! sudoers rule only reaches `setu helper *`, so sudo never gets here.
//!
//! Writes (unless `caddy_only`) the site template unit, the agent unit,
//! `/etc/sudoers.d/setu` (0440, `visudo -cf` before the rename), the
//! firewall (`/etc/setu/nftables.conf` + `setu-nftables.service`, enabled
//! and restarted), and always `/etc/caddy/setu.json` plus the
//! `caddy-api.service` drop-in. Then: `systemctl disable --now
//! caddy.service`, `daemon-reload`, enable `caddy-api.service`.
//!
//! Caddy: when it already answers on the admin socket, the new base is
//! applied with `POST /load`, merged with the routes of the running
//! config so site routes survive (`--resume` would otherwise keep the
//! old base). When it does not, a foreign autosave (left by the distro
//! `caddy.service`, admin on TCP 2019) is moved aside, `caddy-api` is
//! restarted, and once the socket answers the same merged `/load` runs.
//!
//! Shelling out goes through [`SystemCommands`] so the whole function
//! runs unprivileged in tests against a tempdir `root` with
//! [`RecordingCommands`] and [`crate::caddy::FakeCaddy`].

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context as _;

use crate::caddy::{route_json, CaddyAdmin, CaddySocket, FakeAdmin, PanelRoute, PANEL_ROUTE_ID};

/// `setu-nftables.service` (golden).
pub const NFT_UNIT_ASSET: &str = include_str!("../assets/setu-nftables.service");
/// `/etc/setu/nftables.conf` (golden).
pub const NFT_CONF_ASSET: &str = include_str!("../assets/nftables.conf");
/// Base of `/etc/caddy/setu.json` (golden).
pub const CADDY_BASE_ASSET: &str = include_str!("../assets/caddy-base.json");
/// `caddy-api.service.d/10-setu.conf` (golden).
pub const CADDY_DROPIN_ASSET: &str = include_str!("../assets/caddy-api.dropin.conf");

const SITE_TEMPLATE: &str = "etc/systemd/system/setu-site@.service";
const AGENT_UNIT: &str = "etc/systemd/system/setu-agent.service";
const SUDOERS_DIR: &str = "etc/sudoers.d";
const NFT_CONF: &str = "etc/setu/nftables.conf";
const NFT_UNIT: &str = "etc/systemd/system/setu-nftables.service";
const CADDY_CONFIG: &str = "etc/caddy/setu.json";
const CADDY_DROPIN: &str = "etc/systemd/system/caddy-api.service.d/10-setu.conf";
/// Where the packaged Caddy (user `caddy`, home `/var/lib/caddy`) keeps
/// its autosave, which `--resume` prefers over `--config`.
const CADDY_AUTOSAVE: &str = "var/lib/caddy/.config/caddy/autosave.json";

/// The admin listen value of our base config.
const ADMIN_LISTEN: &str = "unix//run/caddy/admin.sock|0660";

/// Options for [`install_system`].
#[derive(Debug, Clone, Default)]
pub struct InstallSystemOpts {
    /// Prefix for every written path. Tests only: refused unless the
    /// crate is built with the `test-root` feature.
    pub root: Option<PathBuf>,
    /// Add Caddy's `internal` issuer policy (`--tls internal`).
    pub tls_internal: bool,
    /// The panel route (`--panel-host`, `--upstream`).
    pub panel: Option<PanelRoute>,
    /// Write only the Caddy config and drop-in (`--caddy-only`).
    pub caddy_only: bool,
}

/// Shell-outs `install_system` makes.
#[async_trait::async_trait]
pub trait SystemCommands: Send + Sync {
    /// Run `prog args…`; `Err` (first stderr line) on a non-zero exit.
    async fn run(&self, prog: &str, args: &[&str]) -> anyhow::Result<String>;
}

/// Real `tokio::process` commands.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealCommands;

#[async_trait::async_trait]
impl SystemCommands for RealCommands {
    async fn run(&self, prog: &str, args: &[&str]) -> anyhow::Result<String> {
        let out = tokio::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .await
            .with_context(|| format!("{prog}: cannot run"))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!(
                "{prog} {}: {}",
                args.first().copied().unwrap_or(""),
                stderr.lines().next().unwrap_or("failed")
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Records each command as one `prog arg…` line and succeeds. With
/// [`RecordingCommands::starting_caddy`], `systemctl restart
/// caddy-api.service` brings the given fake admin up, like the unit would.
#[derive(Debug, Clone, Default)]
pub struct RecordingCommands {
    calls: Arc<Mutex<Vec<String>>>,
    caddy: Option<FakeAdmin>,
    fail_on: Option<String>,
}

impl RecordingCommands {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn starting_caddy(mut self, admin: FakeAdmin) -> Self {
        self.caddy = Some(admin);
        self
    }

    /// Every command whose line contains `needle` fails (after being
    /// recorded).
    pub fn failing_on(mut self, needle: &str) -> Self {
        self.fail_on = Some(needle.to_string());
        self
    }

    pub fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[async_trait::async_trait]
impl SystemCommands for RecordingCommands {
    async fn run(&self, prog: &str, args: &[&str]) -> anyhow::Result<String> {
        let line = std::iter::once(prog)
            .chain(args.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        if line == "systemctl restart caddy-api.service" {
            if let Some(admin) = &self.caddy {
                admin.set_up(true);
            }
        }
        let fail = self.fail_on.as_deref().is_some_and(|n| line.contains(n));
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(line);
        if fail {
            anyhow::bail!("recorded failure");
        }
        Ok(String::new())
    }
}

/// The base Caddy config: the golden asset, plus the `internal` issuer
/// policy when `tls_internal`, plus the panel route when given.
pub fn caddy_base_config(
    tls_internal: bool,
    panel: Option<&PanelRoute>,
) -> anyhow::Result<serde_json::Value> {
    let mut config: serde_json::Value =
        serde_json::from_str(CADDY_BASE_ASSET).context("caddy-base.json")?;
    if tls_internal {
        config["apps"]["tls"] = serde_json::json!({
            "automation": { "policies": [ { "issuers": [ { "module": "internal" } ] } ] }
        });
    }
    if let Some(panel) = panel {
        let route = route_json(PANEL_ROUTE_ID, &panel.host, &panel.upstream)?;
        config["apps"]["http"]["servers"]["setu"]["routes"] = serde_json::json!([route]);
    }
    Ok(config)
}

/// `base` with the routes of the running config kept: every current
/// route whose `@id` the base does not define, in order, then the base's
/// own routes (the panel route, which replaces an older one).
pub fn merge_routes(
    mut base: serde_json::Value,
    current: Vec<serde_json::Value>,
) -> serde_json::Value {
    let routes = &mut base["apps"]["http"]["servers"]["setu"]["routes"];
    let base_routes: Vec<serde_json::Value> = routes.as_array().cloned().unwrap_or_default();
    let base_ids: Vec<&str> = base_routes
        .iter()
        .filter_map(|r| r.get("@id").and_then(|v| v.as_str()))
        .collect();
    let mut merged: Vec<serde_json::Value> = current
        .into_iter()
        .filter(|r| {
            r.get("@id")
                .and_then(|v| v.as_str())
                .is_none_or(|id| !base_ids.contains(&id))
        })
        .collect();
    merged.extend(base_routes.iter().cloned());
    *routes = serde_json::Value::Array(merged);
    base
}

/// Production entry: real commands and the default admin socket.
pub async fn install_system(opts: &InstallSystemOpts) -> anyhow::Result<()> {
    install_system_with(
        opts,
        &RealCommands,
        &CaddySocket::new(crate::caddy::DEFAULT_SOCKET),
    )
    .await
}

/// [`install_system`] over injected commands and Caddy client.
pub async fn install_system_with(
    opts: &InstallSystemOpts,
    sys: &dyn SystemCommands,
    caddy: &dyn CaddyAdmin,
) -> anyhow::Result<()> {
    if opts.root.is_some() && !cfg!(feature = "test-root") {
        anyhow::bail!("--root is only available in test builds");
    }
    let root = opts.root.clone().unwrap_or_else(|| PathBuf::from("/"));
    let at = |rel: &str| root.join(rel);
    let base = caddy_base_config(opts.tls_internal, opts.panel.as_ref())?;

    if !opts.caddy_only {
        write_file(
            &at(SITE_TEMPLATE),
            crate::install::SITE_TEMPLATE_ASSET.as_bytes(),
            0o644,
        )?;
        write_file(
            &at(AGENT_UNIT),
            crate::install::AGENT_UNIT_ASSET.as_bytes(),
            0o644,
        )?;
        write_sudoers(&at(SUDOERS_DIR), sys).await?;
        write_file(&at(NFT_CONF), NFT_CONF_ASSET.as_bytes(), 0o644)?;
        write_file(&at(NFT_UNIT), NFT_UNIT_ASSET.as_bytes(), 0o644)?;
    }
    let mut config_text = serde_json::to_string_pretty(&base)?;
    config_text.push('\n');
    write_file(&at(CADDY_CONFIG), config_text.as_bytes(), 0o644)?;
    write_file(&at(CADDY_DROPIN), CADDY_DROPIN_ASSET.as_bytes(), 0o644)?;

    if !opts.caddy_only {
        // Admin socket (group caddy) and site logs (systemd-journal)
        // without sudo.
        sys.run("usermod", &["-aG", "caddy,systemd-journal", "setu"])
            .await?;
    }
    sys.run("systemctl", &["daemon-reload"]).await?;
    if !opts.caddy_only {
        sys.run("systemctl", &["enable", "setu-nftables.service"])
            .await?;
        // `restart`, not `start`: a oneshot with RemainAfterExit stays
        // active, and a re-run must load the current table.
        sys.run("systemctl", &["restart", "setu-nftables.service"])
            .await?;
    }
    sys.run("systemctl", &["disable", "--now", "caddy.service"])
        .await?;
    sys.run("systemctl", &["enable", "caddy-api.service"])
        .await?;

    let current = match caddy.routes().await {
        Ok(routes) => routes,
        Err(_) => {
            set_aside_foreign_autosave(&at(CADDY_AUTOSAVE))?;
            sys.run("systemctl", &["restart", "caddy-api.service"])
                .await?;
            wait_for_caddy(caddy).await?
        }
    };
    caddy
        .load(&merge_routes(base, current))
        .await
        .context("apply the Caddy base config")?;
    Ok(())
}

/// Poll the admin socket after a restart (up to ~15 s).
async fn wait_for_caddy(caddy: &dyn CaddyAdmin) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut last = None;
    for _ in 0..30 {
        match caddy.routes().await {
            Ok(routes) => return Ok(routes),
            Err(e) => last = Some(e),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    Err(last
        .unwrap_or_else(|| anyhow::anyhow!("no answer"))
        .context("caddy-api did not open the admin socket"))
}

/// Move an autosave whose admin listener is not our socket out of the
/// way, so `caddy run --resume` starts from `/etc/caddy/setu.json`. Our
/// own autosave (site routes) is kept.
fn set_aside_foreign_autosave(path: &Path) -> anyhow::Result<()> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok(());
    };
    let listen = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .and_then(|v| v.pointer("/admin/listen")?.as_str().map(str::to_string));
    if listen.as_deref() == Some(ADMIN_LISTEN) {
        return Ok(());
    }
    let aside = path.with_extension("json.setu-foreign");
    std::fs::rename(path, &aside).with_context(|| format!("move aside {}", path.display()))
}

/// `/etc/sudoers.d/setu` 0440: a temp file (sudo ignores names with a
/// dot), `visudo -cf` on it, then the rename.
async fn write_sudoers(dir: &Path, sys: &dyn SystemCommands) -> anyhow::Result<()> {
    let tmp = dir.join(".setu.tmp");
    write_plain(&tmp, crate::install::SUDOERS_ASSET.as_bytes(), 0o440)?;
    let tmp_str = tmp.to_str().context("sudoers path is not UTF-8")?;
    if let Err(e) = sys.run("visudo", &["-cf", tmp_str]).await {
        let _ = std::fs::remove_file(&tmp);
        return Err(e.context("visudo rejected the sudoers file"));
    }
    std::fs::rename(&tmp, dir.join("setu")).context("install sudoers")
}

/// Temp file + rename with `mode`, parents created 0755.
fn write_file(path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .context("file name")?;
    let tmp = path.with_file_name(format!(".{name}.setu-tmp"));
    write_plain(&tmp, bytes, mode)?;
    std::fs::rename(&tmp, path).with_context(|| format!("install {}", path.display()))
}

fn write_plain(path: &Path, bytes: &[u8], mode: u32) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _, PermissionsExt as _};
    if let Some(parent) = path.parent() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o755)
            .create(parent)
            .with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let _ = std::fs::remove_file(path);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(path)
        .with_context(|| format!("write {}", path.display()))?;
    f.write_all(bytes)
        .with_context(|| format!("write {}", path.display()))?;
    // The umask may have narrowed `mode`; set it exactly.
    f.set_permissions(std::fs::Permissions::from_mode(mode))
        .with_context(|| format!("chmod {}", path.display()))?;
    f.sync_all().ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_keeps_current_routes_and_replaces_the_panel() {
        let panel = PanelRoute {
            host: "new.example.test".into(),
            upstream: "unix//run/setu/http.sock".into(),
        };
        let base = caddy_base_config(false, Some(&panel)).expect("base");
        let site = route_json("setu-site-abcd1234", "a.test", "127.0.0.1:20001").expect("site");
        let old_panel = route_json(
            PANEL_ROUTE_ID,
            "old.example.test",
            "unix//run/setu/http.sock",
        )
        .expect("p");
        let merged = merge_routes(base, vec![old_panel, site.clone()]);
        let routes = merged["apps"]["http"]["servers"]["setu"]["routes"]
            .as_array()
            .expect("routes");
        assert_eq!(routes.len(), 2);
        assert_eq!(routes[0], site);
        assert_eq!(routes[1]["match"][0]["host"][0], "new.example.test");
    }

    #[test]
    fn merge_without_panel_keeps_an_existing_panel_route() {
        let base = caddy_base_config(false, None).expect("base");
        let old_panel = route_json(
            PANEL_ROUTE_ID,
            "old.example.test",
            "unix//run/setu/http.sock",
        )
        .expect("p");
        let merged = merge_routes(base, vec![old_panel.clone()]);
        assert_eq!(
            merged["apps"]["http"]["servers"]["setu"]["routes"],
            serde_json::json!([old_panel])
        );
    }

    #[test]
    fn own_autosave_is_kept_and_foreign_moved_aside() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("autosave.json");
        std::fs::write(&path, CADDY_BASE_ASSET).expect("write");
        set_aside_foreign_autosave(&path).expect("own");
        assert!(path.exists());
        std::fs::write(&path, "{\"admin\":{\"listen\":\"localhost:2019\"}}").expect("write");
        set_aside_foreign_autosave(&path).expect("foreign");
        assert!(!path.exists());
        set_aside_foreign_autosave(&path).expect("missing is fine");
    }
}
