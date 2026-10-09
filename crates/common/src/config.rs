//! Control-plane configuration (`setu.toml` + `SETU_*` env).
//!
//! Sources, in increasing precedence: built-in defaults, `setu.toml` in the
//! current directory when present, then `SETU_*` environment variables.
//! Environment always wins over the file.

use std::{net::SocketAddr, path::PathBuf, str::FromStr};

use url::Url;

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
    pub bind_addr: SocketAddr,
    pub database_url: String,
    pub secret_key: SecretKey,
    pub public_url: Url,
    pub data_dir: PathBuf,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("bind_addr", &self.bind_addr)
            .field("database_url", &"<redacted>")
            .field("secret_key", &self.secret_key)
            .field("public_url", &self.public_url.as_str())
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
    #[error("invalid bind address {0}: {1}")]
    InvalidBindAddr(String, String),
    #[error("invalid public URL {0}: {1}")]
    InvalidPublicUrl(String, String),
    #[error("config load failed: {0}")]
    Load(String),
}

impl Config {
    /// Load from `setu.toml` (if present) with `SETU_*` env overriding.
    pub fn load() -> Result<Self, ConfigError> {
        let cfg = config::Config::builder()
            .set_default("bind_addr", "127.0.0.1:7000")
            .map_err(|e| ConfigError::Load(e.to_string()))?
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

        let bind_raw = cfg
            .get_string("bind_addr")
            .map_err(|_| ConfigError::Load("bind_addr default missing".to_string()))?;
        let bind_addr = SocketAddr::from_str(bind_raw.trim()).map_err(|e| {
            ConfigError::InvalidBindAddr(bind_raw.trim().to_string(), e.to_string())
        })?;

        let data_raw = cfg
            .get_string("data_dir")
            .map_err(|_| ConfigError::Load("data_dir default missing".to_string()))?;
        let data_dir = PathBuf::from(data_raw.trim());

        Ok(Self {
            bind_addr,
            database_url: database_url.trim().to_string(),
            secret_key,
            public_url,
            data_dir,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::env;

    fn clear_setu_env() {
        for key in [
            "SETU_BIND_ADDR",
            "SETU_DATABASE_URL",
            "SETU_SECRET_KEY",
            "SETU_PUBLIC_URL",
            "SETU_DATA_DIR",
        ] {
            // Tests are #[serial] so no concurrent env access.
            env::remove_var(key);
        }
    }

    #[test]
    #[serial]
    fn secret_key_must_be_32_bytes_hex() {
        clear_setu_env();
        // 63 hex chars: one short of the required 64.
        let short = "ab".repeat(31) + "a";
        assert_eq!(short.len(), 63);
        let err = SecretKey::from_hex(&short).expect_err("63 chars must fail");
        let msg = format!("{err}");
        assert!(
            msg.contains("SETU_SECRET_KEY"),
            "error must name SETU_SECRET_KEY, got: {msg}"
        );

        // Non-hex of the right length must also fail naming the key.
        let non_hex = "zz".repeat(32);
        let err = SecretKey::from_hex(&non_hex).expect_err("non-hex must fail");
        assert!(format!("{err}").contains("SETU_SECRET_KEY"));

        // 64 valid hex chars parse.
        let ok = SecretKey::from_hex(&"ab".repeat(32)).expect("64 hex must parse");
        assert_eq!(ok.bytes().len(), 32);

        // Debug must not leak key material.
        let dbg = format!("{ok:?}");
        assert!(!dbg.contains("ab"), "debug must redact: {dbg}");
    }

    #[test]
    #[serial]
    fn env_overrides_file() {
        clear_setu_env();
        let dir = tempfile::tempdir().expect("tempdir");
        let file_path = dir.path().join("setu.toml");
        std::fs::write(
            &file_path,
            format!(
                "database_url = \"postgres://file/db\"\nsecret_key = \"{}\"\npublic_url = \"http://file.example/\"\n",
                "ab".repeat(32),
            ),
        )
        .expect("write setu.toml");
        let orig_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("chdir to tempdir");
        env::set_var("SETU_DATABASE_URL", "postgres://env/db");
        env::set_var("SETU_SECRET_KEY", "ab".repeat(32));
        env::set_var("SETU_PUBLIC_URL", "http://127.0.0.1:7000");
        let loaded = Config::load();
        std::env::set_current_dir(orig_dir).expect("restore cwd");
        clear_setu_env();
        let cfg = loaded.expect("config must load");
        assert_eq!(cfg.database_url, "postgres://env/db");
    }

    #[test]
    #[serial]
    fn missing_database_url_is_an_error() {
        clear_setu_env();
        // Provide everything except the database URL. Use a temp cwd with no
        // setu.toml so the file cannot supply it.
        let dir = tempfile::tempdir().expect("tempdir");
        let orig_dir = std::env::current_dir().expect("cwd");
        std::env::set_current_dir(dir.path()).expect("chdir to tempdir");
        env::set_var("SETU_SECRET_KEY", "ab".repeat(32));
        env::set_var("SETU_PUBLIC_URL", "http://127.0.0.1:7000");
        let err = Config::load().expect_err("missing database url must fail");
        std::env::set_current_dir(orig_dir).expect("restore cwd");
        clear_setu_env();
        let msg = format!("{err}");
        assert!(
            msg.contains("DATABASE_URL") || msg.contains("database_url"),
            "error must mention database url, got: {msg}"
        );
    }
}
