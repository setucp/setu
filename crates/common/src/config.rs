//! Control-plane configuration (`setu.toml` + `SETU_*` env).
//!
//! Sources, in increasing precedence: built-in defaults, `setu.toml` in the
//! current directory when present, then `SETU_*` environment variables.
//! Environment always wins over the file.

use std::{fmt, net::SocketAddr, path::PathBuf, str::FromStr};

use ipnet::IpNet;
use url::Url;

/// Where `setu serve` listens: a unix socket (behind Caddy) or TCP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listen {
    Unix(PathBuf),
    Tcp(SocketAddr),
}

impl FromStr for Listen {
    type Err = ConfigError;

    /// `unix:<absolute path>` or `<ip>:<port>`.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        let bad = |why: &str| ConfigError::InvalidListen(s.to_string(), why.to_string());
        if let Some(path) = s.strip_prefix("unix:") {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(bad("unix socket path must be absolute"));
            }
            return Ok(Listen::Unix(path));
        }
        SocketAddr::from_str(s)
            .map(Listen::Tcp)
            .map_err(|_| bad("expected unix:<path> or <ip>:<port>"))
    }
}

impl fmt::Display for Listen {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Listen::Unix(p) => write!(f, "unix:{}", p.display()),
            Listen::Tcp(a) => write!(f, "{a}"),
        }
    }
}

/// 32-byte secret used for sealing. Never logged.
#[derive(Clone)]
pub struct SecretKey {
    bytes: [u8; 32],
}

impl SecretKey {
    /// Parse exactly 32 bytes as 64 hex chars.
    pub fn from_hex(s: &str) -> Result<Self, ConfigError> {
        if s.len() != 64 {
            return Err(ConfigError::InvalidSecretKey(format!(
                "SETU_SECRET_KEY must be 64 hex chars (32 bytes), got {} chars",
                s.len()
            )));
        }
        let decoded = hex::decode(s).map_err(|e| {
            ConfigError::InvalidSecretKey(format!("SETU_SECRET_KEY is not valid hex: {e}"))
        })?;
        if decoded.len() != 32 {
            return Err(ConfigError::InvalidSecretKey(format!(
                "SETU_SECRET_KEY must decode to 32 bytes, got {} bytes",
                decoded.len()
            )));
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&decoded);
        Ok(Self { bytes })
    }

    /// Raw key bytes for the [`crate::seal::Sealer`].
    pub fn bytes(&self) -> &[u8; 32] {
        &self.bytes
    }
}

impl std::fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretKey([redacted])")
    }
}

/// Control-plane configuration.
#[derive(Clone)]
pub struct Config {
    pub listen: Listen,
    pub database_url: String,
    pub secret_key: SecretKey,
    pub public_url: Url,
    /// Peers whose `X-Forwarded-For` is believed on a TCP listener.
    pub trusted_proxies: Vec<IpNet>,
    pub data_dir: PathBuf,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("listen", &self.listen)
            .field("database_url", &"<redacted>")
            .field("secret_key", &self.secret_key)
            .field("public_url", &self.public_url.as_str())
            .field("trusted_proxies", &self.trusted_proxies)
            .field("data_dir", &self.data_dir)
            .finish()
    }
}

/// Errors from [`Config::load`] and [`SecretKey::from_hex`].
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required config {0} (set the {0} env var or setu.toml key)")]
    Missing(&'static str),
    #[error("invalid SETU_SECRET_KEY: {0}")]
    InvalidSecretKey(String),
    #[error("invalid listen {0}: {1}")]
    InvalidListen(String, String),
    #[error("invalid trusted_proxies entry {0}: expected an IP or CIDR")]
    InvalidTrustedProxy(String),
    #[error("invalid public URL {0}: {1}")]
    InvalidPublicUrl(String, String),
    #[error("config load failed: {0}")]
    Load(String),
}

impl Config {
    /// Load from `setu.toml` (if present) with `SETU_*` env overriding.
    pub fn load() -> Result<Self, ConfigError> {
        let cfg = config::Config::builder()
            .set_default("listen", "127.0.0.1:7000")
            .map_err(|e| ConfigError::Load(e.to_string()))?
            .set_default("trusted_proxies", vec!["127.0.0.1/32"])
            .map_err(|e| ConfigError::Load(e.to_string()))?
            // Ruling R47: the control plane's own state; agents derive
            // site paths from the short id, never from this.
            .set_default("data_dir", "/var/lib/setu")
            .map_err(|e| ConfigError::Load(e.to_string()))?
            .add_source(config::File::with_name("setu").required(false))
            .add_source(
                config::Environment::with_prefix("SETU")
                    .prefix_separator("_")
                    .separator("__")
                    .try_parsing(false),
            )
            .build()
            .map_err(|e| ConfigError::Load(e.to_string()))?;

        let database_url = cfg
            .get_string("database_url")
            .map_err(|_| ConfigError::Missing("SETU_DATABASE_URL"))?;
        if database_url.trim().is_empty() {
            return Err(ConfigError::Missing("SETU_DATABASE_URL"));
        }

        let secret_raw = cfg
            .get_string("secret_key")
            .map_err(|_| ConfigError::Missing("SETU_SECRET_KEY"))?;
        let secret_key = SecretKey::from_hex(secret_raw.trim())?;

        let public_raw = cfg
            .get_string("public_url")
            .map_err(|_| ConfigError::Missing("SETU_PUBLIC_URL"))?;
        let public_url = Url::parse(public_raw.trim()).map_err(|e| {
            ConfigError::InvalidPublicUrl(public_raw.trim().to_string(), e.to_string())
        })?;

        let listen_raw = cfg
            .get_string("listen")
            .map_err(|_| ConfigError::Load("listen default missing".to_string()))?;
        let listen = Listen::from_str(&listen_raw)?;

        // A list in `setu.toml`, a comma-separated string from the env.
        let proxies_raw: Vec<String> = match cfg.get_array("trusted_proxies") {
            Ok(values) => values
                .into_iter()
                .map(config::Value::into_string)
                .collect::<Result<_, _>>()
                .map_err(|e| ConfigError::Load(e.to_string()))?,
            Err(_) => cfg
                .get_string("trusted_proxies")
                .map_err(|e| ConfigError::Load(e.to_string()))?
                .split(',')
                .map(str::to_string)
                .collect(),
        };
        let trusted_proxies = proxies_raw
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .map(parse_proxy)
            .collect::<Result<Vec<_>, _>>()?;

        let data_raw = cfg
            .get_string("data_dir")
            .map_err(|_| ConfigError::Load("data_dir default missing".to_string()))?;
        let data_dir = PathBuf::from(data_raw.trim());

        Ok(Self {
            listen,
            database_url: database_url.trim().to_string(),
            secret_key,
            public_url,
            trusted_proxies,
            data_dir,
        })
    }
}

/// `10.0.0.0/8` or a bare address (`127.0.0.1` → `/32`, `::1` → `/128`).
fn parse_proxy(s: &str) -> Result<IpNet, ConfigError> {
    s.parse::<IpNet>()
        .or_else(|_| s.parse::<std::net::IpAddr>().map(IpNet::from))
        .map_err(|_| ConfigError::InvalidTrustedProxy(s.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    const KEYS: &[&str] = &[
        "SETU_LISTEN",
        "SETU_DATABASE_URL",
        "SETU_SECRET_KEY",
        "SETU_PUBLIC_URL",
        "SETU_DATA_DIR",
        "SETU_TRUSTED_PROXIES",
    ];

    fn clear_setu_env() {
        for key in KEYS {
            // Tests are #[serial] so no concurrent env access.
            env::remove_var(key);
        }
    }

    /// Load with the required keys from env and `extra` set, in an empty
    /// temp cwd (no `setu.toml`), restoring cwd and env afterwards.
    fn load_with(extra: &[(&str, &str)]) -> Result<Config, ConfigError> {
        clear_setu_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let orig_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("chdir to tempdir");
        env::set_var("SETU_DATABASE_URL", "postgres://localhost/db");
        env::set_var("SETU_SECRET_KEY", "ab".repeat(32));
        env::set_var("SETU_PUBLIC_URL", "https://panel.example.com");
        for (k, v) in extra {
            env::set_var(k, v);
        }
        let loaded = Config::load();
        std::env::set_current_dir(orig_dir).expect("restore cwd");
        clear_setu_env();
        loaded
    }

    #[test]
    #[serial]
    fn secret_key_must_be_64_hex() {
        // 63 hex chars: one short of the required 64.
        let short = "ab".repeat(31) + "a";
        let err = SecretKey::from_hex(&short).expect_err("63 chars must fail");
        assert!(format!("{err}").contains("SETU_SECRET_KEY"));
        let err = SecretKey::from_hex(&"zz".repeat(32)).expect_err("non-hex must fail");
        assert!(format!("{err}").contains("SETU_SECRET_KEY"));
        let ok = SecretKey::from_hex(&"ab".repeat(32)).expect("64 hex must parse");
        assert_eq!(ok.bytes().len(), 32);
        assert!(!format!("{ok:?}").contains("ab"));
        let err = load_with(&[("SETU_SECRET_KEY", "abc")]).expect_err("short key");
        assert!(format!("{err}").contains("SETU_SECRET_KEY"));
    }

    #[test]
    #[serial]
    fn env_overrides_file() {
        clear_setu_env();
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("setu.toml"),
            format!(
                "database_url = \"postgres://file/db\"\nsecret_key = \"{}\"\npublic_url = \"http://file.example/\"\nlisten = \"127.0.0.1:7100\"\n",
                "ab".repeat(32),
            ),
        )
        .expect("write setu.toml");
        let orig_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("chdir to tempdir");
        env::set_var("SETU_DATABASE_URL", "postgres://env/db");
        let loaded = Config::load();
        std::env::set_current_dir(orig_dir).expect("restore cwd");
        clear_setu_env();
        let cfg = loaded.expect("config must load");
        assert_eq!(cfg.database_url, "postgres://env/db");
        assert_eq!(cfg.public_url.as_str(), "http://file.example/");
        assert_eq!(cfg.listen, Listen::Tcp("127.0.0.1:7100".parse().unwrap()));
    }

    #[test]
    #[serial]
    fn prefix_separator_is_single_underscore() {
        // `SETU_DATA_DIR` → `data_dir` (prefix `_`, nested `__`): a double
        // underscore after the prefix would not be read.
        let cfg = load_with(&[("SETU_DATA_DIR", "/tmp/setu-data")]).expect("load");
        assert_eq!(cfg.data_dir, PathBuf::from("/tmp/setu-data"));
        let cfg = load_with(&[("SETU__DATA_DIR", "/nope")]).expect("load");
        env::remove_var("SETU__DATA_DIR");
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/setu"));
    }

    #[test]
    #[serial]
    fn defaults_listen_proxies_and_data_dir() {
        let cfg = load_with(&[]).expect("load");
        assert_eq!(cfg.listen, Listen::Tcp("127.0.0.1:7000".parse().unwrap()));
        assert_eq!(
            cfg.trusted_proxies,
            vec!["127.0.0.1/32".parse::<IpNet>().unwrap()]
        );
        assert_eq!(cfg.data_dir, PathBuf::from("/var/lib/setu"));
        let cfg =
            load_with(&[("SETU_TRUSTED_PROXIES", "10.0.0.0/8, 192.168.1.7,::1")]).expect("load");
        let want: Vec<IpNet> = vec![
            "10.0.0.0/8".parse().unwrap(),
            "192.168.1.7/32".parse().unwrap(),
            "::1/128".parse().unwrap(),
        ];
        assert_eq!(cfg.trusted_proxies, want);
        assert!(load_with(&[("SETU_TRUSTED_PROXIES", "not-an-ip")]).is_err());
    }

    #[test]
    #[serial]
    fn missing_database_url_is_an_error() {
        clear_setu_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let orig_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("chdir to tempdir");
        env::set_var("SETU_SECRET_KEY", "ab".repeat(32));
        env::set_var("SETU_PUBLIC_URL", "http://127.0.0.1:7000");
        let err = Config::load().expect_err("missing database url must fail");
        std::env::set_current_dir(orig_dir).expect("restore cwd");
        clear_setu_env();
        assert!(format!("{err}").contains("DATABASE_URL"));
    }

    #[test]
    fn listen_parses_unix_and_tcp() {
        assert_eq!(
            "unix:/run/setu/http.sock".parse::<Listen>().unwrap(),
            Listen::Unix(PathBuf::from("/run/setu/http.sock"))
        );
        assert_eq!(
            "127.0.0.1:7000".parse::<Listen>().unwrap(),
            Listen::Tcp("127.0.0.1:7000".parse().unwrap())
        );
        assert_eq!(
            "[::1]:7000".parse::<Listen>().unwrap(),
            Listen::Tcp("[::1]:7000".parse().unwrap())
        );
        for bad in ["unix:", "unix:relative.sock", "localhost:7000", "7000", ""] {
            assert!(bad.parse::<Listen>().is_err(), "{bad:?} must fail");
        }
        assert_eq!(
            Listen::Unix(PathBuf::from("/run/x.sock")).to_string(),
            "unix:/run/x.sock"
        );
    }

    #[test]
    #[serial]
    fn config_debug_redacts_database_url() {
        let cfg = load_with(&[("SETU_DATABASE_URL", "postgres://u:hunter2@db/x")]).expect("load");
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("hunter2"), "debug leaked: {dbg}");
        assert!(!dbg.contains(&"ab".repeat(32)), "debug leaked key: {dbg}");
        assert!(dbg.contains("listen"));
    }
}
