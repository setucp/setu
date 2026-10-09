//! Dispatch coverage: every `Command` variant is handled, env files land
//! with mode 0600, and a redelivered `WriteUnit` is a no-op.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use setu_agent::{AgentConfig, CaddyFake, FakePostgres, FakeSystemd, NativeRuntime};
use setu_proto::{BinaryRef, Command, EnvVar, Limits, SiteSpec};

fn test_site(sites_root: &Path, site_id: &str, port: u16) -> SiteSpec {
    let dir = sites_root.join(site_id);
    SiteSpec {
        site_id: site_id.to_string(),
        short_id: site_id[site_id.len() - 8..].to_string(),
        user: format!("setu-site-{site_id}"),
        data_dir: dir.join("data"),
        run_dir: dir.join("run"),
        port,
        exec: dir.join("bin").join("current").join("app"),
        args: vec!["serve".to_string()],
        env_file: dir.join("env"),
        limits: Limits {
            memory_mb: 512,
            cpu_percent: 100,
            disk_mb: 5120,
        },
        health_path: "/healthz".to_string(),
        binary: None,
    }
}

fn test_runtime(
    systemd: FakeSystemd,
    sites_root: &Path,
    state_path: &Path,
) -> NativeRuntime<FakeSystemd, FakePostgres, CaddyFake> {
    let cfg = AgentConfig {
        sites_root: sites_root.to_path_buf(),
        state_path: state_path.to_path_buf(),
        chown_files: false,
        use_sudo_helper: false,
        ..AgentConfig::default()
    };
    NativeRuntime::new(systemd, FakePostgres::new(), CaddyFake::new(), cfg)
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
        use flate2::write::GzEncoder;
        use flate2::Compression;
        use std::io::Write;
        let mut enc = GzEncoder::new(&mut gz, Compression::default());
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

#[tokio::test]
async fn every_command_kind_is_handled() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sites_root = dir.path().join("sites");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(systemd.clone(), &sites_root, &dir.path().join("state.json"));
    let servers = start_servers().await;

    let id = "01jdispatch00000000000001";
    let mut site = test_site(&sites_root, id, servers.health_port);
    site.binary = Some(BinaryRef {
        url: servers.tarball_url.clone(),
        sha256: servers.tarball_sha.clone(),
        exec: "app".to_string(),
        version: "0.1.0".to_string(),
    });
    // Pre-create the version dir so SwitchVersion has something to point at.
    let version_dir = sites_root.join(id).join("bin").join("0.1.0");
    std::fs::create_dir_all(&version_dir).expect("mkdir version");
    std::fs::write(version_dir.join("app"), b"fake").expect("write fake exec");
    // A backup dir with a file so tar has something to archive.
    std::fs::create_dir_all(site.data_dir.join("media")).expect("mkdir media");
    std::fs::write(site.data_dir.join("media").join("a.txt"), b"data").expect("write data");

    let dest = dir.path().join("backups").join(id).join("ts1");
    let commands = vec![
        Command::CreateDatabase {
            db: "site_00000001".to_string(),
            role: "site_00000001".to_string(),
            password: "pw".to_string(),
        },
        Command::Install { site: site.clone() },
        Command::WriteUnit { site: site.clone() },
        Command::WriteEnv {
            site: site.clone(),
            env: vec![EnvVar {
                name: "APP_SECRET".to_string(),
                value: "s3cret".to_string(),
                secret: true,
                once: false,
            }],
        },
        Command::Start {
            site_id: id.to_string(),
        },
        Command::Status {
            site_id: id.to_string(),
        },
        Command::Health {
            site_id: id.to_string(),
            port: servers.health_port,
            path: "/healthz".to_string(),
            timeout_s: 5,
        },
        Command::SetRoute {
            domain: "example.com".to_string(),
            port: servers.health_port,
        },
        Command::Backup {
            site: site.clone(),
            db: "site_00000001".to_string(),
            dirs: vec!["media".to_string()],
            dest: dest.clone(),
        },
        Command::Restore {
            site: site.clone(),
            db: "site_00000001".to_string(),
            src: dest.clone(),
        },
        Command::FetchLogs {
            site_id: id.to_string(),
            lines: 10,
        },
        Command::Inventory,
        Command::SwitchVersion {
            site_id: id.to_string(),
            version: "0.1.0".to_string(),
        },
        Command::Restart {
            site_id: id.to_string(),
        },
        Command::Stop {
            site_id: id.to_string(),
        },
        Command::RemoveRoute {
            domain: "example.com".to_string(),
        },
        Command::Remove { site: site.clone() },
        Command::DropDatabase {
            db: "site_00000001".to_string(),
            role: "site_00000001".to_string(),
        },
    ];

    // 18 variants: every member of the Command enum.
    assert_eq!(commands.len(), 18, "test must cover every Command variant");

    // Remove the site dir first so Remove recreates-then-removes cleanly is
    // not needed: install already created it; order above installs first.
    for cmd in &commands {
        let kind = setu_agent::command_kind(cmd);
        // Re-create the site dir before Remove: earlier steps need it, and
        // Remove asserts nothing about prior existence.
        let result = setu_agent::handle("cmd-test-id", cmd, &rt).await;
        assert!(
            result.ok,
            "command {kind} must be handled ok, got error: {:?}",
            result.error
        );
        assert_eq!(result.id, "cmd-test-id");
    }

    // Spot-check side effects the dispatch must have produced.
    assert!(dest.join("db.dump").is_file(), "backup must dump the db");
    assert!(
        dest.join("files.tar.gz").is_file(),
        "backup must archive dirs"
    );
    assert_eq!(
        systemd.daemon_reload_count(),
        1,
        "exactly one unit write happened"
    );
}

#[tokio::test]
async fn write_env_sets_mode_0600() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sites_root = dir.path().join("sites");
    let rt = test_runtime(
        FakeSystemd::new(),
        &sites_root,
        &dir.path().join("state.json"),
    );
    let id = "01jenvtest000000000000001";
    let site = test_site(&sites_root, id, 20011);

    let result = setu_agent::handle(
        "cmd-env",
        &Command::WriteEnv {
            site: site.clone(),
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

    let contents = std::fs::read_to_string(&site.env_file).expect("read env");
    assert!(contents.contains("APP_PORT="), "env must name vars");
    assert!(contents.contains("APP_SECRET="), "env must name secrets");
    assert!(
        contents.contains("s3cret-value"),
        "file holds the real value (masking is an API-layer job)"
    );
    let mode = std::fs::metadata(&site.env_file)
        .expect("meta")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "env file must be mode 0600, got {mode:o}");
    // The result log must not leak the secret either.
    assert!(
        !result.log.contains("s3cret-value"),
        "handle log must not leak env values"
    );
}

#[tokio::test]
async fn redelivered_write_unit_is_a_noop_when_unchanged() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sites_root = dir.path().join("sites");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(systemd.clone(), &sites_root, &dir.path().join("state.json"));
    let id = "01jredeliver0000000000001";
    let site = test_site(&sites_root, id, 20012);

    let first = setu_agent::handle("cmd-1", &Command::WriteUnit { site: site.clone() }, &rt).await;
    assert!(first.ok, "first write_unit: {:?}", first.error);
    assert_eq!(systemd.daemon_reload_count(), 1);

    // Redelivery of the identical command: same bytes, no reload.
    let second = setu_agent::handle("cmd-1", &Command::WriteUnit { site: site.clone() }, &rt).await;
    assert!(second.ok, "redelivered write_unit: {:?}", second.error);
    assert_eq!(
        systemd.daemon_reload_count(),
        1,
        "unchanged redelivery must skip daemon-reload"
    );

    // A changed command (new limits) reloads exactly once more.
    let mut changed = site.clone();
    changed.limits.memory_mb = 1024;
    let third = setu_agent::handle("cmd-2", &Command::WriteUnit { site: changed }, &rt).await;
    assert!(third.ok, "changed write_unit: {:?}", third.error);
    assert_eq!(systemd.daemon_reload_count(), 2);
}
