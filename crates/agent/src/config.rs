//! Agent configuration: `/etc/setu/agent.toml` plus `SETU_AGENT_*` env.
//!
//! The control-plane URL and server token identify this server; the paths
//! have production defaults the tests override per-case.

use std::path::PathBuf;

/// Agent configuration. `server_token` is redacted in `Debug`.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentConfig {
    /// Control-plane base URL, e.g. `https://panel.example.com`.
    pub control_plane_url: String,
    /// Long-lived per-server bearer token from `join`.
    pub server_token: String,
    /// Server id assigned by the control plane at `join`.
    pub server_id: String,
    /// Agent state file (sites this host knows about).
    pub state_path: PathBuf,
    /// Parent dir of per-site dirs (`<sites_root>/<site_id>/…`).
    pub sites_root: PathBuf,
    /// systemd unit dir (`/etc/systemd/system` in production).
    pub unit_dir: PathBuf,
    /// Backup root (`<backups_root>/<site_id>/<ts>/…`).
    pub backups_root: PathBuf,
    /// Caddy admin API base URL.
    pub caddy_admin: String,
    /// Long-poll `?wait=` seconds for `GET /agent/v1/commands`.
    pub poll_wait_secs: u64,
    /// Heartbeat interval seconds.
    pub heartbeat_secs: u64,
    /// Reconnect backoff bounds (seconds) for the command loop.
    pub backoff_min_secs: u64,
    pub backoff_max_secs: u64,
    /// chown site files to the site user (off in tests: users don't exist).
    pub chown_files: bool,
    /// Write privileged files through `sudo <exe> helper write-file`
    /// (off in tests: no sudoers on the scratch host).
    pub use_sudo_helper: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            control_plane_url: "http://127.0.0.1:7000".to_string(),
            server_token: String::new(),
            server_id: String::new(),
            state_path: PathBuf::from("/var/lib/setu/state.json"),
            sites_root: PathBuf::from("/srv/setu/sites"),
            unit_dir: PathBuf::from("/etc/systemd/system"),
            backups_root: PathBuf::from("/var/lib/setu/backups"),
            caddy_admin: "http://127.0.0.1:2019".to_string(),
            poll_wait_secs: 30,
            heartbeat_secs: 15,
            backoff_min_secs: 1,
            backoff_max_secs: 30,
            chown_files: true,
            use_sudo_helper: true,
        }
    }
}

impl std::fmt::Debug for AgentConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentConfig")
            .field("control_plane_url", &self.control_plane_url)
            .field("server_token", &"[redacted]")
            .field("server_id", &self.server_id)
            .field("state_path", &self.state_path)
            .field("sites_root", &self.sites_root)
            .field("unit_dir", &self.unit_dir)
            .field("backups_root", &self.backups_root)
            .field("caddy_admin", &self.caddy_admin)
            .field("poll_wait_secs", &self.poll_wait_secs)
            .field("heartbeat_secs", &self.heartbeat_secs)
            .field("backoff_min_secs", &self.backoff_min_secs)
            .field("backoff_max_secs", &self.backoff_max_secs)
            .field("chown_files", &self.chown_files)
            .field("use_sudo_helper", &self.use_sudo_helper)
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
        let at = path
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(Self::default_path);
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
                cfg.control_plane_url = v.trim().to_string();
            }
        }
        if let Ok(v) = std::env::var("SETU_AGENT_SERVER_TOKEN") {
            if !v.trim().is_empty() {
                cfg.server_token = v.trim().to_string();
            }
        }
        Ok(cfg)
    }

    /// Write to `path` (parent created as needed). Callers running as root
    /// are responsible for mode 0600; see `install`.
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

/// Atomic file write: temp file in the same dir + rename.
pub fn atomic_write(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), nanos_suffix()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn nanos_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u128)
        .unwrap_or(0)
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
        assert_eq!(cfg.control_plane_url, "https://panel.example.test");
    }

    #[test]
    fn save_then_load_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("agent.toml");
        let cfg = AgentConfig {
            control_plane_url: "https://panel.example.test".to_string(),
            server_token: "tok".to_string(),
            ..AgentConfig::default()
        };
        cfg.save(&path).expect("save");
        let back = AgentConfig::load(Some(&path)).expect("load");
        assert_eq!(back.control_plane_url, "https://panel.example.test");
        assert_eq!(back.server_token, "tok");
    }
}
