//! `NativeRuntime`: the phase-1 `Runtime` over the privileged helper and
//! the Postgres/Caddy adapters.
//!
//! Every privileged step is one typed helper verb keyed by the site's
//! short id (spec §4): `site-create`, `install`, `switch-version`,
//! `write-unit`, `write-env`, `unit`, `backup`, `restore`, `site-remove`.
//! The agent itself writes only under `/var/lib/setu` (state, downloads,
//! backups) and reads logs with `journalctl` as its own user.

use serde_json::json;
use setu_proto::{EnvVar, SiteSpec};

use crate::caddy::Caddy;
use crate::config::AgentConfig;
use crate::download::DownloadPolicy;
use crate::helper::{UnitInput, Verb, Workdir};
use crate::helper_client::Helper;
use crate::paths::{check_short, short_of};
use crate::postgres::PostgresAdmin;
use crate::runtime::{Cursor, LogStream, Runtime, RuntimeStatus, SiteId};
use crate::state::{SiteEntry, StateFile};

/// Phase-1 runtime: app binaries supervised as systemd template units.
#[derive(Debug, Clone)]
pub struct NativeRuntime<H, P, C> {
    helper: H,
    postgres: P,
    caddy: C,
    config: AgentConfig,
}

impl<H, P, C> NativeRuntime<H, P, C> {
    pub fn new(helper: H, postgres: P, caddy: C, config: AgentConfig) -> Self {
        Self {
            helper,
            postgres,
            caddy,
            config,
        }
    }

    pub fn helper(&self) -> &H {
        &self.helper
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

    fn policy(&self) -> DownloadPolicy {
        DownloadPolicy {
            insecure: self.config.insecure,
        }
    }

    fn load_state(&self) -> anyhow::Result<StateFile> {
        StateFile::load(&self.config.state_path).map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Insert or update one site's state entry via `f`.
    fn update_state(
        &self,
        site_id: &str,
        port: u16,
        f: impl FnOnce(&mut SiteEntry),
    ) -> anyhow::Result<()> {
        let mut state = self.load_state()?;
        let mut entry = state.sites.get(site_id).cloned().unwrap_or(SiteEntry {
            port,
            db: None,
            version: String::new(),
        });
        entry.port = port;
        f(&mut entry);
        state
            .upsert(site_id, entry, &self.config.state_path)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
}

/// The site's short id from its spec (checked).
fn spec_short(site: &SiteSpec) -> anyhow::Result<String> {
    check_short(&site.short_id)?;
    Ok(site.short_id.clone())
}

/// `exec` relative to `bin/current`: the binary ref's, else the part of
/// the spec's absolute exec path after `bin/current/`.
fn exec_rel(site: &SiteSpec) -> String {
    if let Some(b) = &site.binary {
        return b.exec.clone();
    }
    let comps: Vec<String> = site
        .exec
        .components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    match comps.iter().position(|c| c == "current") {
        Some(i) if i + 1 < comps.len() => comps[i + 1..].join("/"),
        _ => comps.last().cloned().unwrap_or_default(),
    }
}

impl<H: Helper, P, C> NativeRuntime<H, P, C> {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        Ok(self.helper.call(verb, short, input, None).await?.json)
    }

    /// `site-create {dirs}`: user + tree (idempotent).
    pub async fn site_create(&self, short: &str, dirs: &[String]) -> anyhow::Result<()> {
        self.call(Verb::SiteCreate, short, json!({ "dirs": dirs }))
            .await
            .map(|_| ())
    }

    /// Download + verify as the agent, then helper `install {version,
    /// sha256}`. The first install also points `bin/current` at it;
    /// updates leave `current` alone (the explicit `SwitchVersion` step
    /// moves it, so a bad update never stops the running site).
    /// Returns the installed version (`None` without a binary ref).
    pub async fn install_version(&self, site: &SiteSpec) -> anyhow::Result<Option<String>> {
        let short = spec_short(site)?;
        self.site_create(&short, &[]).await?;
        let Some(binary) = site.binary.as_ref() else {
            return Ok(None);
        };
        let sha = binary.sha256.to_ascii_lowercase();
        crate::download::fetch_verified(
            &binary.url,
            &sha,
            &self.config.downloads_dir,
            &self.policy(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        self.call(
            Verb::Install,
            &short,
            json!({ "version": binary.version, "sha256": sha }),
        )
        .await?;
        let first = self
            .load_state()?
            .sites
            .get(&site.site_id)
            .is_none_or(|e| e.version.is_empty());
        if first {
            self.call(
                Verb::SwitchVersion,
                &short,
                json!({ "version": binary.version }),
            )
            .await?;
        }
        self.update_state(&site.site_id, site.port, |e| {
            if first {
                e.version = binary.version.clone();
            }
        })?;
        Ok(Some(binary.version.clone()))
    }

    /// Helper `write-unit` (the drop-in). True when the bytes changed.
    pub async fn write_unit_inner(&self, site: &SiteSpec) -> anyhow::Result<bool> {
        let short = spec_short(site)?;
        let recorded = self
            .load_state()?
            .sites
            .get(&site.site_id)
            .map(|e| e.version.clone())
            .unwrap_or_default();
        let version = site
            .binary
            .as_ref()
            .map(|b| b.version.clone())
            .filter(|v| !v.is_empty())
            .unwrap_or(if recorded.is_empty() {
                "0".to_string()
            } else {
                recorded
            });
        // `workdir` defaults to the manifest default until WS5 adds it
        // to the proto spec (ledger R63).
        let input = UnitInput {
            short_id: short.clone(),
            version,
            exec: exec_rel(site),
            args: site.args.clone(),
            workdir: Workdir::ExecDir,
            memory_mb: site.limits.memory_mb,
            cpu_percent: site.limits.cpu_percent,
        };
        let out = self
            .call(Verb::WriteUnit, &short, serde_json::to_value(&input)?)
            .await?;
        self.update_state(&site.site_id, site.port, |_| {})?;
        Ok(out["changed"].as_bool().unwrap_or(true))
    }

    /// Helper `write-env` (root:root 0600). Values never reach logs.
    pub async fn write_env_inner(&self, site: &SiteSpec, env: &[EnvVar]) -> anyhow::Result<()> {
        let short = spec_short(site)?;
        self.site_create(&short, &[]).await?;
        let vars: Vec<serde_json::Value> = env
            .iter()
            .map(|v| json!({ "name": v.name, "value": v.value }))
            .collect();
        self.call(Verb::WriteEnv, &short, serde_json::Value::Array(vars))
            .await
            .map(|_| ())
    }

    async fn unit(&self, site_id: &str, op: &str) -> anyhow::Result<serde_json::Value> {
        let short = short_of(site_id)?;
        self.call(Verb::Unit, &short, json!({ "op": op })).await
    }

    pub async fn start_inner(&self, site_id: &str) -> anyhow::Result<()> {
        self.unit(site_id, "start").await.map(|_| ())
    }

    pub async fn stop_inner(&self, site_id: &str) -> anyhow::Result<()> {
        self.unit(site_id, "stop").await.map(|_| ())
    }

    pub async fn restart_inner(&self, site_id: &str) -> anyhow::Result<()> {
        self.unit(site_id, "restart").await.map(|_| ())
    }

    pub async fn status_inner(&self, site_id: &str) -> anyhow::Result<RuntimeStatus> {
        Ok(RuntimeStatus::from_show(&self.unit(site_id, "show").await?))
    }

    /// Journal lines after `cursor` (agent user, not the helper).
    pub async fn logs_inner(
        &self,
        site_id: &str,
        cursor: Option<&str>,
        lines: u32,
    ) -> anyhow::Result<LogStream> {
        crate::journal::read(&short_of(site_id)?, cursor, lines).await
    }

    /// Helper `site-remove` (stop, drop-in, user, tree), then forget the
    /// site. The database goes with the separate `DropDatabase` command.
    pub async fn remove_inner(&self, site_id: &str) -> anyhow::Result<()> {
        let short = short_of(site_id)?;
        self.call(Verb::SiteRemove, &short, json!({})).await?;
        let mut state = self.load_state()?;
        state
            .remove(site_id, &self.config.state_path)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Helper `switch-version`; records the running version.
    pub async fn switch_version_inner(&self, site_id: &str, version: &str) -> anyhow::Result<bool> {
        let short = short_of(site_id)?;
        let out = self
            .call(Verb::SwitchVersion, &short, json!({ "version": version }))
            .await?;
        let mut state = self.load_state()?;
        if let Some(mut entry) = state.sites.get(site_id).cloned() {
            entry.version = version.to_string();
            state
                .upsert(site_id, entry, &self.config.state_path)
                .map_err(|e| anyhow::anyhow!("{e}"))?;
        }
        Ok(out["changed"].as_bool().unwrap_or(true))
    }

    /// Helper `backup {dirs}`: the tar.zst bytes.
    pub async fn backup_dirs(&self, short: &str, dirs: &[String]) -> anyhow::Result<Vec<u8>> {
        Ok(self
            .helper
            .call(Verb::Backup, short, json!({ "dirs": dirs }), None)
            .await?
            .stdout)
    }

    /// Helper `restore {dirs}` with the tar.zst on stdin.
    pub async fn restore_dirs(
        &self,
        short: &str,
        dirs: &[String],
        archive: Vec<u8>,
    ) -> anyhow::Result<()> {
        self.helper
            .call(Verb::Restore, short, json!({ "dirs": dirs }), Some(archive))
            .await
            .map(|_| ())
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

#[async_trait::async_trait]
impl<H: Helper, P: PostgresAdmin + Send + Sync, C: Caddy + Send + Sync> Runtime
    for NativeRuntime<H, P, C>
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

    async fn logs(&self, id: SiteId, since: Option<Cursor>) -> anyhow::Result<LogStream> {
        self.logs_inner(&id, since.as_deref(), 500).await
    }

    async fn remove(&self, id: SiteId) -> anyhow::Result<()> {
        self.remove_inner(&id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exec_rel_strips_bin_current() {
        let mut site = SiteSpec {
            site_id: "01jx00000000000000abcd1234".into(),
            short_id: "abcd1234".into(),
            user: "setu-site-abcd1234".into(),
            data_dir: "/srv/setu/sites/abcd1234/data".into(),
            run_dir: "/srv/setu/sites/abcd1234/run".into(),
            port: 20001,
            exec: "/srv/setu/sites/abcd1234/bin/current/dist/app".into(),
            args: vec![],
            env_file: "/srv/setu/sites/abcd1234/env".into(),
            limits: setu_proto::Limits {
                memory_mb: 256,
                cpu_percent: 50,
                disk_mb: 1024,
            },
            health_path: "/healthz".into(),
            binary: None,
        };
        assert_eq!(exec_rel(&site), "dist/app");
        site.binary = Some(setu_proto::BinaryRef {
            url: "https://x/a.tar.gz".into(),
            sha256: "a".repeat(64),
            exec: "vyasa".into(),
            version: "0.2.0".into(),
        });
        assert_eq!(exec_rel(&site), "vyasa");
    }
}
