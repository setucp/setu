//! Env resolution: materialise concrete env vars from a manifest.

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

use rand::rngs::StdRng;
use rand::{Rng, RngCore};

use crate::schema::{EnvSource, EnvSpec, GenerateKind, Manifest};

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

/// Resolve one rule. `postgres_url` is always secret; prompt answers
/// and `value`s must not contain control characters (env-file safety).
fn resolve_one(
    name: &str,
    spec: &EnvSpec,
    input: &mut ResolveInput,
) -> Result<EnvVar, ResolveError> {
    let var = |value: String, secret: bool, once: bool| EnvVar {
        name: name.to_string(),
        value,
        secret,
        once,
    };
    let out = match spec {
        EnvSpec::Generate(gen) => {
            let value = match gen.generate {
                GenerateKind::Hex32 => generate_hex32(&mut input.rng),
                GenerateKind::Password20 => generate_password20(&mut input.rng),
            };
            var(value, gen.secret, gen.once)
        }
        EnvSpec::From(from) => {
            let value = match from.from {
                EnvSource::BindAddr => format!("127.0.0.1:{}", input.port),
                EnvSource::PostgresUrl => input.db_url.clone(),
                EnvSource::Dir => match &from.dir {
                    // Defense in depth: validation already enforces the dir
                    // regex; never join anything else onto data_dir.
                    Some(dir) if setu_common::validate::dir(dir) => {
                        input.data_dir.join(dir).to_string_lossy().into_owned()
                    }
                    Some(dir) => {
                        return Err(ResolveError::Invalid(
                            name.to_string(),
                            format!("dir \"{dir}\" must match [a-z][a-z0-9-]{{0,31}}"),
                        ));
                    }
                    None => {
                        return Err(ResolveError::Invalid(
                            name.to_string(),
                            "from \"dir\" requires dir = \"...\"".to_string(),
                        ));
                    }
                },
                EnvSource::RunDir => input.run_dir.to_string_lossy().into_owned(),
                EnvSource::SiteUrl => input.site_url.clone(),
                EnvSource::OwnerEmail => input.owner_email.clone(),
                EnvSource::Port => input.port.to_string(),
            };
            let secret = from.secret || from.from == EnvSource::PostgresUrl;
            var(value, secret, false)
        }
        EnvSpec::Value(v) => var(v.value.clone(), false, false),
        EnvSpec::Prompt(prompt) => {
            let value = input
                .prompts
                .get(name)
                .cloned()
                .ok_or_else(|| ResolveError::MissingPrompt(name.to_string()))?;
            var(value, prompt.secret, prompt.once)
        }
    };
    if !setu_common::validate::no_control(&out.value) {
        return Err(ResolveError::Invalid(
            name.to_string(),
            "value must not contain control characters".to_string(),
        ));
    }
    Ok(out)
}

/// Resolve every `[env]` entry to a concrete value. Iteration order is
/// sorted by variable name (the manifest stores `env` as a `BTreeMap`),
/// so output is deterministic given the same `rng` state.
pub fn resolve_env(
    manifest: &Manifest,
    input: &mut ResolveInput,
) -> Result<Vec<EnvVar>, ResolveError> {
    manifest
        .env
        .iter()
        .map(|(name, spec)| resolve_one(name, spec, input))
        .collect()
}

/// Env for an update (spec §6): `existing` is the site's stored env,
/// `new_manifest` the target version. `generate` and `prompt` values are
/// kept by name (an update never rotates them), `value` and `from` rules
/// are re-resolved from `input`, rules new to the manifest are resolved
/// fresh, rules the manifest dropped are dropped, and `once` rules are
/// never emitted (their value was consumed on first boot).
pub fn merge(
    existing: &[EnvVar],
    new_manifest: &Manifest,
    input: &mut ResolveInput,
) -> Result<Vec<EnvVar>, ResolveError> {
    let mut out = Vec::with_capacity(new_manifest.env.len());
    for (name, spec) in &new_manifest.env {
        let (sticky, once) = match spec {
            EnvSpec::Generate(g) => (true, g.once),
            EnvSpec::Prompt(p) => (true, p.once),
            EnvSpec::From(_) | EnvSpec::Value(_) => (false, false),
        };
        if once {
            continue;
        }
        let kept = existing.iter().find(|v| &v.name == name);
        match (sticky, kept) {
            (true, Some(old)) => {
                let mut fresh = old.clone();
                fresh.secret = match spec {
                    EnvSpec::Generate(g) => g.secret,
                    EnvSpec::Prompt(p) => p.secret,
                    _ => old.secret,
                };
                fresh.once = false;
                out.push(fresh);
            }
            _ => out.push(resolve_one(name, spec, input)?),
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
admin_path = "/"

[run]
kind = "binary"

[run.binary."x86_64-linux"]
url = "https://example.com/mini.tar.gz"
sha256 = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
exec = "mini"

[needs]
dirs = ["media"]

[env]
MINI_HEX = { generate = "hex32", secret = true }
MINI_PW = { generate = "password20", secret = true, once = true }
MINI_DIR = { from = "dir", dir = "media" }
MINI_BIND = { from = "bind_addr" }
MINI_DB = { from = "postgres_url" }
MINI_NAME = { prompt = "Name" }
MINI_MODE = { value = "prod" }

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
            prompts: BTreeMap::from([("MINI_NAME".to_string(), "Ada".to_string())]),
            rng: StdRng::seed_from_u64(seed),
        }
    }

    fn mini() -> Manifest {
        Manifest::parse(MINI).expect("mini parses")
    }

    fn get<'a>(vars: &'a [EnvVar], name: &str) -> &'a EnvVar {
        vars.iter()
            .find(|v| v.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    }

    #[test]
    fn resolve_is_deterministic_with_seeded_rng() {
        let manifest = mini();
        let va = resolve_env(&manifest, &mut test_input(42)).expect("resolve a");
        let vb = resolve_env(&manifest, &mut test_input(42)).expect("resolve b");
        assert_eq!(va, vb);
        let hex = get(&va, "MINI_HEX");
        assert_eq!(hex.value.len(), 64);
        assert!(hex.value.chars().all(|c| c.is_ascii_hexdigit()));
        let vc = resolve_env(&manifest, &mut test_input(7)).expect("resolve c");
        assert_ne!(va, vc);
        assert_eq!(get(&va, "MINI_DIR").value, "/srv/setu/sites/abc/data/media");
        assert_eq!(get(&va, "MINI_MODE").value, "prod");
    }

    #[test]
    fn password20_is_alphanumeric() {
        let manifest = mini();
        for seed in 0..50 {
            let vars = resolve_env(&manifest, &mut test_input(seed)).expect("resolve");
            let pw = &get(&vars, "MINI_PW").value;
            assert_eq!(pw.len(), 20);
            assert!(pw.chars().all(|c| c.is_ascii_alphanumeric()), "{seed}");
        }
    }

    #[test]
    fn bind_addr_is_loopback_with_port() {
        let vars = resolve_env(&mini(), &mut test_input(1)).expect("resolve");
        let bind = get(&vars, "MINI_BIND");
        assert_eq!(bind.value, "127.0.0.1:20001");
        assert!(!bind.secret);
    }

    #[test]
    fn postgres_url_is_always_secret() {
        let vars = resolve_env(&mini(), &mut test_input(1)).expect("resolve");
        let db = get(&vars, "MINI_DB");
        assert!(
            db.secret,
            "postgres_url must be secret without the modifier"
        );
        assert!(!format!("{db:?}").contains("secret@"));
    }

    #[test]
    fn once_values_are_flagged() {
        let vars = resolve_env(&mini(), &mut test_input(1)).expect("resolve");
        let pw = get(&vars, "MINI_PW");
        assert!(pw.once && pw.secret);
        assert!(!get(&vars, "MINI_HEX").once);
    }

    #[test]
    fn missing_prompt_is_an_error() {
        let mut input = test_input(1);
        input.prompts.clear();
        let err = resolve_env(&mini(), &mut input).expect_err("must fail");
        assert_eq!(err, ResolveError::MissingPrompt("MINI_NAME".to_string()));
    }

    #[test]
    fn prompt_with_control_character_is_refused() {
        let mut input = test_input(1);
        input
            .prompts
            .insert("MINI_NAME".to_string(), "Ada\nEVIL=1".to_string());
        let err = resolve_env(&mini(), &mut input).expect_err("must fail");
        assert!(matches!(err, ResolveError::Invalid(name, _) if name == "MINI_NAME"));
    }

    #[test]
    fn merge_keeps_generated_and_prompt_values() {
        let manifest = mini();
        let existing = resolve_env(&manifest, &mut test_input(1)).expect("resolve");
        // A later version, a new port and new prompt answers: generated and
        // prompt values must survive, derived ones follow the input.
        let mut input = test_input(2);
        input.port = 20002;
        input.prompts.insert("MINI_NAME".into(), "Grace".into());
        let merged = merge(&existing, &manifest, &mut input).expect("merge");
        assert_eq!(get(&merged, "MINI_HEX"), get(&existing, "MINI_HEX"));
        assert_eq!(get(&merged, "MINI_NAME").value, "Ada");
        assert_eq!(get(&merged, "MINI_BIND").value, "127.0.0.1:20002");
    }

    #[test]
    fn merge_adds_new_and_drops_removed() {
        let existing = resolve_env(&mini(), &mut test_input(1)).expect("resolve");
        let next = MINI
            .replace("MINI_MODE = { value = \"prod\" }\n", "")
            .replace(
                "MINI_NAME = { prompt = \"Name\" }",
                "MINI_NAME = { prompt = \"Name\" }\nMINI_NEW = { generate = \"hex32\", secret = true }\nMINI_URL = { from = \"site_url\" }",
            );
        let next = Manifest::parse(&next).expect("next parses");
        let merged = merge(&existing, &next, &mut test_input(9)).expect("merge");
        assert!(merged.iter().all(|v| v.name != "MINI_MODE"));
        assert_eq!(get(&merged, "MINI_NEW").value.len(), 64);
        assert_eq!(get(&merged, "MINI_URL").value, "https://mini.example.com");
        assert_eq!(get(&merged, "MINI_HEX"), get(&existing, "MINI_HEX"));
    }

    #[test]
    fn merge_never_emits_once() {
        let existing = resolve_env(&mini(), &mut test_input(1)).expect("resolve");
        assert!(existing.iter().any(|v| v.once));
        let merged = merge(&existing, &mini(), &mut test_input(1)).expect("merge");
        assert!(merged.iter().all(|v| !v.once && v.name != "MINI_PW"));
        // Not even when the existing env never had it.
        let merged = merge(&[], &mini(), &mut test_input(1)).expect("merge");
        assert!(merged.iter().all(|v| v.name != "MINI_PW"));
    }
}
