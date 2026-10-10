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
    pub admin_path: String,
    /// Seconds health may fail after a start before the start counts as
    /// failed (spec §2).
    #[serde(default = "default_startup_grace_s")]
    pub startup_grace_s: u32,
}

fn default_startup_grace_s() -> u32 {
    90
}

/// `[run]` section. `kind = "binary"` needs `[run.binary."x86_64-linux"]`;
/// `kind = "image"` (phase 2) is parsed and rejected at provisioning.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Run {
    pub kind: RunKind,
    #[serde(default)]
    pub workdir: Workdir,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<BinaryRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<ImageRun>,
}

impl Run {
    /// Is this an image manifest (not runnable in phase 1)?
    pub fn is_image(&self) -> bool {
        self.kind == RunKind::Image
    }

    /// The artefact for `target`, when this is a binary manifest.
    pub fn target(&self, target: Target) -> Option<&BinaryTarget> {
        match self.kind {
            RunKind::Binary => self.binary.as_ref()?.targets.get(&target),
            RunKind::Image => None,
        }
    }
}

/// `run.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunKind {
    Binary,
    Image,
}

/// `run.workdir`: the unit's working directory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Workdir {
    /// The directory holding `exec` inside `bin/current`.
    #[default]
    ExecDir,
    /// `bin/current` itself.
    Bin,
    /// The site's `data/` directory.
    Data,
}

/// Body of `[run.binary]`: one table per target.
#[derive(Debug, Clone)]
pub struct BinaryRun {
    pub targets: BTreeMap<Target, BinaryTarget>,
}

impl Serialize for BinaryRun {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(Some(self.targets.len()))?;
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
        let map = BTreeMap::<String, serde_json::Value>::deserialize(deserializer)?;
        let mut targets = BTreeMap::new();
        for (key, value) in map {
            let target = match key.as_str() {
                "x86_64-linux" => Target::X86_64Linux,
                "aarch64-linux" => Target::Aarch64Linux,
                other => {
                    return Err(serde::de::Error::unknown_field(
                        other,
                        &["x86_64-linux", "aarch64-linux"],
                    ));
                }
            };
            let entry = BinaryTarget::deserialize(value).map_err(serde::de::Error::custom)?;
            targets.insert(target, entry);
        }
        Ok(BinaryRun { targets })
    }
}

/// One downloadable binary.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BinaryTarget {
    pub url: String,
    pub sha256: String,
    /// `tar.gz` (default): `exec` is a path inside the archive.
    /// `binary`: the download is the executable and `exec` its file name.
    #[serde(default, skip_serializing_if = "BinaryFormat::is_default")]
    pub format: BinaryFormat,
    pub exec: String,
}

/// `run.binary.<target>.format`: what the download is.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
pub enum BinaryFormat {
    /// A gzip-compressed tar archive.
    #[default]
    #[serde(rename = "tar.gz")]
    TarGz,
    /// A bare executable.
    #[serde(rename = "binary")]
    Binary,
}

impl BinaryFormat {
    pub fn is_default(&self) -> bool {
        *self == Self::TarGz
    }
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
    "bind_addr",
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

        #[allow(clippy::items_after_statements)] // reviewed: kept as-is (R150)
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
                            "unknown from \"{s}\": expected one of \"bind_addr\", \"postgres_url\", \"dir\", \"run_dir\", \"site_url\", \"owner_email\", \"port\""
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
    /// `127.0.0.1:<port>`.
    #[serde(rename = "bind_addr")]
    BindAddr,
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
            EnvSource::BindAddr => "bind_addr",
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
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub strategy: Strategy,
}

/// `update.strategy`: `replace` is the only strategy in this version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Replace,
}

impl<'de> Deserialize<'de> for Strategy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        match s.as_str() {
            "replace" => Ok(Strategy::Replace),
            "command" => Err(serde::de::Error::custom(
                "update.strategy \"command\" is not supported in this version",
            )),
            other => Err(serde::de::Error::unknown_variant(other, &["replace"])),
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
