//! Env resolution: materialise concrete env vars from a manifest.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use rand::rngs::StdRng;
use rand::{Rng, RngCore};

use crate::schema::{EnvSource, GenerateKind, Manifest};

/// One resolved env var.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    pub secret: bool,
    pub once: bool,
}

impl fmt::Debug for EnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.secret {
            f.debug_struct("EnvVar")
                .field("name", &self.name)
                .field("value", &"[redacted]")
                .field("secret", &self.secret)
                .field("once", &self.once)
                .finish()
        } else {
            f.debug_struct("EnvVar")
                .field("name", &self.name)
                .field("value", &self.value)
                .field("secret", &self.secret)
                .field("once", &self.once)
                .finish()
        }
    }
}

/// Inputs for env resolution.
pub struct ResolveInput {
    pub site_url: String,
    pub owner_email: String,
    pub port: u16,
    pub run_dir: PathBuf,
    pub data_dir: PathBuf,
    pub db_url: String,
    pub prompts: BTreeMap<String, String>,
    pub rng: StdRng,
}

impl fmt::Debug for ResolveInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let redacted_prompts: BTreeMap<&String, &str> =
            self.prompts.keys().map(|k| (k, "[redacted]")).collect();
        f.debug_struct("ResolveInput")
            .field("site_url", &self.site_url)
            .field("owner_email", &self.owner_email)
            .field("port", &self.port)
            .field("run_dir", &self.run_dir)
            .field("data_dir", &self.data_dir)
            .field("db_url", &"[redacted]")
            .field("prompts", &redacted_prompts)
            .field("rng", &"[rng]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResolveError {
    #[error("missing prompt value for {0}")]
    MissingPrompt(String),
    #[error("invalid manifest for {0}: {1}")]
    Invalid(String, String),
}

const PASSWORD_CHARSET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";

fn generate_hex32(rng: &mut StdRng) -> String {
    let mut buf = [0u8; 32];
    rng.fill_bytes(&mut buf);
    hex::encode(buf)
}

fn generate_password20(rng: &mut StdRng) -> String {
    (0..20)
        .map(|_| {
            let idx = rng.random_range(0..PASSWORD_CHARSET.len());
            PASSWORD_CHARSET[idx] as char
        })
        .collect()
}

/// Resolve every `[env]` entry to a concrete value. Iteration order is
/// sorted by variable name (the manifest stores `env` as a `BTreeMap`),
/// so output is deterministic given the same `rng` state.
pub fn resolve_env(
    manifest: &Manifest,
    input: &mut ResolveInput,
) -> Result<Vec<EnvVar>, ResolveError> {
    let mut out = Vec::with_capacity(manifest.env.len());
    for (name, spec) in &manifest.env {
        match spec {
            crate::schema::EnvSpec::Generate(gen) => {
                let value = match gen.generate {
                    GenerateKind::Hex32 => generate_hex32(&mut input.rng),
                    GenerateKind::Password20 => generate_password20(&mut input.rng),
                };
                out.push(EnvVar {
                    name: name.clone(),
                    value,
                    secret: gen.secret,
                    once: gen.once,
                });
            }
            crate::schema::EnvSpec::From(from) => {
                let value = match from.from {
                    EnvSource::PostgresUrl => input.db_url.clone(),
                    EnvSource::Dir => match &from.dir {
                        Some(dir) => {
                            // Defense in depth: validation already rejects
                            // absolute / `..` dirs; never join one outside
                            // data_dir here either.
                            debug_assert!(crate::validate::is_safe_dir(dir));
                            if !crate::validate::is_safe_dir(dir) {
                                return Err(ResolveError::Invalid(
                                    name.clone(),
                                    format!(
                                        "dir \"{dir}\" must be a relative path without \"..\" or leading \"/\""
                                    ),
                                ));
                            }
                            input.data_dir.join(dir).to_string_lossy().into_owned()
                        }
                        None => {
                            return Err(ResolveError::Invalid(
                                name.clone(),
                                "from \"dir\" requires dir = \"...\"".to_string(),
                            ));
                        }
                    },
                    EnvSource::RunDir => input.run_dir.to_string_lossy().into_owned(),
                    EnvSource::SiteUrl => input.site_url.clone(),
                    EnvSource::OwnerEmail => input.owner_email.clone(),
                    EnvSource::Port => input.port.to_string(),
                };
                out.push(EnvVar {
                    name: name.clone(),
                    value,
                    secret: from.secret,
                    once: false,
                });
            }
            crate::schema::EnvSpec::Value(v) => {
                out.push(EnvVar {
                    name: name.clone(),
                    value: v.value.clone(),
                    secret: false,
                    once: false,
                });
            }
            crate::schema::EnvSpec::Prompt(prompt) => {
                let value = input
                    .prompts
                    .get(name)
                    .cloned()
                    .ok_or_else(|| ResolveError::MissingPrompt(name.clone()))?;
                out.push(EnvVar {
                    name: name.clone(),
                    value,
                    secret: prompt.secret,
                    once: prompt.once,
                });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;

    const MINI: &str = r#"
schema = 1

[app]
id = "mini"
name = "Mini"
version = "0.1.0"
homepage = "https://example.com"
health = "/healthz"
port_env = "PORT"
admin_path = "/"

[run]
kind = "binary"

[run.binary]
args = []
"x86_64-linux" = { url = "https://example.com/mini.tar.gz", sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", exec = "mini" }

[needs]
dirs = ["media"]

[env]
MINI_HEX = { generate = "hex32", secret = true }
MINI_PW = { generate = "password20", secret = true, once = true }
MINI_DIR = { from = "dir", dir = "media" }
MINI_PORT = { from = "port" }

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = false

[update]
strategy = "replace"
"#;

    fn test_input(seed: u64) -> ResolveInput {
        ResolveInput {
            site_url: "https://mini.example.com".to_string(),
            owner_email: "owner@example.com".to_string(),
            port: 20001,
            run_dir: PathBuf::from("/srv/setu/sites/abc/run"),
            data_dir: PathBuf::from("/srv/setu/sites/abc/data"),
            db_url: "postgres://mini:secret@127.0.0.1:5432/mini".to_string(),
            prompts: BTreeMap::new(),
            rng: StdRng::seed_from_u64(seed),
        }
    }

    #[test]
    fn resolve_generates_stable_output_with_seeded_rng() {
        let manifest = crate::Manifest::parse(MINI).expect("mini parses");
        let mut a = test_input(42);
        let mut b = test_input(42);
        let va = resolve_env(&manifest, &mut a).expect("resolve a");
        let vb = resolve_env(&manifest, &mut b).expect("resolve b");
        assert_eq!(va, vb);

        let hex = va.iter().find(|v| v.name == "MINI_HEX").expect("hex");
        assert_eq!(hex.value.len(), 64);
        assert!(hex.value.chars().all(|c| c.is_ascii_hexdigit()));
        let pw = va.iter().find(|v| v.name == "MINI_PW").expect("pw");
        assert_eq!(pw.value.len(), 20);

        // A different seed must (overwhelmingly likely) differ.
        let mut c = test_input(7);
        let vc = resolve_env(&manifest, &mut c).expect("resolve c");
        assert_ne!(va, vc);
    }

    #[test]
    fn once_values_are_marked() {
        let manifest = crate::Manifest::parse(MINI).expect("mini parses");
        let mut input = test_input(1);
        let vars = resolve_env(&manifest, &mut input).expect("resolve");
        let pw = vars.iter().find(|v| v.name == "MINI_PW").expect("pw");
        assert!(pw.once);
        assert!(pw.secret);
        let hex = vars.iter().find(|v| v.name == "MINI_HEX").expect("hex");
        assert!(!hex.once);
    }

    #[test]
    fn prompt_missing_is_an_error() {
        let src = MINI.replace(
            "MINI_PORT = { from = \"port\" }",
            "MINI_PORT = { from = \"port\" }\nMINI_NAME = { prompt = \"Name\" }",
        );
        let manifest = crate::Manifest::parse(&src).expect("parses");
        let mut input = test_input(1);
        let err = resolve_env(&manifest, &mut input).expect_err("must fail");
        assert_eq!(err, ResolveError::MissingPrompt("MINI_NAME".to_string()));
    }
}
