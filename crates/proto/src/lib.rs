//! Agent command/result/heartbeat types (spec §5).
//!
//! The control plane talks to agents only through long-polled commands.
//! Every command is idempotent (the agent checks state before acting),
//! expires (`expires_at`, per kind: [`Command::default_timeout`]) and is
//! deduplicated by id on the agent. Nothing path-like crosses the wire:
//! the agent derives every path from the site's `short_id`.
//!
//! This crate is intentionally self-contained: the agent library must not
//! depend on manifest parsing, so [`Limits`], [`Workdir`] and [`EnvVar`]
//! here are local mirrors, not re-exports of `setu-manifest` types.
//!
//! `site` fields: [`SiteSpec`] where the agent needs the spec's content
//! (`SiteCreate` dirs, `WriteUnit` exec/args/workdir/limits, `Health`
//! port/path/grace); everywhere else the site's 8-char short id (ledger
//! R85), so commands for a site whose manifest or row is gone (delete,
//! `DeleteBackup` of a `pre_delete` copy) can still be built.

use std::fmt;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Resource limits for one site (mirrors the manifest `limits.default`;
/// kept local so the agent needs no manifest dependency).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Limits {
    pub memory_mb: u32,
    pub cpu_percent: u32,
    pub disk_mb: u32,
}

/// The unit's working directory (mirrors the manifest's `run.workdir`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
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

/// One downloadable binary release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BinaryRef {
    pub version: String,
    pub url: String,
    pub sha256: String,
    /// Path of the executable inside the archive (relative).
    pub exec: String,
}

/// One concrete env var to write into the site env file.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct EnvVar {
    pub name: String,
    pub value: String,
    pub secret: bool,
    pub once: bool,
}

impl fmt::Debug for EnvVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Logs never print env values (CLAUDE.md): always redact, even
        // for non-secret vars, so a `{:?}` in a job log cannot leak.
        f.debug_struct("EnvVar")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .field("secret", &self.secret)
            .field("once", &self.once)
            .finish()
    }
}

/// Everything the agent needs to materialise one site. No paths, no
/// user, no env file: the agent derives them from `short_id` (spec §4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SiteSpec {
    pub site_id: String,
    pub short_id: String,
    /// The version `bin/current` should run.
    pub version: String,
    pub port: u16,
    /// Executable inside `bin/<version>/` (relative, validated).
    pub exec: String,
    pub args: Vec<String>,
    pub workdir: Workdir,
    /// Directories under `data/` (manifest `needs.dirs`).
    pub dirs: Vec<String>,
    pub limits: Limits,
    /// Health path polled on `127.0.0.1:<port>`.
    pub health: String,
    pub startup_grace_s: u32,
}

/// Idempotent agent commands, JSON-tagged by `kind` (snake_case).
#[derive(Clone, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
    SiteCreate {
        site: SiteSpec,
    },
    CreateDatabase {
        db: String,
        role: String,
        password: String,
    },
    DropDatabase {
        db: String,
        role: String,
    },
    Install {
        site: String,
        binary: BinaryRef,
    },
    SwitchVersion {
        site: String,
        version: String,
    },
    WriteUnit {
        site: SiteSpec,
    },
    WriteEnv {
        site: String,
        env: Vec<EnvVar>,
    },
    Start {
        site: String,
    },
    Stop {
        site: String,
    },
    Restart {
        site: String,
    },
    Status {
        site: String,
    },
    Health {
        site: SiteSpec,
    },
    SetRoute {
        site_short_id: String,
        domain: String,
        port: u16,
    },
    RemoveRoute {
        site_short_id: String,
        domain: String,
    },
    Backup {
        site: String,
        db: String,
        dirs: Vec<String>,
        backup_id: String,
    },
    Restore {
        site: String,
        db: String,
        role: String,
        dirs: Vec<String>,
        backup_id: String,
    },
    DeleteBackup {
        site: String,
        backup_id: String,
    },
    SiteRemove {
        site: String,
    },
    FetchLogs {
        site: String,
        cursor: Option<String>,
        lines: u32,
    },
    Inventory,
}

/// `install` may download a large archive.
pub const INSTALL_TIMEOUT: Duration = Duration::from_secs(15 * 60);
/// `backup`/`restore` dump or replace a whole database.
pub const BACKUP_TIMEOUT: Duration = Duration::from_secs(60 * 60);
/// Everything else.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(2 * 60);
/// Added to `startup_grace_s` for `health`.
pub const HEALTH_SLACK: Duration = Duration::from_secs(30);

impl Command {
    /// The snake-case `kind` tag (the JSON discriminator).
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SiteCreate { .. } => "site_create",
            Self::CreateDatabase { .. } => "create_database",
            Self::DropDatabase { .. } => "drop_database",
            Self::Install { .. } => "install",
            Self::SwitchVersion { .. } => "switch_version",
            Self::WriteUnit { .. } => "write_unit",
            Self::WriteEnv { .. } => "write_env",
            Self::Start { .. } => "start",
            Self::Stop { .. } => "stop",
            Self::Restart { .. } => "restart",
            Self::Status { .. } => "status",
            Self::Health { .. } => "health",
            Self::SetRoute { .. } => "set_route",
            Self::RemoveRoute { .. } => "remove_route",
            Self::Backup { .. } => "backup",
            Self::Restore { .. } => "restore",
            Self::DeleteBackup { .. } => "delete_backup",
            Self::SiteRemove { .. } => "site_remove",
            Self::FetchLogs { .. } => "fetch_logs",
            Self::Inventory => "inventory",
        }
    }

    /// The site this command acts on (the agent's per-site FIFO key).
    /// `None` for server-level commands: `Inventory` and the database
    /// commands, which name a database, not a site (ledger R86).
    pub fn site_short_id(&self) -> Option<&str> {
        match self {
            Self::SiteCreate { site } | Self::WriteUnit { site } | Self::Health { site } => {
                Some(&site.short_id)
            }
            Self::Install { site, .. }
            | Self::SwitchVersion { site, .. }
            | Self::WriteEnv { site, .. }
            | Self::Start { site }
            | Self::Stop { site }
            | Self::Restart { site }
            | Self::Status { site }
            | Self::Backup { site, .. }
            | Self::Restore { site, .. }
            | Self::DeleteBackup { site, .. }
            | Self::SiteRemove { site }
            | Self::FetchLogs { site, .. } => Some(site),
            Self::SetRoute { site_short_id, .. } | Self::RemoveRoute { site_short_id, .. } => {
                Some(site_short_id)
            }
            Self::CreateDatabase { .. } | Self::DropDatabase { .. } | Self::Inventory => None,
        }
    }

    /// Commands that change nothing and take no per-site slot.
    pub fn is_read_only(&self) -> bool {
        matches!(
            self,
            Self::Status { .. } | Self::FetchLogs { .. } | Self::Inventory
        )
    }

    /// TTL of the command (its `expires_at`) and the control plane's
    /// wait: `install` 15 min, `health` grace + 30 s, `backup`/`restore`
    /// 60 min, everything else 2 min (spec §5).
    pub fn default_timeout(&self) -> Duration {
        match self {
            Self::Install { .. } => INSTALL_TIMEOUT,
            Self::Health { site } => {
                Duration::from_secs(u64::from(site.startup_grace_s)) + HEALTH_SLACK
            }
            Self::Backup { .. } | Self::Restore { .. } => BACKUP_TIMEOUT,
            _ => DEFAULT_TIMEOUT,
        }
    }

    /// Does this command carry secret values?
    pub fn has_secrets(&self) -> bool {
        matches!(self, Self::CreateDatabase { .. } | Self::WriteEnv { .. })
    }

    /// The JSON form with every secret nulled: the `create_database`
    /// password and each `write_env` value. The agent-side twin of the
    /// SQL function `setu_redact_command` (ledger R75); a redacted
    /// command deliberately does not deserialize back into a `Command`.
    pub fn redact(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or(serde_json::Value::Null);
        match self {
            Self::CreateDatabase { .. } => {
                if let Some(p) = v.get_mut("password") {
                    *p = serde_json::Value::Null;
                }
            }
            Self::WriteEnv { .. } => {
                if let Some(serde_json::Value::Array(vars)) = v.get_mut("env") {
                    for var in vars {
                        if let Some(value) = var.get_mut("value") {
                            *value = serde_json::Value::Null;
                        }
                    }
                }
            }
            _ => {}
        }
        v
    }
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // One redacted rendering for every variant: a new variant can
        // never forget to redact.
        f.debug_tuple("Command").field(&self.redact()).finish()
    }
}

/// A command in flight: stable `id` so redelivery is idempotent.
/// The wire has no delivery counter (ledger R85): the agent dedupes by
/// id; `agent_commands.attempts` stays a control-plane diagnostic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CommandEnvelope {
    pub id: String,
    pub site_short_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub command: Command,
}

/// The agent's report for one command id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct CommandResult {
    pub id: String,
    pub ok: bool,
    pub output: serde_json::Value,
    pub log: String,
    pub error: Option<String>,
}

/// One site unit's state as observed by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct UnitState {
    pub short_id: String,
    pub active: bool,
    pub sub: String,
    pub pid: Option<u32>,
    pub restarts: u32,
}

/// One per-site metrics sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct SiteMetric {
    pub short_id: String,
    pub at: DateTime<Utc>,
    pub cpu_seconds: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

/// Agent heartbeat posted every 15 s. `boot_id` changes whenever the
/// agent process (or the host) restarts.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Heartbeat {
    pub agent_version: String,
    pub boot_id: String,
    pub load1: f64,
    pub mem_free_mb: u64,
    pub disk_free_mb: u64,
    pub units: Vec<UnitState>,
    pub metrics: Vec<SiteMetric>,
}

/// First contact: the agent redeems a single-use join token with a
/// random `nonce` (a retry within 60 s with the same nonce gets the same
/// server token; another nonce is refused).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JoinRequest {
    pub join_token: String,
    pub nonce: String,
    pub hostname: String,
    pub arch: String,
    pub os: String,
    pub cpu_cores: u32,
    pub memory_mb: u64,
    pub disk_mb: u64,
}

impl fmt::Debug for JoinRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinRequest")
            .field("join_token", &"[redacted]")
            .field("nonce", &"[redacted]")
            .field("hostname", &self.hostname)
            .field("arch", &self.arch)
            .field("os", &self.os)
            .field("cpu_cores", &self.cpu_cores)
            .field("memory_mb", &self.memory_mb)
            .field("disk_mb", &self.disk_mb)
            .finish()
    }
}

/// Control-plane answer to a join: the long-lived server identity.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct JoinResponse {
    pub server_id: String,
    pub server_token: String,
}

impl fmt::Debug for JoinResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinResponse")
            .field("server_id", &self.server_id)
            .field("server_token", &"[redacted]")
            .finish()
    }
}

/// `Inventory` output: what the agent finds on the host.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct InventoryReport {
    pub units: Vec<UnitState>,
    /// Short ids with a tree under `/srv/setu/sites`.
    pub sites_on_disk: Vec<String>,
    /// Caddy route ids (`setu-site-<short>`, `setu-panel`).
    pub routes: Vec<String>,
    /// Databases named `site_<short>`.
    pub databases: Vec<String>,
    /// TCP ports in `LISTEN` on the host (`/proc/net/tcp{,6}`), sorted
    /// and unique: provision `validate` refuses a site port already in
    /// use by something other than the site's own unit.
    #[serde(default)]
    pub listening_ports: Vec<u16>,
}

/// `FetchLogs` output: lines after the given cursor and the next one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct LogsOutput {
    pub lines: Vec<String>,
    pub cursor: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const SECRET: &str = "SECRET-abc-123";

    fn test_site() -> SiteSpec {
        SiteSpec {
            site_id: "01J0000000000000000000001".to_string(),
            short_id: "abcd1234".to_string(),
            version: "0.1.0".to_string(),
            port: 20001,
            exec: "app".to_string(),
            args: vec!["serve".to_string()],
            workdir: Workdir::Data,
            dirs: vec!["media".to_string()],
            limits: Limits {
                memory_mb: 512,
                cpu_percent: 100,
                disk_mb: 5120,
            },
            health: "/healthz".to_string(),
            startup_grace_s: 60,
        }
    }

    fn test_env() -> Vec<EnvVar> {
        vec![
            EnvVar {
                name: "APP_SECRET".to_string(),
                value: SECRET.to_string(),
                secret: true,
                once: false,
            },
            EnvVar {
                name: "PLAIN".to_string(),
                value: format!("{SECRET}-plain"),
                secret: false,
                once: true,
            },
        ]
    }

    fn short() -> String {
        "abcd1234".to_string()
    }

    /// One of every variant, in declaration order, with its kind tag.
    fn all_commands() -> Vec<(Command, &'static str)> {
        let site = test_site();
        vec![
            (Command::SiteCreate { site: site.clone() }, "site_create"),
            (
                Command::CreateDatabase {
                    db: "site_abcd1234".to_string(),
                    role: "site_abcd1234".to_string(),
                    password: SECRET.to_string(),
                },
                "create_database",
            ),
            (
                Command::DropDatabase {
                    db: "site_abcd1234".to_string(),
                    role: "site_abcd1234".to_string(),
                },
                "drop_database",
            ),
            (
                Command::Install {
                    site: short(),
                    binary: BinaryRef {
                        version: "0.1.0".to_string(),
                        url: "https://example.com/app.tar.gz".to_string(),
                        sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                            .to_string(),
                        exec: "app".to_string(),
                    },
                },
                "install",
            ),
            (
                Command::SwitchVersion {
                    site: short(),
                    version: "0.1.1".to_string(),
                },
                "switch_version",
            ),
            (Command::WriteUnit { site: site.clone() }, "write_unit"),
            (
                Command::WriteEnv {
                    site: short(),
                    env: test_env(),
                },
                "write_env",
            ),
            (Command::Start { site: short() }, "start"),
            (Command::Stop { site: short() }, "stop"),
            (Command::Restart { site: short() }, "restart"),
            (Command::Status { site: short() }, "status"),
            (Command::Health { site: site.clone() }, "health"),
            (
                Command::SetRoute {
                    site_short_id: short(),
                    domain: "example.com".to_string(),
                    port: 20001,
                },
                "set_route",
            ),
            (
                Command::RemoveRoute {
                    site_short_id: short(),
                    domain: "example.com".to_string(),
                },
                "remove_route",
            ),
            (
                Command::Backup {
                    site: short(),
                    db: "site_abcd1234".to_string(),
                    dirs: vec!["media".to_string()],
                    backup_id: "01J0000000000000000000009".to_string(),
                },
                "backup",
            ),
            (
                Command::Restore {
                    site: short(),
                    db: "site_abcd1234".to_string(),
                    role: "site_abcd1234".to_string(),
                    dirs: vec!["media".to_string()],
                    backup_id: "01J0000000000000000000009".to_string(),
                },
                "restore",
            ),
            (
                Command::DeleteBackup {
                    site: short(),
                    backup_id: "01J0000000000000000000009".to_string(),
                },
                "delete_backup",
            ),
            (Command::SiteRemove { site: short() }, "site_remove"),
            (
                Command::FetchLogs {
                    site: short(),
                    cursor: Some("s=1;i=2".to_string()),
                    lines: 200,
                },
                "fetch_logs",
            ),
            (Command::Inventory, "inventory"),
        ]
    }

    /// Exhaustiveness guard: adding a variant without listing it in
    /// [`all_commands`] fails to compile here.
    fn listed(cmd: &Command) -> bool {
        match cmd {
            Command::SiteCreate { .. }
            | Command::CreateDatabase { .. }
            | Command::DropDatabase { .. }
            | Command::Install { .. }
            | Command::SwitchVersion { .. }
            | Command::WriteUnit { .. }
            | Command::WriteEnv { .. }
            | Command::Start { .. }
            | Command::Stop { .. }
            | Command::Restart { .. }
            | Command::Status { .. }
            | Command::Health { .. }
            | Command::SetRoute { .. }
            | Command::RemoveRoute { .. }
            | Command::Backup { .. }
            | Command::Restore { .. }
            | Command::DeleteBackup { .. }
            | Command::SiteRemove { .. }
            | Command::FetchLogs { .. }
            | Command::Inventory => true,
        }
    }

    #[test]
    fn every_command_round_trips_json() {
        let all = all_commands();
        assert_eq!(all.len(), 20, "spec §5 lists 20 commands");
        for (cmd, kind) in all {
            assert!(listed(&cmd));
            assert_eq!(cmd.kind(), kind);
            let value = serde_json::to_value(&cmd).expect("command must serialize");
            assert_eq!(value["kind"].as_str(), Some(kind), "kind tag for {kind}");
            let back: Command = serde_json::from_value(value).expect("command must deserialize");
            assert_eq!(back, cmd, "round trip for {kind}");
            let s = serde_json::to_string(&cmd).expect("to string");
            assert_eq!(
                serde_json::from_str::<Command>(&s).expect("from string"),
                cmd
            );
            if kind != "inventory" && kind != "create_database" && kind != "drop_database" {
                assert_eq!(cmd.site_short_id(), Some("abcd1234"), "{kind}");
            } else {
                assert_eq!(cmd.site_short_id(), None, "{kind}");
            }
        }
        // No path-like or identity field ever crosses the wire.
        let spec = serde_json::to_value(test_site()).expect("spec");
        for gone in [
            "user",
            "data_dir",
            "run_dir",
            "env_file",
            "binary",
            "health_path",
        ] {
            assert!(spec.get(gone).is_none(), "SiteSpec still has {gone}");
        }

        let at = Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap();
        let envelope = CommandEnvelope {
            id: "01J0000000000000000000002".to_string(),
            site_short_id: Some(short()),
            created_at: at,
            expires_at: at + chrono::Duration::minutes(2),
            command: Command::Status { site: short() },
        };
        let v = serde_json::to_value(&envelope).unwrap();
        assert!(v.get("attempts").is_none());
        assert_eq!(
            serde_json::from_value::<CommandEnvelope>(v).unwrap(),
            envelope
        );
        let hb = Heartbeat {
            agent_version: "0.1.0".to_string(),
            boot_id: "b1".to_string(),
            load1: 0.5,
            mem_free_mb: 1024,
            disk_free_mb: 2048,
            units: vec![UnitState {
                short_id: short(),
                active: true,
                sub: "running".to_string(),
                pid: Some(1234),
                restarts: 0,
            }],
            metrics: vec![SiteMetric {
                short_id: short(),
                at,
                cpu_seconds: 1.5,
                memory_bytes: 1024,
                disk_bytes: 2048,
            }],
        };
        let v = serde_json::to_value(&hb).unwrap();
        assert_eq!(serde_json::from_value::<Heartbeat>(v).unwrap(), hb);
        let inv = InventoryReport {
            units: hb.units.clone(),
            sites_on_disk: vec![short()],
            routes: vec!["setu-site-abcd1234".to_string()],
            databases: vec!["site_abcd1234".to_string()],
            listening_ports: vec![22, 20000],
        };
        let v = serde_json::to_value(&inv).unwrap();
        assert_eq!(serde_json::from_value::<InventoryReport>(v).unwrap(), inv);
    }

    #[test]
    fn redact_nulls_every_secret_variant() {
        let mut secret_bearing = 0;
        for (cmd, kind) in all_commands() {
            let redacted = cmd.redact();
            let text = redacted.to_string();
            assert!(!text.contains(SECRET), "{kind} leaks after redact: {text}");
            assert_eq!(redacted["kind"].as_str(), Some(kind));
            let plain = serde_json::to_string(&cmd).unwrap();
            if plain.contains(SECRET) {
                secret_bearing += 1;
                assert!(cmd.has_secrets(), "{kind} carries a secret");
                // A redacted secret command is not executable.
                assert!(serde_json::from_value::<Command>(redacted.clone()).is_err());
            } else {
                assert!(!cmd.has_secrets(), "{kind}");
                assert_eq!(redacted, serde_json::to_value(&cmd).unwrap(), "{kind}");
            }
            match kind {
                "create_database" => assert!(redacted["password"].is_null()),
                "write_env" => {
                    let env = redacted["env"].as_array().expect("env");
                    assert_eq!(env.len(), 2);
                    for var in env {
                        assert!(var["value"].is_null());
                        assert!(var["name"].is_string(), "names survive");
                    }
                }
                _ => {}
            }
        }
        assert_eq!(secret_bearing, 2);
    }

    #[test]
    fn debug_never_prints_secrets() {
        for (cmd, kind) in all_commands() {
            let dbg = format!("{cmd:?}");
            assert!(!dbg.contains(SECRET), "{kind} Debug leaked: {dbg}");
        }
        for var in test_env() {
            let dbg = format!("{var:?}");
            assert!(!dbg.contains(SECRET), "EnvVar leaked: {dbg}");
        }
        let join = JoinRequest {
            join_token: SECRET.to_string(),
            nonce: format!("{SECRET}-nonce"),
            hostname: "web1".to_string(),
            arch: "x86_64".to_string(),
            os: "linux".to_string(),
            cpu_cores: 4,
            memory_mb: 8192,
            disk_mb: 102400,
        };
        let dbg = format!("{join:?}");
        assert!(!dbg.contains(SECRET), "JoinRequest leaked: {dbg}");
        let resp = JoinResponse {
            server_id: "01J0000000000000000000003".to_string(),
            server_token: SECRET.to_string(),
        };
        let dbg = format!("{resp:?}");
        assert!(!dbg.contains(SECRET), "JoinResponse leaked: {dbg}");
        let at = Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap();
        for (cmd, _) in all_commands() {
            let envelope = CommandEnvelope {
                id: "01J0000000000000000000002".to_string(),
                site_short_id: None,
                created_at: at,
                expires_at: at,
                command: cmd,
            };
            let dbg = format!("{envelope:?}");
            assert!(!dbg.contains(SECRET), "envelope leaked: {dbg}");
        }
    }

    #[test]
    fn default_timeouts_by_kind() {
        let min = |m: u64| Duration::from_secs(m * 60);
        for (cmd, kind) in all_commands() {
            let want = match kind {
                "install" => min(15),
                "health" => Duration::from_secs(60 + 30),
                "backup" | "restore" => min(60),
                _ => min(2),
            };
            assert_eq!(cmd.default_timeout(), want, "{kind}");
        }
        let mut slow = test_site();
        slow.startup_grace_s = 300;
        assert_eq!(
            Command::Health { site: slow }.default_timeout(),
            Duration::from_secs(330)
        );
    }

    #[test]
    fn read_only_set_is_exact() {
        let read_only: Vec<&str> = all_commands()
            .into_iter()
            .filter(|(c, _)| c.is_read_only())
            .map(|(_, k)| k)
            .collect();
        assert_eq!(read_only, ["status", "fetch_logs", "inventory"]);
    }
}
