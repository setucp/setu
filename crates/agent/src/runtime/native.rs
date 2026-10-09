//! `NativeRuntime`: the phase-1 `Runtime` over the three adapters.
//!
//! Layout per site (`<sites_root>/<id>/`): `bin/<version>/` + `bin/current`
//! symlink, `data/`, `run/`, `env` (0600). The systemd template unit
//! (`setu-site@.service`, installed once by `agent install`) provides the
//! hardening; the per-site drop-in written by `write_unit` overrides
//! `ExecStart` plus the plan limits.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use setu_proto::{EnvVar, SiteSpec};

use crate::caddy::Caddy;
use crate::config::AgentConfig;
use crate::postgres::PostgresAdmin;
use crate::runtime::{Cursor, LogStream, Runtime, RuntimeStatus, SiteId};
use crate::state::{SiteEntry, StateFile};
use crate::systemd::{self, Systemd};

/// Phase-1 runtime: app binaries supervised as systemd template units.
#[derive(Debug, Clone)]
pub struct NativeRuntime<S, P, C> {
    systemd: S,
    postgres: P,
    caddy: C,
    config: AgentConfig,
}

impl<S, P, C> NativeRuntime<S, P, C> {
    pub fn new(systemd: S, postgres: P, caddy: C, config: AgentConfig) -> Self {
        Self {
            systemd,
            postgres,
            caddy,
            config,
        }
    }

    pub fn systemd(&self) -> &S {
        &self.systemd
    }

    pub fn postgres(&self) -> &P {
        &self.postgres
    }

    pub fn caddy(&self) -> &C {
        &self.caddy
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    /// Site working dir (`<sites_root>/<id>`).
    pub fn site_dir(&self, site_id: &str) -> PathBuf {
        self.config.sites_root.join(site_id)
    }

    fn unit(&self, site_id: &str) -> String {
        systemd::unit_name(site_id)
    }

    fn load_state(&self) -> Result<StateFile, crate::state::StateError> {
        StateFile::load(&self.config.state_path)
    }

    /// Ensure the site's directories exist.
    pub fn ensure_dirs(&self, site: &SiteSpec) -> anyhow::Result<()> {
        for dir in [
            self.site_dir(&site.site_id),
            self.site_dir(&site.site_id).join("bin"),
            site.data_dir.clone(),
            site.run_dir.clone(),
        ] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dir.display()))?;
        }
        Ok(())
    }

    /// Write one privileged file: via the sudo helper in production
    /// (which sets mode + owner as root), directly in tests
    /// (`use_sudo_helper = false`).
    async fn privileged_write(
        &self,
        path: &Path,
        contents: &[u8],
        mode: &str,
        owner: Option<&str>,
    ) -> anyhow::Result<()> {
        if self.config.use_sudo_helper {
            let exe = std::env::current_exe()
                .map_err(|e| anyhow::anyhow!("cannot find own binary: {e}"))?;
            crate::helper::sudo_write_file(&exe, path, contents, mode, owner).await
        } else {
            crate::helper::write_file_with_roots(
                path,
                contents,
                mode,
                owner,
                &crate::helper::test_roots(),
            )
            .map_err(|e| anyhow::anyhow!("{e}"))
        }
    }

    /// Install the site's binary version (download + verify + extract).
    /// No-op when `site.binary` is absent or the version is installed.
    pub async fn install_version(&self, site: &SiteSpec) -> anyhow::Result<PathBuf>
    where
        S: Systemd,
    {
        self.ensure_dirs(site)?;
        let Some(binary) = site.binary.as_ref() else {
            return Ok(crate::download::bin_dir(
                &self.config.sites_root,
                &site.site_id,
            ));
        };
        let dest = crate::download::fetch_and_install(
            &binary.url,
            &binary.sha256,
            &binary.exec,
            &self.config.sites_root,
            &site.site_id,
            &binary.version,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        // First provision points `current` at the new version; updates
        // leave it alone (the explicit `SwitchVersion` step moves it, so a
        // bad update never stops the running site).
        let current =
            crate::download::bin_dir(&self.config.sites_root, &site.site_id).join("current");
        if std::fs::read_link(&current).is_err() {
            crate::download::switch_current(
                &self.config.sites_root,
                &site.site_id,
                &binary.version,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        let mut state = self.load_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        // The recorded version is the *running* version: set on first
        // install, moved only by `switch_version_inner`. Installing a new
        // version beside the old (update flow) leaves it alone.
        let entry = state
            .sites
            .get(&site.site_id)
            .cloned()
            .unwrap_or(SiteEntry {
                port: site.port,
                db: None,
                version: binary.version.clone(),
            });
        state
            .upsert(
                &site.site_id,
                SiteEntry {
                    port: site.port,
                    db: entry.db,
                    version: entry.version,
                },
                &self.config.state_path,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(dest)
    }

    /// Write the per-site drop-in (ExecStart override + limits).
    /// Returns true when the bytes changed (daemon-reload happened).
    pub async fn write_unit_inner(&self, site: &SiteSpec) -> anyhow::Result<bool>
    where
        S: Systemd,
    {
        self.ensure_dirs(site)?;
        // The drop-in runs the unit as `site.user` and the env write
        // chowns to it: the user must exist before either lands.
        self.systemd.ensure_user(&site.user).await?;
        let contents = systemd::render_dropin(site, &self.config.sites_root);
        let changed = self
            .systemd
            .write_dropin(&self.unit(&site.site_id), &contents)
            .await?;
        let mut state = self.load_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        let entry = state
            .sites
            .get(&site.site_id)
            .cloned()
            .unwrap_or(SiteEntry {
                port: site.port,
                db: None,
                version: String::new(),
            });
        state
            .upsert(
                &site.site_id,
                SiteEntry {
                    port: site.port,
                    db: entry.db,
                    version: entry.version,
                },
                &self.config.state_path,
            )
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(changed)
    }

    /// Write the env file (mode 0600, site user when privileged).
    pub async fn write_env_inner(&self, site: &SiteSpec, env: &[EnvVar]) -> anyhow::Result<()>
    where
        S: Systemd,
    {
        self.ensure_dirs(site)?;
        let contents = render_env(env);
        if let Some(parent) = site.env_file.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| anyhow::anyhow!("cannot create env dir: {e}"))?;
        }
        self.privileged_write(
            &site.env_file,
            contents.as_bytes(),
            "0600",
            self.config.chown_files.then_some(site.user.as_str()),
        )
        .await?;
        // Belt and braces for the direct-write path: the helper path sets
        // mode itself, but a plain write inherits the umask.
        #[cfg(unix)]
        {
            let perm = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&site.env_file, perm);
        }
        Ok(())
    }

    pub async fn start_inner(&self, site_id: &str) -> anyhow::Result<()>
    where
        S: Systemd,
    {
        self.systemd.start(&self.unit(site_id)).await
    }

    pub async fn stop_inner(&self, site_id: &str) -> anyhow::Result<()>
    where
        S: Systemd,
    {
        self.systemd.stop(&self.unit(site_id)).await
    }

    pub async fn restart_inner(&self, site_id: &str) -> anyhow::Result<()>
    where
        S: Systemd,
    {
        self.systemd.restart(&self.unit(site_id)).await
    }

    pub async fn status_inner(&self, site_id: &str) -> anyhow::Result<RuntimeStatus>
    where
        S: Systemd,
    {
        let props = self.systemd.show(&self.unit(site_id)).await?;
        Ok(RuntimeStatus {
            active: props.active,
            sub: props.sub,
            pid: props.pid,
        })
    }

    pub async fn logs_inner(&self, site_id: &str, lines: u32) -> anyhow::Result<Vec<String>>
    where
        S: Systemd,
    {
        self.systemd.fetch_logs(&self.unit(site_id), lines).await
    }

    /// Stop, drop the unit drop-in, delete the site dir and state entry.
    /// The database is dropped by the separate `DropDatabase` command so a
    /// failed delete can be retried piece by piece.
    pub async fn remove_inner(&self, site: &SiteSpec) -> anyhow::Result<()>
    where
        S: Systemd,
    {
        let _ = self.stop_inner(&site.site_id).await;
        let _ = self.systemd.remove_dropin(&self.unit(&site.site_id)).await;
        let dir = self.site_dir(&site.site_id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)
                .map_err(|e| anyhow::anyhow!("cannot remove {}: {e}", dir.display()))?;
        }
        // After the unit is stopped no site processes remain, so the user
        // owns nothing and `userdel` succeeds (missing user is a no-op).
        self.systemd.remove_user(&site.user).await?;
        let mut state = self.load_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        state
            .remove(&site.site_id, &self.config.state_path)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }

    pub async fn switch_version_inner(&self, site_id: &str, version: &str) -> anyhow::Result<bool> {
        let changed = crate::download::switch_current(&self.config.sites_root, site_id, version)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let mut state = self.load_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        if let Some(mut entry) = state.sites.get(site_id).cloned() {
            entry.version = version.to_string();
            state
                .upsert(site_id, entry, &self.config.state_path)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(changed)
    }

    /// Poll `http://127.0.0.1:<port><path>` until 2xx or `timeout`.
    pub async fn health_inner(
        &self,
        site_id: &str,
        port: u16,
        path: &str,
        timeout: std::time::Duration,
    ) -> anyhow::Result<u16> {
        let url = format!("http://127.0.0.1:{port}{path}");
        let client = reqwest::Client::new();
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match client
                .get(&url)
                .timeout(std::time::Duration::from_secs(5))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    return Ok(resp.status().as_u16());
                }
                Ok(resp) if tokio::time::Instant::now() >= deadline => {
                    anyhow::bail!("site {site_id} health {url}: HTTP {}", resp.status());
                }
                Err(_) if tokio::time::Instant::now() >= deadline => {
                    anyhow::bail!("site {site_id} health {url}: unreachable after timeout");
                }
                _ => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

impl<S: Systemd, P: PostgresAdmin, C: Caddy> NativeRuntime<S, P, C> {
    /// Remove needs the full spec only for its paths; the id-only form
    /// stops the unit and forgets state without touching the fs.
    pub async fn remove_by_id(&self, site_id: &str) -> anyhow::Result<()>
    where
        S: Systemd,
    {
        let _ = self.stop_inner(site_id).await;
        let mut state = self.load_state().map_err(|e| anyhow::anyhow!("{e}"))?;
        state
            .remove(site_id, &self.config.state_path)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok(())
    }
}

#[async_trait::async_trait]
impl<S: Systemd + Send + Sync, P: PostgresAdmin + Send + Sync, C: Caddy + Send + Sync> Runtime
    for NativeRuntime<S, P, C>
{
    async fn install(&self, site: &SiteSpec) -> anyhow::Result<()> {
        self.install_version(site).await.map(|_| ())
    }

    async fn write_unit(&self, site: &SiteSpec) -> anyhow::Result<()> {
        self.write_unit_inner(site).await.map(|_| ())
    }

    async fn start(&self, id: SiteId) -> anyhow::Result<()> {
        self.start_inner(&id).await
    }

    async fn stop(&self, id: SiteId) -> anyhow::Result<()> {
        self.stop_inner(&id).await
    }

    async fn status(&self, id: SiteId) -> anyhow::Result<RuntimeStatus> {
        self.status_inner(&id).await
    }

    async fn logs(&self, id: SiteId, _since: Option<Cursor>) -> anyhow::Result<LogStream> {
        // Phase 1: last 500 lines; cursor support arrives with the
        // journal-cursor work in phase 2.
        let lines = self.logs_inner(&id, 500).await?;
        Ok(LogStream { lines, next: None })
    }

    async fn remove(&self, id: SiteId) -> anyhow::Result<()> {
        self.remove_by_id(&id).await
    }
}

/// Render env-file bytes: one `NAME='value'` line each (single-quote
/// escaping), trailing newline. Values are never logged by callers.
pub fn render_env(env: &[EnvVar]) -> String {
    let mut out = String::new();
    for var in env {
        out.push_str(&var.name);
        out.push('=');
        out.push('\'');
        out.push_str(&var.value.replace('\'', "'\\''"));
        out.push_str("'\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_render_quotes_and_escapes() {
        let vars = vec![
            EnvVar {
                name: "A".to_string(),
                value: "plain".to_string(),
                secret: false,
                once: false,
            },
            EnvVar {
                name: "B".to_string(),
                value: "with'quote".to_string(),
                secret: true,
                once: false,
            },
        ];
        let text = render_env(&vars);
        assert!(text.starts_with("A='plain'\n"));
        assert!(text.contains("B='with'\\''quote'\n"));
    }
}
