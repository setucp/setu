//! `NativeRuntime`: the phase-1 `Runtime` over the privileged helper and
//! the Postgres/Caddy adapters.
//!
//! Every privileged step is one typed helper verb keyed by the site's
//! short id (spec §4): `site-create`, `install`, `switch-version`,
//! `write-unit`, `write-env`, `unit`, `backup`, `restore`, `site-remove`.
//! The agent itself writes only under `/var/lib/setu` (state, downloads,
//! backups) and reads logs with `journalctl` as its own user.

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::json;
use setu_proto::{BinaryFormat, BinaryRef, EnvVar, SiteSpec};

use crate::caddy::CaddyAdmin;
use crate::config::AgentConfig;
use crate::download::DownloadPolicy;
use crate::helper::{UnitInput, Verb};
use crate::helper_client::{Helper, HelperIo};
use crate::paths::check_short;
use crate::postgres::PostgresAdmin;
use crate::runtime::{Cursor, LogStream, Runtime, RuntimeStatus, SiteId};
use crate::state::{SiteRecord, StateStore};

/// Phase-1 runtime: app binaries supervised as systemd template units.
/// Every site is addressed by its 8-char short id (spec §4).
#[derive(Debug, Clone)]
pub struct NativeRuntime<H, P, C> {
    helper: H,
    postgres: P,
    caddy: C,
    config: AgentConfig,
    state: Arc<StateStore>,
    /// Serialises route writes (`SetRoute`/`RemoveRoute`) against a
    /// reconcile pass, so a reconcile never resurrects a removed route.
    routes: Arc<tokio::sync::Mutex<()>>,
    journalctl: PathBuf,
}

impl<H, P, C> NativeRuntime<H, P, C> {
    /// Opens `config.state_path` as the one in-process state owner.
    pub fn new(helper: H, postgres: P, caddy: C, config: AgentConfig) -> Self {
        Self {
            helper,
            postgres,
            caddy,
            state: Arc::new(StateStore::open(&config.state_path)),
            config,
            routes: Arc::new(tokio::sync::Mutex::new(())),
            journalctl: PathBuf::from("journalctl"),
        }
    }

    /// Run `program` instead of `journalctl` (tests).
    #[allow(clippy::return_self_not_must_use)] // reviewed: kept as-is (R150)
    pub fn with_journalctl(mut self, program: PathBuf) -> Self {
        self.journalctl = program;
        self
    }

    /// The shared state (`state.json`).
    pub fn state(&self) -> &StateStore {
        &self.state
    }

    /// The route-write lock (see the field).
    pub fn route_lock(&self) -> &tokio::sync::Mutex<()> {
        &self.routes
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

    /// Update one site's state record via `f` (created when absent).
    pub fn update_state(&self, short: &str, f: impl FnOnce(&mut SiteRecord)) -> anyhow::Result<()> {
        check_short(short)?;
        self.state
            .update_site(short, f)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Short ids with a tree under the sites root.
    pub fn sites_on_disk(&self) -> Vec<String> {
        crate::reconcile::sites_on_disk(&self.config.sites_root)
    }
}

/// The spec's short id (checked).
fn spec_short(site: &SiteSpec) -> anyhow::Result<String> {
    check_short(&site.short_id)?;
    Ok(site.short_id.clone())
}

impl<H: Helper, P, C> NativeRuntime<H, P, C> {
    async fn call(
        &self,
        verb: Verb,
        short: &str,
        input: serde_json::Value,
    ) -> anyhow::Result<serde_json::Value> {
        check_short(short)?;
        Ok(self
            .helper
            .call(verb, short, input, HelperIo::none())
            .await?
            .json)
    }

    /// Helper `site-create {dirs}`: user + tree + `data/<dir>` from the
    /// spec (idempotent); records the site and its port.
    pub async fn site_create(&self, site: &SiteSpec) -> anyhow::Result<()> {
        let short = spec_short(site)?;
        self.call(Verb::SiteCreate, &short, json!({ "dirs": site.dirs }))
            .await?;
        self.update_state(&short, |e| e.port = site.port)
    }

    /// Download + verify as the agent, then helper `install {version,
    /// sha256}` (plus `format`, `exec` for a raw binary) into
    /// `bin/<version>`. Never touches `bin/current`: the
    /// explicit `SwitchVersion` moves it, so a bad update never stops the
    /// running site.
    pub async fn install_version(&self, short: &str, binary: &BinaryRef) -> anyhow::Result<String> {
        check_short(short)?;
        let sha = binary.sha256.to_ascii_lowercase();
        crate::download::fetch_verified(
            &binary.url,
            &sha,
            &self.config.downloads_dir,
            &self.policy(),
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        let input = match binary.format {
            BinaryFormat::TarGz => json!({ "version": binary.version, "sha256": sha }),
            BinaryFormat::Binary => json!({
                "version": binary.version,
                "sha256": sha,
                "format": binary.format,
                "exec": binary.exec,
            }),
        };
        self.call(Verb::Install, short, input).await?;
        Ok(binary.version.clone())
    }

    /// Helper `write-unit` (the drop-in) from the spec: version, exec,
    /// args, workdir and limits. True when the bytes changed.
    pub async fn write_unit_inner(&self, site: &SiteSpec) -> anyhow::Result<bool> {
        let short = spec_short(site)?;
        let input = UnitInput {
            short_id: short.clone(),
            version: site.version.clone(),
            exec: site.exec.clone(),
            args: site.args.clone(),
            workdir: site.workdir,
            memory_mb: site.limits.memory_mb,
            cpu_percent: site.limits.cpu_percent,
        };
        let out = self
            .call(Verb::WriteUnit, &short, serde_json::to_value(&input)?)
            .await?;
        self.update_state(&short, |e| e.port = site.port)?;
        Ok(out["changed"].as_bool().unwrap_or(true))
    }

    /// Helper `write-env` (root:root 0600). Values never reach logs.
    pub async fn write_env_inner(&self, short: &str, env: &[EnvVar]) -> anyhow::Result<()> {
        let vars: Vec<serde_json::Value> = env
            .iter()
            .map(|v| json!({ "name": v.name, "value": v.value }))
            .collect();
        self.call(Verb::WriteEnv, short, serde_json::Value::Array(vars))
            .await
            .map(|_| ())
    }

    async fn unit(&self, short: &str, op: &str) -> anyhow::Result<serde_json::Value> {
        self.call(Verb::Unit, short, json!({ "op": op })).await
    }

    pub async fn start_inner(&self, short: &str) -> anyhow::Result<()> {
        self.unit(short, "start").await.map(|_| ())
    }

    pub async fn stop_inner(&self, short: &str) -> anyhow::Result<()> {
        self.unit(short, "stop").await.map(|_| ())
    }

    pub async fn restart_inner(&self, short: &str) -> anyhow::Result<()> {
        self.unit(short, "restart").await.map(|_| ())
    }

    pub async fn status_inner(&self, short: &str) -> anyhow::Result<RuntimeStatus> {
        Ok(RuntimeStatus::from_show(&self.unit(short, "show").await?))
    }

    /// Journal lines after `cursor` (agent user, not the helper).
    pub async fn logs_inner(
        &self,
        short: &str,
        cursor: Option<&str>,
        lines: u32,
    ) -> anyhow::Result<LogStream> {
        crate::journal::read(&self.journalctl, short, cursor, lines).await
    }

    /// Helper `site-remove` (stop, drop-in, user, tree), then forget the
    /// site. The database goes with the separate `DropDatabase` command.
    pub async fn remove_inner(&self, short: &str) -> anyhow::Result<()> {
        self.call(Verb::SiteRemove, short, json!({})).await?;
        self.state
            .remove_site(short)
            .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// Helper `switch-version`; records the running version.
    pub async fn switch_version_inner(&self, short: &str, version: &str) -> anyhow::Result<bool> {
        let out = self
            .call(Verb::SwitchVersion, short, json!({ "version": version }))
            .await?;
        self.update_state(short, |e| e.version = version.to_string())?;
        Ok(out["changed"].as_bool().unwrap_or(true))
    }

    /// Helper `backup {dirs}`: the tar.zst streamed into the file `to`;
    /// returns its size in bytes.
    pub async fn backup_dirs(
        &self,
        short: &str,
        dirs: &[String],
        to: &std::path::Path,
    ) -> anyhow::Result<u64> {
        check_short(short)?;
        Ok(self
            .helper
            .call(
                Verb::Backup,
                short,
                json!({ "dirs": dirs }),
                HelperIo::stdout_to(to),
            )
            .await?
            .streamed)
    }

    /// Helper `restore {dirs}` with the tar.zst streamed from the file
    /// `from` on stdin.
    pub async fn restore_dirs(
        &self,
        short: &str,
        dirs: &[String],
        from: &std::path::Path,
    ) -> anyhow::Result<()> {
        check_short(short)?;
        self.helper
            .call(
                Verb::Restore,
                short,
                json!({ "dirs": dirs }),
                HelperIo::stdin_from(from),
            )
            .await
            .map(|_| ())
    }

    /// Poll `http://127.0.0.1:<port><health>` until 2xx or the spec's
    /// `startup_grace_s` (at least 1 s) passes.
    pub async fn health_inner(&self, site: &SiteSpec) -> anyhow::Result<u16> {
        spec_short(site)?;
        if !site.health.starts_with('/') || !setu_common::validate::no_control(&site.health) {
            anyhow::bail!("invalid health path");
        }
        let timeout = std::time::Duration::from_secs(u64::from(site.startup_grace_s.max(1)));
        let url = format!("http://127.0.0.1:{}{}", site.port, site.health);
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
                    anyhow::bail!(
                        "site {} health {url}: HTTP {}",
                        site.short_id,
                        resp.status()
                    );
                }
                Err(_) if tokio::time::Instant::now() >= deadline => {
                    anyhow::bail!(
                        "site {} health {url}: unreachable after timeout",
                        site.short_id
                    );
                }
                _ => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

#[async_trait::async_trait]
impl<H: Helper, P: PostgresAdmin + Send + Sync, C: CaddyAdmin> Runtime for NativeRuntime<H, P, C> {
    async fn install(&self, site: &SiteSpec, binary: &BinaryRef) -> anyhow::Result<()> {
        self.install_version(&site.short_id, binary)
            .await
            .map(|_| ())
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
