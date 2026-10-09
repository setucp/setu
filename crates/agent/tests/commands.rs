//! `commands::handle` (spec §5 semantics) and `dispatch::execute` over
//! every `Command` variant, with the real helper code running in-process
//! on a tempdir root (`LocalHelper` + fakes), plus the loop, heartbeat
//! and health/log behaviour over fakes.

use std::collections::VecDeque;
use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use setu_agent::helper::{Ctx, FakeSystemctl, FakeUsers, Recording, Verb};
use setu_agent::run_loop::{Backoff, ControlPlane};
use setu_agent::{
    Agent, AgentConfig, FakeCaddy, FakeHelper, FakePostgres, LocalHelper, NativeRuntime, Paths,
};
use setu_proto::{
    BinaryRef, Command, CommandEnvelope, CommandResult, EnvVar, InventoryReport, Limits,
    LogsOutput, SiteSpec, Workdir,
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

/// An envelope expiring in two minutes.
fn envelope(id: &str, command: Command) -> CommandEnvelope {
    let now = chrono::Utc::now();
    CommandEnvelope {
        id: id.to_string(),
        site_short_id: command.site_short_id().map(str::to_string),
        created_at: now,
        expires_at: now + chrono::Duration::minutes(2),
        command,
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
    let agent = setu_agent::Agent::new(rt);
    let rt = agent.runtime();
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
    for (i, cmd) in commands.iter().enumerate() {
        let kind = setu_agent::command_kind(cmd);
        let cmd_id = format!("cmd-{i}");
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
        let result = setu_agent::handle(&envelope(&cmd_id, cmd.clone()), &agent).await;
        if matches!(cmd, Command::FetchLogs { .. }) && !journalctl_available() {
            continue;
        }
        assert!(
            result.ok,
            "command {kind} must be handled ok, got error: {:?}",
            result.error
        );
        assert_eq!(result.id, cmd_id);
        assert_eq!(
            rt.state().cached(&cmd_id).as_ref(),
            Some(&result),
            "{kind}: persisted"
        );
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
            Command::SetRoute { .. } => {
                let domain = rt.state().read(|st| st.sites[short].domain.clone());
                assert_eq!(domain.as_deref(), Some("example.com"), "domain recorded");
            }
            Command::RemoveRoute { .. } => {
                let domain = rt.state().read(|st| st.sites[short].domain.clone());
                assert_eq!(domain, None, "domain cleared");
            }
            Command::DeleteBackup { .. } => {
                assert!(!backup_dir.exists(), "delete_backup removes the dir");
                assert_eq!(result.output["existed"], true);
            }
            _ => {}
        }
    }
    assert!(!sdir.exists(), "site_remove deletes the tree");
    assert!(
        rt.state().read(|st| st.sites.is_empty()),
        "site_remove forgets the site"
    );
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
        setu_agent::execute("c0", &Command::SiteCreate { site: site.clone() }, &rt)
            .await
            .ok
    );

    let result = setu_agent::execute(
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
    let bad = setu_agent::execute(
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

    let first = setu_agent::execute("cmd-1", &Command::WriteUnit { site: site.clone() }, &rt).await;
    assert!(first.ok, "first write_unit: {:?}", first.error);
    assert_eq!(first.output["changed"], true);
    assert_eq!(units.reloads(), 1);

    let second =
        setu_agent::execute("cmd-1", &Command::WriteUnit { site: site.clone() }, &rt).await;
    assert!(second.ok, "redelivered write_unit: {:?}", second.error);
    assert_eq!(second.output["changed"], false);
    assert_eq!(
        units.reloads(),
        1,
        "unchanged redelivery must skip daemon-reload"
    );

    let mut changed = site.clone();
    changed.limits.memory_mb = 1024;
    let third = setu_agent::execute("cmd-2", &Command::WriteUnit { site: changed }, &rt).await;
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
    let r = setu_agent::execute("c1", &set, &rt).await;
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
    assert!(!setu_agent::execute("c3", &bad_short, &rt).await.ok);
    let empty_short = Command::SetRoute {
        site_short_id: String::new(),
        domain: "b.example.test".to_string(),
        port: 20002,
    };
    assert!(!setu_agent::execute("c2", &empty_short, &rt).await.ok);

    let remove = Command::RemoveRoute {
        site_short_id: "abcd1234".to_string(),
        domain: "a.example.test".to_string(),
    };
    assert!(setu_agent::execute("c4", &remove, &rt).await.ok);
    assert!(
        setu_agent::execute("c5", &remove, &rt).await.ok,
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
        assert!(!setu_agent::execute("d1", &cmd, &rt).await.ok, "{bad}");
    }
    let cmd = Command::DeleteBackup {
        site: "abcd1234".to_string(),
        backup_id: "gone".to_string(),
    };
    let r = setu_agent::execute("d2", &cmd, &rt).await;
    assert!(r.ok && r.output["existed"] == false);
    assert!(victim.is_dir());
}

// ---------------------------------------------------------------------
// Spec §5 command semantics over the fake helper.

type FakeRt = NativeRuntime<FakeHelper, FakePostgres, FakeCaddy>;

fn fake_config(root: &Path) -> AgentConfig {
    let p = Paths::new(Some(root.to_path_buf()));
    AgentConfig {
        state_path: p.state(),
        sites_root: p.sites(),
        downloads_dir: p.downloads(),
        backups_root: p.backups(),
        ..AgentConfig::default()
    }
}

fn fake_agent(root: &Path, helper: FakeHelper) -> Arc<Agent<FakeHelper, FakePostgres, FakeCaddy>> {
    let rt: FakeRt = NativeRuntime::new(
        helper,
        FakePostgres::new(),
        FakeCaddy::new(),
        fake_config(root),
    );
    Agent::new(rt)
}

#[tokio::test]
async fn expired_command_is_rejected() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    let agent = fake_agent(dir.path(), helper.clone());
    let mut env = envelope(
        "c-exp",
        Command::Start {
            site: "abcd1234".into(),
        },
    );
    env.expires_at = chrono::Utc::now() - chrono::Duration::seconds(1);
    let r = setu_agent::handle(&env, &agent).await;
    assert!(!r.ok);
    assert_eq!(r.error.as_deref(), Some("expired"));
    assert!(helper.calls().is_empty(), "an expired command never runs");
    assert_eq!(agent.runtime().state().cached("c-exp"), Some(r));
}

#[tokio::test]
async fn duplicate_command_id_returns_cached_result_without_reexecuting() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    let env = envelope(
        "c-dup",
        Command::Start {
            site: "abcd1234".into(),
        },
    );
    let first = {
        let agent = fake_agent(dir.path(), helper.clone());
        let first = setu_agent::handle(&env, &agent).await;
        assert!(first.ok, "{:?}", first.error);
        let again = setu_agent::handle(&env, &agent).await;
        assert_eq!(again, first);
        first
    };
    // A restarted agent (fresh process, same state.json) gets the
    // command redelivered: cached, not re-run.
    let agent = fake_agent(dir.path(), helper.clone());
    let redelivered = setu_agent::handle(&env, &agent).await;
    assert_eq!(redelivered, first);
    assert_eq!(helper.calls_for(Verb::Unit).len(), 1, "helper ran once");
}

#[tokio::test]
async fn secrets_are_redacted_in_log_and_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    helper.push_error(Verb::WriteEnv, "cannot write value hunter2-secret");
    let agent = fake_agent(dir.path(), helper.clone());
    let cmd = Command::WriteEnv {
        site: "abcd1234".into(),
        env: vec![EnvVar {
            name: "APP_SECRET".into(),
            value: "hunter2-secret".into(),
            secret: true,
            once: false,
        }],
    };
    let r = setu_agent::handle(&envelope("c-env", cmd), &agent).await;
    assert!(!r.ok);
    let error = r.error.clone().unwrap_or_default();
    assert!(
        !error.contains("hunter2-secret") && error.contains("***"),
        "{error}"
    );
    let saved = std::fs::read_to_string(dir.path().join("var/lib/setu/state.json")).expect("state");
    assert!(
        !saved.contains("hunter2-secret"),
        "state.json holds no secret"
    );
}

#[tokio::test]
async fn commands_for_one_site_run_in_order_while_other_sites_and_readonly_run_concurrently() {
    let dir = tempfile::tempdir().expect("tempdir");
    let servers = start_servers().await;
    let helper = FakeHelper::new();
    helper.set_delay(Verb::Install, Duration::from_millis(500));
    let agent = fake_agent(dir.path(), helper.clone());
    let binary = BinaryRef {
        version: "0.1.0".to_string(),
        url: servers.tarball_url.clone(),
        sha256: servers.tarball_sha.clone(),
        exec: "app".to_string(),
    };
    let install = |site: &str| Command::Install {
        site: site.into(),
        binary: binary.clone(),
    };
    let start = Instant::now();
    let timed = |env: CommandEnvelope| {
        let task = agent.spawn(env);
        tokio::spawn(async move {
            let r = task.await.expect("join");
            (r, start.elapsed())
        })
    };
    // Delivery order: A install, A switch, B install, A status.
    let a1 = timed(envelope("a1", install("aaaaaaaa")));
    let a2 = timed(envelope(
        "a2",
        Command::SwitchVersion {
            site: "aaaaaaaa".into(),
            version: "0.1.0".into(),
        },
    ));
    let b1 = timed(envelope("b1", install("bbbbbbbb")));
    let st = timed(envelope(
        "st",
        Command::Status {
            site: "aaaaaaaa".into(),
        },
    ));
    let (a1, a2, b1, st) = (
        a1.await.expect("a1"),
        a2.await.expect("a2"),
        b1.await.expect("b1"),
        st.await.expect("st"),
    );
    for (r, _) in [&a1, &a2, &b1, &st] {
        assert!(r.ok, "{}: {:?}", r.id, r.error);
    }
    let total = start.elapsed();
    assert!(
        total < Duration::from_millis(800),
        "sites ran serially: {total:?}"
    );
    assert!(
        st.1 < Duration::from_millis(400),
        "status waited: {:?}",
        st.1
    );
    assert!(a2.1 >= a1.1, "a2 finished before a1");
    let verbs: Vec<(Verb, String)> = helper
        .calls()
        .into_iter()
        .filter(|c| c.short == "aaaaaaaa" && c.verb != Verb::Unit)
        .map(|c| (c.verb, c.short))
        .collect();
    assert_eq!(
        verbs,
        [
            (Verb::Install, "aaaaaaaa".to_string()),
            (Verb::SwitchVersion, "aaaaaaaa".to_string())
        ]
    );
    assert_eq!(agent.slots().busy(), 0, "slots released");
}

#[tokio::test]
async fn health_polls_until_2xx_or_grace() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent = fake_agent(dir.path(), FakeHelper::new());
    // 503 twice, then 200.
    let hits = Arc::new(AtomicUsize::new(0));
    let h = hits.clone();
    let app = axum::Router::new().route(
        "/healthz",
        axum::routing::get(move || {
            let n = h.fetch_add(1, Ordering::SeqCst);
            async move {
                if n < 2 {
                    axum::http::StatusCode::SERVICE_UNAVAILABLE
                } else {
                    axum::http::StatusCode::OK
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let mut site = test_site("01jhealth0000000000000001", port);
    site.short_id = "abcd1234".into();
    let r = setu_agent::handle(
        &envelope("h1", Command::Health { site: site.clone() }),
        &agent,
    )
    .await;
    assert!(r.ok, "{:?}", r.error);
    assert_eq!(r.output["http"], 200);
    assert_eq!(hits.load(Ordering::SeqCst), 3, "polled until 2xx");

    // Never healthy: fails once the grace has passed.
    let app = axum::Router::new().route(
        "/healthz",
        axum::routing::get(|| async { axum::http::StatusCode::SERVICE_UNAVAILABLE }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    site.port = listener.local_addr().expect("addr").port();
    site.startup_grace_s = 1;
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    let t = Instant::now();
    let r = setu_agent::handle(&envelope("h2", Command::Health { site }), &agent).await;
    assert!(!r.ok);
    assert!(r.error.unwrap_or_default().contains("503"));
    let took = t.elapsed();
    assert!(
        took >= Duration::from_secs(1) && took < Duration::from_secs(4),
        "{took:?}"
    );
}

#[tokio::test]
async fn fetch_logs_returns_cursor() {
    let dir = tempfile::tempdir().expect("tempdir");
    let bin = tempfile::tempdir().expect("bindir");
    let args_file = dir.path().join("args");
    let script = bin.path().join("journalctl");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n\
             echo '{{\"__CURSOR\":\"c1\",\"MESSAGE\":\"one\"}}'\n\
             echo '{{\"__CURSOR\":\"c2\",\"MESSAGE\":\"two\"}}'\n",
            args_file.display()
        ),
    )
    .expect("script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    let rt: FakeRt = NativeRuntime::new(
        FakeHelper::new(),
        FakePostgres::new(),
        FakeCaddy::new(),
        fake_config(dir.path()),
    )
    .with_journalctl(script);
    let agent = Agent::new(rt);
    let cmd = Command::FetchLogs {
        site: "abcd1234".into(),
        cursor: Some("c0".into()),
        lines: 50,
    };
    let r = setu_agent::handle(&envelope("l1", cmd), &agent).await;
    assert!(r.ok, "{:?}", r.error);
    let out: LogsOutput = serde_json::from_value(r.output).expect("logs output");
    assert_eq!(out.lines, ["one", "two"]);
    assert_eq!(out.cursor.as_deref(), Some("c2"));
    let args = std::fs::read_to_string(&args_file).expect("args");
    let args: Vec<&str> = args.lines().collect();
    assert!(args.contains(&"setu-site@abcd1234.service"), "{args:?}");
    assert!(args.contains(&"--after-cursor=c0"), "{args:?}");
    assert!(args.contains(&"50"), "{args:?}");
}

#[tokio::test]
async fn heartbeat_includes_metrics() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    helper.push_json(
        Verb::Unit,
        serde_json::json!({
            "ActiveState": "active", "SubState": "running", "MainPID": "42",
            "ControlGroup": "/nonexistent.slice/setu-site@abcd1234.service",
            "MemoryCurrent": "1048576", "CPUUsageNSec": "2500000000", "NRestarts": "1",
            "data_bytes": 4096
        }),
    );
    let agent = fake_agent(dir.path(), helper.clone());
    agent
        .runtime()
        .update_state("abcd1234", |r| r.port = 20001)
        .expect("state");
    let hb = setu_agent::heartbeat::collect_heartbeat(agent.runtime()).await;
    assert!(!hb.boot_id.is_empty());
    assert_eq!(hb.units.len(), 1);
    assert!(hb.units[0].active && hb.units[0].pid == Some(42));
    assert_eq!(hb.metrics.len(), 1);
    let m = &hb.metrics[0];
    assert_eq!(m.short_id, "abcd1234");
    assert_eq!(m.memory_bytes, 1_048_576);
    assert!((m.cpu_seconds - 2.5).abs() < 1e-9);
    assert_eq!(m.disk_bytes, 4096, "data_bytes from the helper, not du");
    assert_eq!(
        helper.calls_for(Verb::Unit)[0].input,
        serde_json::json!({"op": "show"})
    );
}

// ---------------------------------------------------------------------
// The loop against a fake control plane.

#[derive(Clone, Default)]
struct FakeCp {
    queue: Arc<Mutex<VecDeque<CommandEnvelope>>>,
    /// (id, was the same result already in state.json when it arrived)
    posted: Arc<Mutex<Vec<(String, bool)>>>,
    state_path: Arc<Mutex<PathBuf>>,
    fail_posts: Arc<AtomicUsize>,
}

async fn fake_cp(cp: FakeCp) -> String {
    use axum::extract::{Path as AxPath, State as AxState};
    use axum::http::StatusCode;
    async fn poll(AxState(cp): AxState<FakeCp>) -> axum::response::Response {
        use axum::response::IntoResponse;
        let next = cp.queue.lock().expect("q").pop_front();
        match next {
            Some(env) => axum::Json(env).into_response(),
            None => {
                tokio::time::sleep(Duration::from_millis(50)).await;
                StatusCode::NO_CONTENT.into_response()
            }
        }
    }
    async fn result(
        AxState(cp): AxState<FakeCp>,
        AxPath(id): AxPath<String>,
        axum::Json(r): axum::Json<CommandResult>,
    ) -> StatusCode {
        assert_eq!(r.id, id);
        if cp.fail_posts.load(Ordering::SeqCst) > 0 {
            cp.fail_posts.fetch_sub(1, Ordering::SeqCst);
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        let path = cp.state_path.lock().expect("p").clone();
        let state = setu_agent::State::load(&path).expect("state");
        let persisted = state.recent.iter().any(|e| e.id == id && e.result == r);
        cp.posted.lock().expect("posted").push((id, persisted));
        StatusCode::NO_CONTENT
    }
    let app = axum::Router::new()
        .route("/agent/v1/commands", axum::routing::get(poll))
        .route(
            "/agent/v1/commands/{id}/result",
            axum::routing::post(result),
        )
        .with_state(cp);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    tokio::spawn(async move { axum::serve(listener, app).await.expect("serve") });
    format!("http://{addr}")
}

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(10), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn result_is_persisted_before_post() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = fake_config(dir.path());
    // A result a previous process persisted but never posted.
    let mut old = setu_agent::State::default();
    old.push_result(
        CommandResult {
            id: "old-1".into(),
            ok: true,
            output: serde_json::json!({}),
            log: "start ok".into(),
            error: None,
        },
        false,
    );
    old.save(&config.state_path).expect("seed");

    let cp = FakeCp::default();
    *cp.state_path.lock().expect("p") = config.state_path.clone();
    // The first post (the start-up re-post of old-1) fails: it must be
    // re-posted after the failure.
    cp.fail_posts.store(1, Ordering::SeqCst);
    let base = fake_cp(cp.clone()).await;
    let helper = FakeHelper::new();
    let agent = fake_agent(dir.path(), helper.clone());
    let client = ControlPlane::new(&AgentConfig {
        control_plane: base,
        server_token: "tok".into(),
        poll_wait_secs: 1,
        ..config.clone()
    })
    .expect("client");
    let looped = tokio::spawn(setu_agent::run_loop::run(
        agent.clone(),
        Arc::new(client),
        Backoff {
            min: Duration::from_millis(10),
            max: Duration::from_millis(50),
        },
    ));
    wait_for("old result re-posted", || {
        cp.posted
            .lock()
            .expect("p")
            .iter()
            .any(|(id, _)| id == "old-1")
    })
    .await;
    cp.queue.lock().expect("q").push_back(envelope(
        "new-1",
        Command::Start {
            site: "abcd1234".into(),
        },
    ));
    wait_for("new result posted after a failed post", || {
        cp.posted
            .lock()
            .expect("p")
            .iter()
            .any(|(id, _)| id == "new-1")
    })
    .await;
    wait_for("both marked posted", || {
        agent.runtime().state().unposted().is_empty()
    })
    .await;
    looped.abort();
    let posted = cp.posted.lock().expect("p").clone();
    assert!(
        posted.iter().all(|(_, persisted)| *persisted),
        "every result was in state.json before its post: {posted:?}"
    );
    assert_eq!(helper.calls_for(Verb::Unit).len(), 1, "new-1 ran once");
    let saved = setu_agent::State::load(&config.state_path).expect("state");
    assert!(saved.recent.iter().all(|r| r.posted));
}
