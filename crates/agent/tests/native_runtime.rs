//! `NativeRuntime` behaviour against `FakeSystemd`/`FakePostgres`/`FakeCaddy`.
//!
//! No root, no systemd, no Postgres, no Caddy: everything is a trait fake
//! plus tempdirs and a local axum server for downloads.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use setu_agent::{AgentConfig, CaddyFake, FakePostgres, FakeSystemd, NativeRuntime, Runtime};
use setu_proto::{BinaryRef, Limits, SiteSpec};

fn test_site(sites_root: &Path, port: u16) -> SiteSpec {
    let id = "01jtestsite00000000000001";
    let dir = sites_root.join(id);
    SiteSpec {
        site_id: id.to_string(),
        short_id: "00000001".to_string(),
        // Production form: the short id (usernames cap at 32 chars).
        user: format!("setu-site-{}", &id[id.len() - 8..]),
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

/// Build a tiny tar.gz in memory containing one executable file `name`.
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

/// Serve `tarball` at `/app.tar.gz` on an ephemeral port; returns the URL.
async fn serve_tarball(tarball: Vec<u8>) -> String {
    let app = axum::Router::new().route(
        "/app.tar.gz",
        axum::routing::get(move || {
            let body = tarball.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/gzip")],
                    axum::body::Body::from(body),
                )
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr: SocketAddr = listener.local_addr().expect("addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve");
    });
    format!("http://{addr}/app.tar.gz")
}

#[tokio::test]
async fn write_unit_ensures_site_user_before_dropin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(
        systemd.clone(),
        &dir.path().join("sites"),
        &dir.path().join("state.json"),
    );
    let site = test_site(&dir.path().join("sites"), 20001);

    rt.write_unit(&site).await.expect("write_unit");

    // The site user must exist before the drop-in (which names it) and
    // the env file (which is chowned to it) land.
    let calls = systemd.calls();
    let user_at = calls
        .iter()
        .position(|c| c == "ensure-user setu-site-00000001")
        .expect("ensure-user must be recorded");
    let dropin_at = calls
        .iter()
        .position(|c| c.starts_with("write-dropin"))
        .expect("drop-in must be recorded");
    assert!(user_at < dropin_at, "user first: {calls:?}");
}

#[tokio::test]
async fn remove_inner_deletes_user_and_dropin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(
        systemd.clone(),
        &dir.path().join("sites"),
        &dir.path().join("state.json"),
    );
    let site = test_site(&dir.path().join("sites"), 20001);

    rt.write_unit(&site).await.expect("write_unit");
    rt.remove_inner(&site).await.expect("remove_inner");

    let calls = systemd.calls();
    assert!(
        calls
            .iter()
            .any(|c| c == "remove-dropin setu-site@01jtestsite00000000000001.service"),
        "drop-in removed: {calls:?}"
    );
    assert!(
        calls.iter().any(|c| c == "remove-user setu-site-00000001"),
        "site user removed: {calls:?}"
    );
}

#[tokio::test]
async fn write_unit_renders_limits_dropin() {
    let dir = tempfile::tempdir().expect("tempdir");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(
        systemd.clone(),
        &dir.path().join("sites"),
        &dir.path().join("state.json"),
    );
    let site = test_site(&dir.path().join("sites"), 20001);

    rt.write_unit(&site).await.expect("write_unit");

    let unit = "setu-site@01jtestsite00000000000001.service";
    let dropin = systemd
        .dropin_contents(unit)
        .expect("drop-in must be written");
    let expected = "\
# Setu-managed drop-in for site 01jtestsite00000000000001. Do not edit by hand.\n\
[Service]\n\
User=setu-site-00000001\n\
WorkingDirectory=/SITES/01jtestsite00000000000001\n\
EnvironmentFile=/SITES/01jtestsite00000000000001/env\n\
ExecStart=\n\
ExecStart=/SITES/01jtestsite00000000000001/bin/current/app serve\n\
MemoryMax=512M\n\
CPUQuota=100%\n";
    let expected = expected.replace("/SITES", dir.path().join("sites").to_str().expect("utf8"));
    assert_eq!(dropin, expected, "drop-in golden mismatch");
}

#[tokio::test]
async fn install_verifies_checksum_and_switches_current() {
    let dir = tempfile::tempdir().expect("tempdir");
    let sites_root = dir.path().join("sites");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(systemd, &sites_root, &dir.path().join("state.json"));

    let tarball = make_tarball("app", b"#!/bin/sh\necho hello\n");
    let url = serve_tarball(tarball.clone()).await;
    let good_sha = sha256_hex(&tarball);

    // Wrong sha: must fail before touching `current`.
    let mut bad = test_site(&sites_root, 20002);
    bad.site_id = "01jtestsite00000000000002".to_string();
    let baddir = sites_root.join(&bad.site_id);
    bad.data_dir = baddir.join("data");
    bad.run_dir = baddir.join("run");
    bad.exec = baddir.join("bin").join("current").join("app");
    bad.env_file = baddir.join("env");
    bad.binary = Some(BinaryRef {
        url: url.clone(),
        sha256: "0".repeat(64),
        exec: "app".to_string(),
        version: "0.1.0".to_string(),
    });
    let err = rt.install(&bad).await.expect_err("bad sha must fail");
    assert!(
        err.to_string().contains("checksum"),
        "error must name checksum, got: {err}"
    );
    assert!(
        !baddir.join("bin").join("current").exists(),
        "failed install must not create current"
    );

    // Right sha: installs, exec bit set, `current` points at 0.1.0.
    let mut good = test_site(&sites_root, 20003);
    good.site_id = "01jtestsite00000000000003".to_string();
    let gooddir = sites_root.join(&good.site_id);
    good.data_dir = gooddir.join("data");
    good.run_dir = gooddir.join("run");
    good.exec = gooddir.join("bin").join("current").join("app");
    good.env_file = gooddir.join("env");
    good.binary = Some(BinaryRef {
        url,
        sha256: good_sha,
        exec: "app".to_string(),
        version: "0.1.0".to_string(),
    });
    rt.install(&good).await.expect("install");
    let exe = gooddir.join("bin").join("0.1.0").join("app");
    assert!(exe.is_file(), "extracted exec must exist");
    let mode = std::fs::metadata(&exe).expect("meta").permissions().mode();
    assert_ne!(mode & 0o111, 0, "exec must be executable");
    let link = std::fs::read_link(gooddir.join("bin").join("current")).expect("readlink");
    assert_eq!(link, PathBuf::from("0.1.0"), "current must point at 0.1.0");
}

#[tokio::test]
async fn install_beside_old_keeps_recorded_version_until_switch() {
    use setu_agent::StateFile;

    let dir = tempfile::tempdir().expect("tempdir");
    let sites_root = dir.path().join("sites");
    let state_path = dir.path().join("state.json");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(systemd, &sites_root, &state_path);

    let tarball = make_tarball("app", b"#!/bin/sh\necho hello\n");
    let url = serve_tarball(tarball.clone()).await;
    let sha = sha256_hex(&tarball);

    let mut site = test_site(&sites_root, 20005);
    site.site_id = "01jversion0000000000000001".to_string();
    let sitedir = sites_root.join(&site.site_id);
    site.data_dir = sitedir.join("data");
    site.run_dir = sitedir.join("run");
    site.exec = sitedir.join("bin").join("current").join("app");
    site.env_file = sitedir.join("env");

    // Install 0.1.0: `current` points there and state records 0.1.0.
    site.binary = Some(BinaryRef {
        url: url.clone(),
        sha256: sha.clone(),
        exec: "app".to_string(),
        version: "0.1.0".to_string(),
    });
    rt.install(&site).await.expect("install 0.1.0");
    assert_eq!(
        StateFile::load(&state_path).expect("state").sites[&site.site_id].version,
        "0.1.0"
    );

    // Install 0.1.1 beside it: extracted, but `current` and the recorded
    // version stay on 0.1.0 until the explicit switch.
    site.binary = Some(BinaryRef {
        url,
        sha256: sha,
        exec: "app".to_string(),
        version: "0.1.1".to_string(),
    });
    rt.install(&site).await.expect("install 0.1.1");
    assert!(sitedir.join("bin").join("0.1.1").join("app").is_file());
    assert_eq!(
        std::fs::read_link(sitedir.join("bin").join("current")).expect("readlink"),
        PathBuf::from("0.1.0")
    );
    assert_eq!(
        StateFile::load(&state_path).expect("state").sites[&site.site_id].version,
        "0.1.0"
    );

    // Explicit switch moves both.
    rt.switch_version_inner(&site.site_id, "0.1.1")
        .await
        .expect("switch");
    assert_eq!(
        std::fs::read_link(sitedir.join("bin").join("current")).expect("readlink"),
        PathBuf::from("0.1.1")
    );
    assert_eq!(
        StateFile::load(&state_path).expect("state").sites[&site.site_id].version,
        "0.1.1"
    );
}

#[tokio::test]
async fn start_then_status_reports_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let systemd = FakeSystemd::new();
    let rt = test_runtime(
        systemd.clone(),
        &dir.path().join("sites"),
        &dir.path().join("state.json"),
    );
    let site = test_site(&dir.path().join("sites"), 20004);

    let before = rt
        .status(site.site_id.clone())
        .await
        .expect("status before start");
    assert!(!before.active, "unit must start inactive");

    rt.start(site.site_id.clone()).await.expect("start");
    let after = rt.status(site.site_id.clone()).await.expect("status");
    assert!(after.active, "unit must be active after start");
    assert_eq!(after.sub, "running");
    assert!(systemd
        .calls()
        .contains(&format!("start setu-site@{}.service", site.site_id)));
}
