//! Agent command/result/heartbeat types.
//!
//! The control plane talks to agents only through long-polled commands
//! (spec §4). Every command is idempotent: the agent checks state before
//! acting, so the control plane may retry freely after timeouts.
//!
//! This crate is intentionally self-contained: the agent library must not
//! depend on manifest parsing, so [`Limits`] and [`EnvVar`] here are local
//! mirrors, not re-exports of `setu-manifest` types.

use std::fmt;
use std::path::PathBuf;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Resource limits for one site (mirrors the manifest `limits.default`;
/// kept local so the agent needs no manifest dependency).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub memory_mb: u32,
    pub cpu_percent: u32,
    pub disk_mb: u32,
}

/// One downloadable binary release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BinaryRef {
    pub url: String,
    pub sha256: String,
    pub exec: String,
    pub version: String,
}

/// One concrete env var to write into the site env file.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// Everything the agent needs to materialise one site.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SiteSpec {
    pub site_id: String,
    pub short_id: String,
    pub user: String,
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
    pub port: u16,
    pub exec: PathBuf,
    pub args: Vec<String>,
    pub env_file: PathBuf,
    pub limits: Limits,
    pub health_path: String,
    pub binary: Option<BinaryRef>,
}

/// Idempotent agent commands, JSON-tagged by `kind` (snake_case).
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Command {
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
        site: SiteSpec,
    },
    WriteUnit {
        site: SiteSpec,
    },
    WriteEnv {
        site: SiteSpec,
        env: Vec<EnvVar>,
    },
    Start {
        site_id: String,
    },
    Stop {
        site_id: String,
    },
    Restart {
        site_id: String,
    },
    Status {
        site_id: String,
    },
    Health {
        site_id: String,
        port: u16,
        path: String,
        timeout_s: u64,
    },
    SetRoute {
        domain: String,
        port: u16,
    },
    RemoveRoute {
        domain: String,
    },
    Backup {
        site: SiteSpec,
        db: String,
        dirs: Vec<String>,
        dest: PathBuf,
    },
    Restore {
        site: SiteSpec,
        db: String,
        src: PathBuf,
    },
    Remove {
        site: SiteSpec,
    },
    SwitchVersion {
        site_id: String,
        version: String,
    },
    FetchLogs {
        site_id: String,
        lines: u32,
    },
    Inventory,
}

impl fmt::Debug for Command {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CreateDatabase { db, role, .. } => f
                .debug_struct("CreateDatabase")
                .field("db", db)
                .field("role", role)
                .field("password", &"[redacted]")
                .finish(),
            Self::DropDatabase { db, role } => f
                .debug_struct("DropDatabase")
                .field("db", db)
                .field("role", role)
                .finish(),
            Self::Install { site } => f.debug_struct("Install").field("site", site).finish(),
            Self::WriteUnit { site } => f.debug_struct("WriteUnit").field("site", site).finish(),
            Self::WriteEnv { site, env } => {
                // Env values never reach logs: show names/flags only.
                let redacted: Vec<String> = env
                    .iter()
                    .map(|v| format!("{} (redacted)", v.name))
                    .collect();
                f.debug_struct("WriteEnv")
                    .field("site", site)
                    .field("env", &redacted)
                    .finish()
            }
            Self::Start { site_id } => f.debug_struct("Start").field("site_id", site_id).finish(),
            Self::Stop { site_id } => f.debug_struct("Stop").field("site_id", site_id).finish(),
            Self::Restart { site_id } => {
                f.debug_struct("Restart").field("site_id", site_id).finish()
            }
            Self::Status { site_id } => f.debug_struct("Status").field("site_id", site_id).finish(),
            Self::Health {
                site_id,
                port,
                path,
                timeout_s,
            } => f
                .debug_struct("Health")
                .field("site_id", site_id)
                .field("port", port)
                .field("path", path)
                .field("timeout_s", timeout_s)
                .finish(),
            Self::SetRoute { domain, port } => f
                .debug_struct("SetRoute")
                .field("domain", domain)
                .field("port", port)
                .finish(),
            Self::RemoveRoute { domain } => f
                .debug_struct("RemoveRoute")
                .field("domain", domain)
                .finish(),
            Self::Backup {
                site,
                db,
                dirs,
                dest,
            } => f
                .debug_struct("Backup")
                .field("site", site)
                .field("db", db)
                .field("dirs", dirs)
                .field("dest", dest)
                .finish(),
            Self::Restore { site, db, src } => f
                .debug_struct("Restore")
                .field("site", site)
                .field("db", db)
                .field("src", src)
                .finish(),
            Self::Remove { site } => f.debug_struct("Remove").field("site", site).finish(),
            Self::SwitchVersion { site_id, version } => f
                .debug_struct("SwitchVersion")
                .field("site_id", site_id)
                .field("version", version)
                .finish(),
            Self::FetchLogs { site_id, lines } => f
                .debug_struct("FetchLogs")
                .field("site_id", site_id)
                .field("lines", lines)
                .finish(),
            Self::Inventory => f.debug_struct("Inventory").finish(),
        }
    }
}

/// A command in flight: stable `id` so redelivery is idempotent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandEnvelope {
    pub id: String,
    pub created_at: DateTime<Utc>,
    pub command: Command,
}

/// The agent's report for one command id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommandResult {
    pub id: String,
    pub ok: bool,
    pub output: serde_json::Value,
    pub log: String,
    pub error: Option<String>,
}

/// One systemd unit's state as observed by the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitState {
    pub site_id: String,
    pub active: bool,
    pub sub: String,
    pub pid: Option<u32>,
}

/// One per-site metrics sample.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SiteMetric {
    pub site_id: String,
    pub at: DateTime<Utc>,
    pub cpu_seconds: f64,
    pub memory_bytes: u64,
    pub disk_bytes: u64,
}

/// Agent heartbeat posted every 15 s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Heartbeat {
    pub agent_version: String,
    pub load1: f64,
    pub mem_free_mb: u64,
    pub disk_free_mb: u64,
    pub units: Vec<UnitState>,
    pub metrics: Vec<SiteMetric>,
}

/// First contact: the agent redeems a single-use join token.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequest {
    pub join_token: String,
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn test_site() -> SiteSpec {
        SiteSpec {
            site_id: "01J0000000000000000000001".to_string(),
            short_id: "00000001".to_string(),
            user: "setu-site-01J0000000000000000000001".to_string(),
            data_dir: PathBuf::from("/srv/setu/sites/01J0000000000000000000001/data"),
            run_dir: PathBuf::from("/srv/setu/sites/01J0000000000000000000001/run"),
            port: 20001,
            exec: PathBuf::from("/srv/setu/sites/01J0000000000000000000001/bin/current/app"),
            args: vec!["serve".to_string()],
            env_file: PathBuf::from("/srv/setu/sites/01J0000000000000000000001/env"),
            limits: Limits {
                memory_mb: 512,
                cpu_percent: 100,
                disk_mb: 5120,
            },
            health_path: "/healthz".to_string(),
            binary: Some(BinaryRef {
                url: "https://example.com/app.tar.gz".to_string(),
                sha256: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                    .to_string(),
                exec: "app".to_string(),
                version: "0.1.0".to_string(),
            }),
        }
    }

    fn test_env() -> Vec<EnvVar> {
        vec![EnvVar {
            name: "APP_SECRET".to_string(),
            value: "SECRET-abc-123".to_string(),
            secret: true,
            once: false,
        }]
    }

    fn all_commands() -> Vec<(Command, &'static str)> {
        let site = test_site();
        vec![
            (
                Command::CreateDatabase {
                    db: "site_00000001".to_string(),
                    role: "site_00000001".to_string(),
                    password: "SECRET-abc-123".to_string(),
                },
                "create_database",
            ),
            (
                Command::DropDatabase {
                    db: "site_00000001".to_string(),
                    role: "site_00000001".to_string(),
                },
                "drop_database",
            ),
            (Command::Install { site: site.clone() }, "install"),
            (Command::WriteUnit { site: site.clone() }, "write_unit"),
            (
                Command::WriteEnv {
                    site: site.clone(),
                    env: test_env(),
                },
                "write_env",
            ),
            (
                Command::Start {
                    site_id: "01J0000000000000000000001".to_string(),
                },
                "start",
            ),
            (
                Command::Stop {
                    site_id: "01J0000000000000000000001".to_string(),
                },
                "stop",
            ),
            (
                Command::Restart {
                    site_id: "01J0000000000000000000001".to_string(),
                },
                "restart",
            ),
            (
                Command::Status {
                    site_id: "01J0000000000000000000001".to_string(),
                },
                "status",
            ),
            (
                Command::Health {
                    site_id: "01J0000000000000000000001".to_string(),
                    port: 20001,
                    path: "/healthz".to_string(),
                    timeout_s: 90,
                },
                "health",
            ),
            (
                Command::SetRoute {
                    domain: "example.com".to_string(),
                    port: 20001,
                },
                "set_route",
            ),
            (
                Command::RemoveRoute {
                    domain: "example.com".to_string(),
                },
                "remove_route",
            ),
            (
                Command::Backup {
                    site: site.clone(),
                    db: "site_00000001".to_string(),
                    dirs: vec!["media".to_string()],
                    dest: PathBuf::from("/var/lib/setu/backups/01J0000000000000000000001/ts"),
                },
                "backup",
            ),
            (
                Command::Restore {
                    site: site.clone(),
                    db: "site_00000001".to_string(),
                    src: PathBuf::from("/var/lib/setu/backups/01J0000000000000000000001/ts"),
                },
                "restore",
            ),
            (Command::Remove { site: site.clone() }, "remove"),
            (
                Command::SwitchVersion {
                    site_id: "01J0000000000000000000001".to_string(),
                    version: "0.1.1".to_string(),
                },
                "switch_version",
            ),
            (
                Command::FetchLogs {
                    site_id: "01J0000000000000000000001".to_string(),
                    lines: 200,
                },
                "fetch_logs",
            ),
            (Command::Inventory, "inventory"),
        ]
    }

    #[test]
    fn every_command_round_trips_json() {
        for (cmd, kind) in all_commands() {
            let value = serde_json::to_value(&cmd).expect("command must serialize");
            assert_eq!(
                value.get("kind").and_then(|k| k.as_str()),
                Some(kind),
                "kind tag for {kind}"
            );
            let back: Command =
                serde_json::from_value(value.clone()).expect("command must deserialize");
            assert_eq!(back, cmd, "round trip for {kind}");
            // String form round-trips too.
            let s = serde_json::to_string(&cmd).expect("to string");
            let back_str: Command = serde_json::from_str(&s).expect("from string");
            assert_eq!(back_str, cmd, "string round trip for {kind}");
        }
    }

    #[test]
    fn envelope_result_heartbeat_join_round_trip() {
        let at = Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap();
        let envelope = CommandEnvelope {
            id: "01J0000000000000000000002".to_string(),
            created_at: at,
            command: Command::Status {
                site_id: "01J0000000000000000000001".to_string(),
            },
        };
        let v = serde_json::to_value(&envelope).unwrap();
        assert_eq!(
            serde_json::from_value::<CommandEnvelope>(v).unwrap(),
            envelope
        );

        let result = CommandResult {
            id: envelope.id.clone(),
            ok: true,
            output: serde_json::json!({"active": true}),
            log: "ok".to_string(),
            error: None,
        };
        let v = serde_json::to_value(&result).unwrap();
        assert_eq!(serde_json::from_value::<CommandResult>(v).unwrap(), result);

        let hb = Heartbeat {
            agent_version: "0.1.0".to_string(),
            load1: 0.5,
            mem_free_mb: 1024,
            disk_free_mb: 2048,
            units: vec![UnitState {
                site_id: "01J0000000000000000000001".to_string(),
                active: true,
                sub: "running".to_string(),
                pid: Some(1234),
            }],
            metrics: vec![SiteMetric {
                site_id: "01J0000000000000000000001".to_string(),
                at,
                cpu_seconds: 1.5,
                memory_bytes: 1024,
                disk_bytes: 2048,
            }],
        };
        let v = serde_json::to_value(&hb).unwrap();
        assert_eq!(serde_json::from_value::<Heartbeat>(v).unwrap(), hb);

        let join = JoinRequest {
            join_token: "SECRET-abc-123".to_string(),
            hostname: "web1".to_string(),
            arch: "x86_64".to_string(),
            os: "linux".to_string(),
            cpu_cores: 4,
            memory_mb: 8192,
            disk_mb: 102400,
        };
        let v = serde_json::to_value(&join).unwrap();
        assert_eq!(serde_json::from_value::<JoinRequest>(v).unwrap(), join);

        let resp = JoinResponse {
            server_id: "01J0000000000000000000003".to_string(),
            server_token: "SECRET-abc-123".to_string(),
        };
        let v = serde_json::to_value(&resp).unwrap();
        assert_eq!(serde_json::from_value::<JoinResponse>(v).unwrap(), resp);
    }

    #[test]
    fn secret_fields_are_redacted_in_debug() {
        let create = Command::CreateDatabase {
            db: "site_00000001".to_string(),
            role: "site_00000001".to_string(),
            password: "SECRET-abc-123".to_string(),
        };
        let dbg = format!("{create:?}");
        assert!(!dbg.contains("SECRET-abc-123"), "password leaked: {dbg}");

        let write_env = Command::WriteEnv {
            site: test_site(),
            env: test_env(),
        };
        let dbg = format!("{write_env:?}");
        assert!(!dbg.contains("SECRET-abc-123"), "env value leaked: {dbg}");

        let env_var = EnvVar {
            name: "APP_SECRET".to_string(),
            value: "SECRET-abc-123".to_string(),
            secret: true,
            once: false,
        };
        let dbg = format!("{env_var:?}");
        assert!(!dbg.contains("SECRET-abc-123"), "EnvVar leaked: {dbg}");

        let join = JoinRequest {
            join_token: "SECRET-abc-123".to_string(),
            hostname: "web1".to_string(),
            arch: "x86_64".to_string(),
            os: "linux".to_string(),
            cpu_cores: 4,
            memory_mb: 8192,
            disk_mb: 102400,
        };
        let dbg = format!("{join:?}");
        assert!(!dbg.contains("SECRET-abc-123"), "join token leaked: {dbg}");

        let resp = JoinResponse {
            server_id: "01J0000000000000000000003".to_string(),
            server_token: "SECRET-abc-123".to_string(),
        };
        let dbg = format!("{resp:?}");
        assert!(
            !dbg.contains("SECRET-abc-123"),
            "server token leaked: {dbg}"
        );

        // The envelope wraps a secret-bearing command: still redacted.
        let envelope = CommandEnvelope {
            id: "01J0000000000000000000002".to_string(),
            created_at: Utc.with_ymd_and_hms(2026, 10, 9, 12, 0, 0).unwrap(),
            command: create,
        };
        let dbg = format!("{envelope:?}");
        assert!(!dbg.contains("SECRET-abc-123"), "envelope leaked: {dbg}");
    }
}
