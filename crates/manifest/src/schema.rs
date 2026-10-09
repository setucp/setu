//! Manifest schema types mirroring spec §2.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Top-level manifest.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub app: App,
    pub run: Run,
    pub needs: Needs,
    #[serde(default)]
    pub env: BTreeMap<String, EnvSpec>,
    pub limits: LimitsSection,
    pub backup: Backup,
    pub update: Update,
}

/// `[app]` section.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct App {
    pub id: String,
    pub name: String,
    pub version: String,
    pub homepage: String,
    pub health: String,
    pub port_env: String,
    pub admin_path: String,
}

/// `[run]` section: binary (phase 1) or image (phase 2, parsed but
/// rejected later at provisioning).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum Run {
    #[serde(rename = "binary")]
    Binary {
        #[serde(rename = "binary")]
        binary: BinaryRun,
    },
    #[serde(rename = "image")]
    Image {
        #[serde(rename = "image")]
        image: ImageRun,
    },
}

impl<'de> Deserialize<'de> for Run {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Deserialize nested tables directly (not via `toml::Value`) so
        // inner errors keep their spans and `toml_error_to_manifest_error`
        // can report `[run.binary]` / `[run.image]` lines instead of `[run]`.
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Helper {
            kind: String,
            #[serde(default)]
            binary: Option<BinaryRun>,
            #[serde(default)]
            image: Option<ImageRun>,
        }

        let helper = Helper::deserialize(deserializer)?;
        match helper.kind.as_str() {
            "binary" => {
                let binary = helper
                    .binary
                    .ok_or_else(|| serde::de::Error::missing_field("binary"))?;
                Ok(Run::Binary { binary })
            }
            "image" => {
                let image = helper
                    .image
                    .ok_or_else(|| serde::de::Error::missing_field("image"))?;
                Ok(Run::Image { image })
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["binary", "image"],
            )),
        }
    }
}

/// Body of `[run.binary]`: dynamic target keys plus `args`.
#[derive(Debug, Clone)]
pub struct BinaryRun {
    pub args: Vec<String>,
    pub targets: BTreeMap<Target, BinaryTarget>,
}

impl Serialize for BinaryRun {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.targets.len() + 1))?;
        map.serialize_entry("args", &self.args)?;
        for (target, entry) in &self.targets {
            map.serialize_entry(&target.to_string(), entry)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for BinaryRun {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // NOTE: the intermediate is `serde_json::Value`, not `toml::Value`,
        // so manifests round-trip through the JSON stored in `app_versions`
        // (the control plane reads them back without TOML). `null` has no
        // TOML spelling, so `toml::Value` cannot represent stored manifests.
        let mut map = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let args = match map.remove("args") {
            Some(value) => Vec::<String>::deserialize(value).map_err(serde::de::Error::custom)?,
            None => Vec::new(),
        };
        let mut targets = BTreeMap::new();
        for (key, value) in map {
            let target = match key.as_str() {
                "x86_64-linux" => Target::X86_64Linux,
                "aarch64-linux" => Target::Aarch64Linux,
                other => {
                    return Err(serde::de::Error::unknown_field(
                        other,
                        &["x86_64-linux", "aarch64-linux", "args"],
                    ));
                }
            };
            let entry = BinaryTarget::deserialize(value).map_err(serde::de::Error::custom)?;
            targets.insert(target, entry);
        }
        Ok(BinaryRun { args, targets })
    }
}

/// One downloadable binary.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryTarget {
    pub url: String,
    pub sha256: String,
    pub exec: String,
}

/// Body of `[run.image]`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ImageRun {
    pub image: String,
}

/// Supported build targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
pub enum Target {
    #[serde(rename = "x86_64-linux")]
    X86_64Linux,
    #[serde(rename = "aarch64-linux")]
    Aarch64Linux,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Target::X86_64Linux => write!(f, "x86_64-linux"),
            Target::Aarch64Linux => write!(f, "aarch64-linux"),
        }
    }
}

/// `[needs]` section.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Needs {
    #[serde(default)]
    pub postgres: Option<semver::VersionReq>,
    #[serde(default)]
    pub dirs: Vec<String>,
}

/// One `[env]` entry: `generate`, `from`, `value` or `prompt`.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum EnvSpec {
    Generate(GenerateSpec),
    From(FromSpec),
    Value(ValueSpec),
    Prompt(PromptSpec),
}

const FROM_CHOICES: &[&str] = &[
    "postgres_url",
    "dir",
    "run_dir",
    "site_url",
    "owner_email",
    "port",
];

impl<'de> Deserialize<'de> for EnvSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;

        // Discriminate on the entry's keys so errors can name the closest
        // variant and the offending field instead of the generic
        // "data did not match any variant of untagged enum" message.
        // See above: `serde_json::Value` keeps stored (JSON) manifests
        // readable; TOML parsing is unaffected.
        let map =
            std::collections::BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let has_generate = map.contains_key("generate");
        let has_from = map.contains_key("from");
        let has_value = map.contains_key("value");
        let has_prompt = map.contains_key("label") || map.contains_key("prompt");
        let count = [has_generate, has_from, has_value, has_prompt]
            .iter()
            .filter(|b| **b)
            .count();

        if count == 0 {
            if let Some((key, _)) = map.iter().next() {
                return Err(D::Error::unknown_field(
                    key,
                    &["generate", "from", "value", "label", "prompt"],
                ));
            }
            return Err(D::Error::custom(
                "env entry must have one of \"generate\", \"from\", \"value\", \"label\"/\"prompt\"",
            ));
        }
        if count > 1 {
            return Err(D::Error::custom(
                "env entry must have only one of \"generate\", \"from\", \"value\", \"label\"/\"prompt\"",
            ));
        }

        fn table(map: std::collections::BTreeMap<String, serde_json::Value>) -> serde_json::Value {
            serde_json::Value::Object(map.into_iter().collect())
        }

        if has_generate {
            if let Some(v) = map.get("generate") {
                if let Some(s) = v.as_str() {
                    if s != "hex32" && s != "password20" {
                        return Err(D::Error::custom(format!(
                            "unknown generate \"{s}\": expected one of \"hex32\", \"password20\""
                        )));
                    }
                }
            }
            let value = table(map);
            return GenerateSpec::deserialize(value)
                .map(EnvSpec::Generate)
                .map_err(D::Error::custom);
        }
        if has_from {
            if let Some(v) = map.get("from") {
                if let Some(s) = v.as_str() {
                    if !FROM_CHOICES.contains(&s) {
                        return Err(D::Error::custom(format!(
                            "unknown from \"{s}\": expected one of \"postgres_url\", \"dir\", \"run_dir\", \"site_url\", \"owner_email\", \"port\""
                        )));
                    }
                }
            }
            let value = table(map);
            return FromSpec::deserialize(value)
                .map(EnvSpec::From)
                .map_err(D::Error::custom);
        }
        if has_value {
            let value = table(map);
            return ValueSpec::deserialize(value)
                .map(EnvSpec::Value)
                .map_err(D::Error::custom);
        }
        // Prompt variant (`label` or its `prompt` alias).
        let value = table(map);
        PromptSpec::deserialize(value)
            .map(EnvSpec::Prompt)
            .map_err(D::Error::custom)
    }
}

/// `{ generate = "hex32"|"password20", secret, once }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct GenerateSpec {
    pub generate: GenerateKind,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub once: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum GenerateKind {
    #[serde(rename = "hex32")]
    Hex32,
    #[serde(rename = "password20")]
    Password20,
}

/// `{ from = "...", dir?, secret }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FromSpec {
    pub from: EnvSource,
    #[serde(default)]
    pub dir: Option<String>,
    #[serde(default)]
    pub secret: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub enum EnvSource {
    #[serde(rename = "postgres_url")]
    PostgresUrl,
    #[serde(rename = "dir")]
    Dir,
    #[serde(rename = "run_dir")]
    RunDir,
    #[serde(rename = "site_url")]
    SiteUrl,
    #[serde(rename = "owner_email")]
    OwnerEmail,
    #[serde(rename = "port")]
    Port,
}

impl EnvSource {
    /// Manifest spelling used in user-facing errors (not Rust variant names).
    pub fn as_str(self) -> &'static str {
        match self {
            EnvSource::PostgresUrl => "postgres_url",
            EnvSource::Dir => "dir",
            EnvSource::RunDir => "run_dir",
            EnvSource::SiteUrl => "site_url",
            EnvSource::OwnerEmail => "owner_email",
            EnvSource::Port => "port",
        }
    }
}

/// `{ value = "..." }`.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValueSpec {
    pub value: String,
}

/// `{ label = "..." }` (also accepts `{ prompt = "..." }` as an alias
/// for the label); with `secret` and `once` modifiers.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PromptSpec {
    #[serde(alias = "prompt")]
    pub label: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub once: bool,
}

/// `[limits.default]` wrapper plus the limits themselves.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    pub default: Limits,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    pub memory_mb: u32,
    pub cpu_percent: u32,
    pub disk_mb: u32,
}

/// `[backup]` section.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Backup {
    pub postgres: bool,
    #[serde(default)]
    pub dirs: Vec<String>,
}

/// `[update]` section.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "strategy", rename_all = "lowercase")]
pub enum Update {
    Replace,
    Command { command: String },
}

impl<'de> Deserialize<'de> for Update {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Helper {
            strategy: String,
            #[serde(default)]
            command: Option<String>,
        }

        let helper = Helper::deserialize(deserializer)?;
        match helper.strategy.as_str() {
            "replace" => {
                if helper.command.is_some() {
                    return Err(serde::de::Error::unknown_field("command", &["strategy"]));
                }
                Ok(Update::Replace)
            }
            "command" => {
                let command = helper
                    .command
                    .ok_or_else(|| serde::de::Error::missing_field("command"))?;
                Ok(Update::Command { command })
            }
            other => Err(serde::de::Error::unknown_variant(
                other,
                &["replace", "command"],
            )),
        }
    }
}

/// One validation failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    /// TOML path, e.g. `app.id` or `env.FOO`.
    pub path: String,
    /// 1-based line number in the source, when known.
    pub line: Option<usize>,
    /// Full human-readable message (includes the path for readability,
    /// e.g. `app.id must match [a-z0-9-]`).
    pub message: String,
}

impl ManifestError {
    pub fn new(path: impl Into<String>, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            line,
            message: message.into(),
        }
    }
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            Some(l) => write!(f, "{} line {}", self.message, l),
            None => write!(f, "{}", self.message),
        }
    }
}

impl std::error::Error for ManifestError {}
