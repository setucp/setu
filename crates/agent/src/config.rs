//! Agent configuration: `/etc/setu/agent.toml` plus `SETU_AGENT_*` env.
//!
//! The control-plane URL and server token identify this server; the paths
//! have production defaults the tests override per-case.

use std::path::PathBuf;

/// Agent configuration. `server_token` is redacted in `Debug`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    /// Control-plane base URL, e.g. `https://panel.example.com`.
    #[serde(alias = "control_plane_url")]
    pub control_plane: String,
    /// Long-lived per-server bearer token from `join`.
    pub server_token: String,
    /// Server id assigned by the control plane at `join`.
    pub server_id: String,
    /// Agent state file (sites this host knows about).
    pub state_path: PathBuf,
    /// Parent dir of per-site dirs (`<sites_root>/<site_id>/…`).
    pub sites_root: PathBuf,
    /// Verified downloads (`<sha256>.tar.gz`), read by the helper.
    pub downloads_dir: PathBuf,
    /// Backup root (`<backups_root>/<site_id>/<ts>/…`).
    pub backups_root: PathBuf,
    /// Caddy admin API unix socket (TCP admin is off, spec §4).
    pub caddy_socket: PathBuf,
    /// Panel route on a control-plane box (`setu-panel`), reconciled
    /// like a site route.
    pub panel: Option<crate::caddy::PanelRoute>,
    /// Long-poll `?wait=` seconds for `GET /agent/v1/commands`.
    pub poll_wait_secs: u64,
    /// Heartbeat interval seconds.
    pub heartbeat_secs: u64,
    /// Reconnect backoff bounds (seconds) for the command loop.
    pub backoff_min_secs: u64,
    pub backoff_max_secs: u64,
    /// Allow plain-http control planes and downloads to non-loopback
    /// hosts (`--insecure`).
    pub insecure: bool,
    /// Extra root certificate (PEM) trusted for the control plane
    /// (`--ca`, copied next to `agent.toml`).
    pub ca: Option<PathBuf>,
    /// Seconds between route reconciles (spec §4: 5 minutes;
    /// `SETU_AGENT_RECONCILE_SECS` overrides).
    pub reconcile_secs: u64,
}

impl Default for AgentConfig {
    fn default() -> Self {
        let paths = crate::paths::Paths::default();
        Self {
            control_plane: "http://127.0.0.1:7000".to_string(),
            server_token: String::new(),
            server_id: String::new(),
            state_path: paths.state(),
            sites_root: paths.sites(),
            downloads_dir: paths.downloads(),
            backups_root: paths.backups(),
            caddy_socket: PathBuf::from(crate::caddy::DEFAULT_SOCKET),
            panel: None,
            poll_wait_secs: 30,
            heartbeat_secs: 15,
            backoff_min_secs: 1,
            backoff_max_secs: 30,
            insecure: false,
            ca: None,
            reconcile_secs: 300,
        }
    }
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("control_plane", &self.control_plane)
            .field("server_token", &"[redacted]")
            .field("server_id", &self.server_id)
            .field("state_path", &self.state_path)
            .field("sites_root", &self.sites_root)
            .field("downloads_dir", &self.downloads_dir)
            .field("backups_root", &self.backups_root)
            .field("caddy_socket", &self.caddy_socket)
            .field("panel", &self.panel)
            .field("poll_wait_secs", &self.poll_wait_secs)
            .field("heartbeat_secs", &self.heartbeat_secs)
            .field("backoff_min_secs", &self.backoff_min_secs)
            .field("backoff_max_secs", &self.backoff_max_secs)
            .field("insecure", &self.insecure)
            .field("ca", &self.ca)
            .field("reconcile_secs", &self.reconcile_secs)
            .finish()
    }
}

/// Errors loading the agent config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {0}: {1}")]
    Read(String, String),
    #[error("cannot parse {0}: {1}")]
    Parse(String, String),
}

impl AgentConfig {
    /// Default config-file path.
    pub fn default_path() -> PathBuf {
        PathBuf::from("/etc/setu/agent.toml")
    }

    /// Load from `path` (or the default path); a missing file yields
    /// defaults. `SETU_AGENT_CONTROL_PLANE_URL` and
    /// `SETU_AGENT_SERVER_TOKEN` env vars win over the file.
    pub fn load(path: Option<&std::path::Path>) -> Result<Self, ConfigError> {
        let mut cfg = Self::default();
        let at = path.map_or_else(Self::default_path, std::path::Path::to_path_buf);
        match std::fs::read_to_string(&at) {
            Ok(text) => {
                let file: Self = toml::from_str(&text)
                    .map_err(|e| ConfigError::Parse(at.display().to_string(), e.to_string()))?;
                cfg = file;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(ConfigError::Read(at.display().to_string(), e.to_string()));
            }
        }
        if let Ok(v) = std::env::var("SETU_AGENT_CONTROL_PLANE_URL") {
            if !v.trim().is_empty() {
                cfg.control_plane = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("SETU_AGENT_RECONCILE_SECS") {
            if let Ok(secs) = v.trim().parse::<u64>() {
                cfg.reconcile_secs = secs;
            }
        }
        if let Ok(v) = std::env::var("SETU_AGENT_SERVER_TOKEN") {
            if !v.trim().is_empty() {
                cfg.server_token = v.trim().to_string();
            }
        }
        Ok(cfg)
    }

    /// Write to `path` (parent created as needed): a temp file created
    /// 0600 from the start, then renamed over `path` (finding 6: the
    /// token is never world-readable, not even between create and
    /// chmod). `install` then hands it to `setu`.
    pub fn save(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Read(path.display().to_string(), e.to_string()))?;
        }
        let text = toml::to_string_pretty(self)
            .map_err(|e| ConfigError::Parse(path.display().to_string(), e.to_string()))?;
        atomic_write(path, text.as_bytes())
            .map_err(|e| ConfigError::Read(path.display().to_string(), e.to_string()))?;
        Ok(())
    }
}

impl AgentConfig {
    /// The control-plane HTTP client: rustls with the extra `ca` root
    /// when configured.
    pub fn http_client(&self) -> anyhow::Result<reqwest::Client> {
        http_client(self.ca.as_deref())
    }

    /// Control-plane base URL without a trailing slash.
    pub fn base_url(&self) -> String {
        self.control_plane.trim_end_matches('/').to_string()
    }
}

/// A reqwest client trusting the system roots plus `ca` (PEM).
pub fn http_client(ca: Option<&std::path::Path>) -> anyhow::Result<reqwest::Client> {
    let mut builder =
        reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(10));
    if let Some(ca) = ca {
        let pem = std::fs::read(ca)
            .map_err(|e| anyhow::anyhow!("cannot read CA {}: {e}", ca.display()))?;
        let cert = reqwest::Certificate::from_pem(&pem)
            .map_err(|e| anyhow::anyhow!("invalid CA {}: {e}", ca.display()))?;
        builder = builder.add_root_certificate(cert);
    }
    builder
        .build()
        .map_err(|e| anyhow::anyhow!("http client: {e}"))
}

/// Atomic file write: a fresh temp file in the same dir, created
/// `O_CREAT|O_EXCL` with mode 0600, written, synced, renamed over
/// `path`. Nothing is ever readable by others at any point.
pub fn atomic_write(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}", setu_common::random_hex(8)));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_token_is_redacted_in_debug() {
        let cfg = AgentConfig {
            server_token: "SECRET-server-token".to_string(),
            ..AgentConfig::default()
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("SECRET-server-token"), "token leaked: {dbg}");
        assert!(dbg.contains("[redacted]"));
    }

    #[test]
    fn missing_file_yields_defaults_and_env_wins() {
        let cfg = AgentConfig::load(Some(std::path::Path::new(
            "/nonexistent-setu-agent-config.toml",
        )))
        .expect("missing file is defaults");
        assert_eq!(cfg.poll_wait_secs, 30);
        assert_eq!(cfg.heartbeat_secs, 15);

        std::env::set_var("SETU_AGENT_CONTROL_PLANE_URL", "https://panel.example.test");
        let cfg = AgentConfig::load(Some(std::path::Path::new(
            "/nonexistent-setu-agent-config.toml",
        )))
        .expect("env override");
        std::env::remove_var("SETU_AGENT_CONTROL_PLANE_URL");
        assert_eq!(cfg.control_plane, "https://panel.example.test");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.toml");
        let cfg = AgentConfig {
            control_plane: "https://panel.example.test".to_string(),
            server_token: "tok".to_string(),
            ..AgentConfig::default()
        };
        cfg.save(&path).expect("save");
        let back = AgentConfig::load(Some(&path)).expect("load");
        assert_eq!(back.control_plane, "https://panel.example.test");
        assert_eq!(back.server_token, "tok");
    }
}
