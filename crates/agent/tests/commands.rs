//! `dispatch::handle` over every `Command` variant, with the real helper
//! code running in-process on a tempdir root (`LocalHelper` + fakes).

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use setu_agent::helper::{Ctx, FakeSystemctl, FakeUsers, Recording};
use setu_agent::{AgentConfig, FakeCaddy, FakePostgres, LocalHelper, NativeRuntime, Paths};
use setu_proto::{
    BinaryRef, Command, EnvVar, InventoryReport, Limits, LogsOutput, SiteSpec, Workdir,
};

fn test_site(site_id: &str, port: u16) -> SiteSpec {
    let short = setu_common::short_id(site_id).to_lowercase();
    SiteSpec {
        site_id: site_id.to_string(),
        short_id: short,
        version: "0.1.0".to_string(),
        port,
        exec: "app".to_string(),
        args: vec!["serve".to_string()],
        workdir: Workdir::ExecDir,
        dirs: vec!["media".to_string()],
        limits: Limits {
            memory_mb: 512,
            cpu_percent: 100,
            disk_mb: 5120,
        },
        health: "/healthz".to_string(),
        startup_grace_s: 5,
    }
}

/// `<root>/srv/setu/sites/<short>` (the agent derives it; tests peek).
fn site_dir(root: &Path, site: &SiteSpec) -> PathBuf {
    root.join("srv/setu/sites").join(&site.short_id)
}

type Rt = NativeRuntime<LocalHelper, FakePostgres, FakeCaddy>;

fn test_runtime(root: &Path) -> (Rt, FakeSystemctl) {
    let units = FakeSystemctl::new();
    let ctx = Ctx {
        root: root.to_path_buf(),
        users: Box::new(FakeUsers::new()),
        own: Box::new(Recording::new()),
        units: Box::new(units.clone()),
    };
    let p = Paths::new(Some(root.to_path_buf()));
    let cfg = AgentConfig {
        state_path: p.state(),
        sites_root: p.sites(),
        downloads_dir: p.downloads(),
        backups_root: p.backups(),
        ..AgentConfig::default()
    };
    (
        NativeRuntime::new(
            LocalHelper::new(ctx),
            FakePostgres::new(),
            FakeCaddy::new(),
            cfg,
        ),
        units,
    )
}

fn make_tarball(name: &str, contents: &[u8]) -> Vec<u8> {
    let mut tar_data = Vec::new();
    {
        let mut builder = tar::Builder::new(&mut tar_data);
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        builder
            .append_data(&mut header, name, contents)
            .expect("tar append");
        builder.finish().expect("tar finish");
    }
    let mut gz = Vec::new();
    {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::default());
        enc.write_all(&tar_data).expect("gzip");
        enc.finish().expect("gzip finish");
    }
    gz
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(bytes))
}

struct Servers {
    tarball_url: String,
    tarball_sha: String,
    health_port: u16,
}

async fn start_servers() -> Servers {
    let tarball = make_tarball("app", b"#!/bin/sh\necho hello\n");
    let sha = sha256_hex(&tarball);
    let tar_app = axum::Router::new().route(
        "/app.tar.gz",
        axum::routing::get(move || {
            let body = tarball.clone();
            async move { axum::body::Body::from(body) }
        }),
    );
    let tar_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind tar");
    let tar_addr: SocketAddr = tar_listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(tar_listener, tar_app).await.expect("serve");
    });

    let health_app = axum::Router::new().route("/healthz", axum::routing::get(|| async { "ok" }));
    let health_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind health");
    let health_port = health_listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        axum::serve(health_listener, health_app)
            .await
            .expect("serve");
    });

    Servers {
        tarball_url: format!("http://{tar_addr}/app.tar.gz"),
        tarball_sha: sha,
        health_port,
    }
}

fn journalctl_available() -> bool {
    std::process::Command::new("journalctl")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
}

#[tokio::test]
async fn every_command_kind_is_handled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let (rt, units) = test_runtime(root);
    let servers = start_servers().await;

    let id = "01jdispatch00000000000001";
    let short = "00000001";
    let site = test_site(id, servers.health_port);
    let sdir = site_dir(root, &site);
    let data = sdir.join("data");
    let binary = BinaryRef {
        version: "0.1.0".to_string(),
        url: servers.tarball_url.clone(),
        sha256: servers.tarball_sha.clone(),
        exec: "app".to_string(),
    };
    let backup_id = "01jbackup0000000000000001";
    let s = || short.to_string();
    let commands = vec![
        Command::SiteCreate { site: site.clone() },
        Command::CreateDatabase {
            db: "site_00000001".to_string(),
            role: "site_00000001".to_string(),
            password: "pw".to_string(),
        },
        Command::Install {
            site: s(),
            binary: binary.clone(),
        },
        Command::SwitchVersion {
            site: s(),
            version: "0.1.0".to_string(),
        },
        Command::WriteUnit { site: site.clone() },
        Command::WriteEnv {
            site: s(),
            env: vec![EnvVar {
                name: "APP_SECRET".to_string(),
                value: "s3cret".to_string(),
                secret: true,
                once: false,
            }],
        },
        Command::Start { site: s() },
        Command::Status { site: s() },
        Command::Health { site: site.clone() },
        Command::SetRoute {
            site_short_id: s(),
            domain: "example.com".to_string(),
            port: servers.health_port,
        },
        Command::Backup {
            site: s(),
            db: "site_00000001".to_string(),
            dirs: vec!["media".to_string()],
            backup_id: backup_id.to_string(),
        },
        Command::Stop { site: s() },
        Command::Restore {
            site: s(),
            db: "site_00000001".to_string(),
            role: "site_00000001".to_string(),
            dirs: vec!["media".to_string()],
            backup_id: backup_id.to_string(),
        },
        Command::FetchLogs {
            site: s(),
            cursor: None,
            lines: 10,
        },
        Command::Inventory,
        Command::Restart { site: s() },
        Command::RemoveRoute {
            site_short_id: s(),
            domain: "example.com".to_string(),
        },
        Command::DeleteBackup {
            site: s(),
            backup_id: backup_id.to_string(),
        },
        Command::SiteRemove { site: s() },
        Command::DropDatabase {
            db: "site_00000001".to_string(),
            role: "site_00000001".to_string(),
        },
    ];
    assert_eq!(commands.len(), 20, "test must cover every Command variant");

    let backup_dir = root
        .join("var/lib/setu/backups")
        .join(short)
        .join(backup_id);
    for cmd in &commands {
        let kind = setu_agent::command_kind(cmd);
        match cmd {
            Command::Backup { .. } => {
                // A file so the archive has something in it.
                std::fs::write(data.join("media").join("a.txt"), b"data").expect("write data");
            }
            Command::Restore { .. } => {
                // Prove restore really swaps: change the live data first.
                std::fs::write(data.join("media").join("a.txt"), b"changed").expect("write");
            }
            _ => {}
        }
        let result = setu_agent::handle("cmd-test-id", cmd, &rt).await;
        if matches!(cmd, Command::FetchLogs { .. }) && !journalctl_available() {
            continue;
        }
        assert!(
            result.ok,
            "command {kind} must be handled ok, got error: {:?}",
            result.error
        );
        assert_eq!(result.id, "cmd-test-id");
        match cmd {
            Command::SiteCreate { .. } => {
                assert!(data.join("media").is_dir(), "needs.dirs from the spec");
            }
            Command::Install { .. } => {
                assert!(sdir.join("bin/0.1.0/app").is_file());
                assert!(std::fs::symlink_metadata(sdir.join("bin/current")).is_err());
            }
            Command::SwitchVersion { .. } => {
                assert_eq!(
                    std::fs::read_link(sdir.join("bin/current")).expect("current"),
                    PathBuf::from("0.1.0")
                );
            }
            Command::Backup { .. } => {
                assert!(
                    backup_dir.join("db.dump").is_file(),
                    "backup must dump the db"
                );
                assert!(
                    backup_dir.join("dirs.tar.zst").is_file(),
                    "backup must archive dirs"
                );
                assert!(result.output["size_bytes"].as_u64().unwrap_or(0) > 0);
            }
            Command::Restore { .. } => {
                assert_eq!(
                    std::fs::read(data.join("media").join("a.txt")).expect("read"),
                    b"data"
                );
                assert!(rt
                    .postgres()
                    .calls()
                    .contains(&"restore site_00000001 site_00000001".to_string()));
            }
            Command::FetchLogs { .. } => {
                let out: LogsOutput =
                    serde_json::from_value(result.output.clone()).expect("logs output");
                let _ = out.cursor;
            }
            Command::Inventory => {
                let inv: InventoryReport =
                    serde_json::from_value(result.output.clone()).expect("inventory");
                assert_eq!(inv.sites_on_disk, [short]);
                assert_eq!(inv.routes, [format!("setu-site-{short}")]);
                assert_eq!(inv.databases, ["site_00000001"]);
                assert_eq!(inv.units.len(), 1);
                assert_eq!(inv.units[0].short_id, short);
                assert!(!inv.units[0].active, "stopped before inventory");
            }
            Command::DeleteBackup { .. } => {
                assert!(!backup_dir.exists(), "delete_backup removes the dir");
                assert_eq!(result.output["existed"], true);
            }
            _ => {}
        }
    }
    assert!(!sdir.exists(), "site_remove deletes the tree");
    assert_eq!(units.reloads(), 2, "one unit write + one removal");
}

#[tokio::test]
async fn write_env_sets_mode_0600() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (rt, _) = test_runtime(dir.path());
    let id = "01jenvtest000000000000001";
    let site = test_site(id, 20011);
    let env_file = site_dir(dir.path(), &site).join("env");
    assert!(
        setu_agent::handle("c0", &Command::SiteCreate { site: site.clone() }, &rt)
            .await
            .ok
    );

    let result = setu_agent::handle(
        "cmd-env",
        &Command::WriteEnv {
            site: site.short_id.clone(),
            env: vec![
                EnvVar {
                    name: "APP_PORT".to_string(),
                    value: "20011".to_string(),
                    secret: false,
                    once: false,
                },
                EnvVar {
                    name: "APP_SECRET".to_string(),
                    value: "s3cret-value".to_string(),
                    secret: true,
                    once: true,
                },
            ],
        },
        &rt,
    )
    .await;
    assert!(result.ok, "write_env must succeed: {:?}", result.error);

    let contents = std::fs::read_to_string(&env_file).expect("read env");
    assert!(
        contents.contains("APP_PORT=\"20011\""),
        "env must name vars"
    );
    assert!(
        contents.contains("APP_SECRET=\"s3cret-value\""),
        "file holds the real value"
    );
    let mode = std::fs::metadata(&env_file)
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "env file must be mode 0600, got {mode:o}");
    assert!(
        !result.log.contains("s3cret-value"),
        "handle log must not leak env values"
    );

    // A newline in a value fails without echoing the value.
    let bad = setu_agent::handle(
        "cmd-env-bad",
        &Command::WriteEnv {
            site: site.short_id.clone(),
            env: vec![EnvVar {
                name: "A".to_string(),
                value: "x\nLEAKED=1".to_string(),
                secret: true,
                once: false,
            }],
        },
        &rt,
    )
    .await;
    assert!(!bad.ok);
    assert!(!bad.error.unwrap_or_default().contains("LEAKED"));
}

#[tokio::test]
async fn redelivered_write_unit_is_a_noop_when_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (rt, units) = test_runtime(dir.path());
    let id = "01jredeliver0000000000001";
    let site = test_site(id, 20012);

    let first = setu_agent::handle("cmd-1", &Command::WriteUnit { site: site.clone() }, &rt).await;
    assert!(first.ok, "first write_unit: {:?}", first.error);
    assert_eq!(first.output["changed"], true);
    assert_eq!(units.reloads(), 1);

    let second = setu_agent::handle("cmd-1", &Command::WriteUnit { site: site.clone() }, &rt).await;
    assert!(second.ok, "redelivered write_unit: {:?}", second.error);
    assert_eq!(second.output["changed"], false);
    assert_eq!(
        units.reloads(),
        1,
        "unchanged redelivery must skip daemon-reload"
    );

    let mut changed = site.clone();
    changed.limits.memory_mb = 1024;
    let third = setu_agent::handle("cmd-2", &Command::WriteUnit { site: changed }, &rt).await;
    assert!(third.ok, "changed write_unit: {:?}", third.error);
    assert_eq!(units.reloads(), 2);
}

/// `SetRoute`/`RemoveRoute` address the Caddy route `setu-site-<short>`
/// (spec §4); a malformed short id is refused.
#[tokio::test]
async fn route_commands_key_off_the_site_short_id() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (rt, _units) = test_runtime(dir.path());
    let set = Command::SetRoute {
        site_short_id: "abcd1234".to_string(),
        domain: "a.example.test".to_string(),
        port: 20001,
    };
    let r = setu_agent::handle("c1", &set, &rt).await;
    assert!(r.ok, "{:?}", r.error);
    let routes = rt.caddy().admin().routes();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0]["@id"], "setu-site-abcd1234");
    assert_eq!(
        routes[0]["handle"][0]["upstreams"][0]["dial"],
        "127.0.0.1:20001"
    );

    let bad_short = Command::RemoveRoute {
        site_short_id: "../etc".to_string(),
        domain: "a.example.test".to_string(),
    };
    assert!(!setu_agent::handle("c3", &bad_short, &rt).await.ok);
    let empty_short = Command::SetRoute {
        site_short_id: String::new(),
        domain: "b.example.test".to_string(),
        port: 20002,
    };
    assert!(!setu_agent::handle("c2", &empty_short, &rt).await.ok);

    let remove = Command::RemoveRoute {
        site_short_id: "abcd1234".to_string(),
        domain: "a.example.test".to_string(),
    };
    assert!(setu_agent::handle("c4", &remove, &rt).await.ok);
    assert!(
        setu_agent::handle("c5", &remove, &rt).await.ok,
        "idempotent"
    );
    assert!(rt.caddy().admin().routes().is_empty());
}

/// `DeleteBackup` refuses a hostile id and is idempotent.
#[tokio::test]
async fn delete_backup_refuses_traversal_and_repeats_ok() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (rt, _units) = test_runtime(dir.path());
    let victim = dir.path().join("var/lib/setu/backups/abcd1234/keep");
    std::fs::create_dir_all(&victim).expect("mkdir");
    for bad in ["../abcd1234/keep", "..", "a/b"] {
        let cmd = Command::DeleteBackup {
            site: "abcd1234".to_string(),
            backup_id: bad.to_string(),
        };
        assert!(!setu_agent::handle("d1", &cmd, &rt).await.ok, "{bad}");
    }
    let cmd = Command::DeleteBackup {
        site: "abcd1234".to_string(),
        backup_id: "gone".to_string(),
    };
    let r = setu_agent::handle("d2", &cmd, &rt).await;
    assert!(r.ok && r.output["existed"] == false);
    assert!(victim.is_dir());
}
