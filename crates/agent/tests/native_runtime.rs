//! `NativeRuntime` over the helper trait: `FakeHelper` for the exact
//! verbs and inputs, `LocalHelper` (the real helper code on a tempdir
//! root with fakes) for end-to-end effects. No root, systemd, Postgres
//! or Caddy.

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::json;
use setu_agent::helper::{Ctx, Verb};
use setu_agent::{
    AgentConfig, FakeCaddy, FakeHelper, FakePostgres, LocalHelper, NativeRuntime, Paths, Runtime,
    State,
};
use setu_proto::{BinaryRef, Limits, SiteSpec, Workdir};

const ID: &str = "01jtestsite00000000000001";
const SHORT: &str = "00000001";

/// A spec carries no paths: everything derives from the short id.
fn test_site(_root: &Path, port: u16) -> SiteSpec {
    SiteSpec {
        site_id: ID.to_string(),
        short_id: SHORT.to_string(),
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

fn binary(url: String, sha256: String, version: &str) -> BinaryRef {
    BinaryRef {
        version: version.to_string(),
        url,
        sha256,
        exec: "app".to_string(),
    }
}

fn config(root: &Path) -> AgentConfig {
    let p = Paths::new(Some(root.to_path_buf()));
    AgentConfig {
        state_path: p.state(),
        sites_root: p.sites(),
        downloads_dir: p.downloads(),
        backups_root: p.backups(),
        ..AgentConfig::default()
    }
}

fn fake_rt(root: &Path, helper: FakeHelper) -> NativeRuntime<FakeHelper, FakePostgres, FakeCaddy> {
    NativeRuntime::new(helper, FakePostgres::new(), FakeCaddy::new(), config(root))
}

fn local_rt(root: &Path) -> NativeRuntime<LocalHelper, FakePostgres, FakeCaddy> {
    NativeRuntime::new(
        LocalHelper::new(Ctx::fake(root)),
        FakePostgres::new(),
        FakeCaddy::new(),
        config(root),
    )
}

/// A tiny tar.gz with one executable file `name`.
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

async fn serve_tarball(tarball: Vec<u8>) -> String {
    let app = axum::Router::new().route(
        "/app.tar.gz",
        axum::routing::get(move || {
            let body = tarball.clone();
            async move { axum::body::Body::from(body) }
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
async fn install_downloads_then_calls_helper_install_with_version_and_sha() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    let rt = fake_rt(dir.path(), helper.clone());
    let tarball = make_tarball("app", b"#!/bin/sh\necho hi\n");
    let sha = sha256_hex(&tarball);
    let site = test_site(dir.path(), 20001);
    let mut bin = binary(
        serve_tarball(tarball.clone()).await,
        sha.to_uppercase(),
        "0.1.0",
    );

    rt.install(&site, &bin).await.expect("install");

    let dl = dir
        .path()
        .join("var/lib/setu/downloads")
        .join(format!("{sha}.tar.gz"));
    assert_eq!(std::fs::read(&dl).expect("download"), tarball);
    // Install never touches `current` (spec §1): no site-create, no
    // switch-version; those are their own commands.
    assert_eq!(helper.verbs(), [Verb::Install]);
    let call = &helper.calls_for(Verb::Install)[0];
    assert_eq!(call.short, SHORT);
    assert_eq!(call.input, json!({"version": "0.1.0", "sha256": sha}));

    // Wrong sha: the download fails, the helper is never asked to install.
    let helper = FakeHelper::new();
    let rt = fake_rt(dir.path(), helper.clone());
    bin.sha256 = "0".repeat(64);
    let err = rt.install(&site, &bin).await.expect_err("bad sha");
    assert!(err.to_string().contains("checksum"), "{err}");
    assert!(!helper.verbs().contains(&Verb::Install));
}

#[tokio::test]
async fn write_unit_sends_typed_input_without_paths() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    let rt = fake_rt(dir.path(), helper.clone());
    let mut site = test_site(dir.path(), 20001);
    site.workdir = Workdir::Data;
    site.exec = "dist/app".to_string();
    rt.write_unit(&site).await.expect("write_unit");
    let call = &helper.calls_for(Verb::WriteUnit)[0];
    assert_eq!(call.short, SHORT);
    // Version, exec, args, workdir and limits all come from the spec.
    assert_eq!(
        call.input,
        json!({
            "short_id": SHORT, "version": "0.1.0", "exec": "dist/app", "args": ["serve"],
            "workdir": "data", "memory_mb": 512, "cpu_percent": 100
        })
    );
    assert!(!call
        .input
        .to_string()
        .contains(dir.path().to_str().expect("utf8")));
}

#[tokio::test]
async fn site_create_sends_the_spec_dirs_and_records_the_port() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    let rt = fake_rt(dir.path(), helper.clone());
    rt.site_create(&test_site(dir.path(), 20009))
        .await
        .expect("site_create");
    let call = &helper.calls_for(Verb::SiteCreate)[0];
    assert_eq!(call.short, SHORT);
    assert_eq!(call.input, json!({"dirs": ["media"]}));
    let state = State::load(&dir.path().join("var/lib/setu/state.json")).expect("state");
    assert_eq!(state.sites[SHORT].port, 20009);
}

#[tokio::test]
async fn status_maps_show_output_including_data_bytes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let helper = FakeHelper::new();
    helper.push_json(
        Verb::Unit,
        json!({
            "ActiveState": "active", "SubState": "running", "MainPID": "4242",
            "ControlGroup": "/system.slice/system-setu\\x2dsite.slice/setu-site@00000001.service",
            "MemoryCurrent": "1048576", "CPUUsageNSec": "2000000000", "NRestarts": "3",
            "data_bytes": 12345
        }),
    );
    let rt = fake_rt(dir.path(), helper.clone());
    let st = rt.status(SHORT.to_string()).await.expect("status");
    assert!(st.active);
    assert_eq!(st.sub, "running");
    assert_eq!(st.pid, Some(4242));
    assert_eq!(st.memory_bytes, Some(1048576));
    assert_eq!(st.cpu_nsec, Some(2_000_000_000));
    assert_eq!(st.n_restarts, 3);
    assert_eq!(st.data_bytes, 12345);
    assert_eq!(
        setu_agent::metrics::cgroup_path(st.control_group.as_deref().expect("cg")).expect("path"),
        PathBuf::from(
            "/sys/fs/cgroup/system.slice/system-setu\\x2dsite.slice/setu-site@00000001.service"
        )
    );
    assert_eq!(helper.calls_for(Verb::Unit)[0].input, json!({"op": "show"}));
    // `[not set]` values map to None, not to an error.
    let st = rt.status(SHORT.to_string()).await.expect("default show");
    assert!(!st.active && st.memory_bytes.is_none() && st.pid.is_none());
}

#[tokio::test]
async fn install_beside_old_keeps_recorded_version_until_switch() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = local_rt(dir.path());
    let state_path = dir.path().join("var/lib/setu/state.json");
    let tarball = make_tarball("app", b"#!/bin/sh\necho hello\n");
    let url = serve_tarball(tarball.clone()).await;
    let sha = sha256_hex(&tarball);
    let sitedir = dir.path().join("srv/setu/sites").join(SHORT);
    let site = test_site(dir.path(), 20005);
    rt.site_create(&site).await.expect("site_create");
    assert!(sitedir.join("data/media").is_dir(), "needs.dirs created");

    rt.install(&site, &binary(url.clone(), sha.clone(), "0.1.0"))
        .await
        .expect("install 0.1.0");
    let exe = sitedir.join("bin/0.1.0/app");
    assert_eq!(
        std::fs::metadata(&exe).expect("meta").permissions().mode() & 0o777,
        0o755
    );
    assert!(
        std::fs::symlink_metadata(sitedir.join("bin/current")).is_err(),
        "install never points current"
    );
    assert!(rt
        .switch_version_inner(SHORT, "0.1.0")
        .await
        .expect("switch"));
    assert_eq!(
        std::fs::read_link(sitedir.join("bin/current")).expect("link"),
        PathBuf::from("0.1.0")
    );
    assert_eq!(
        State::load(&state_path).expect("state").sites[SHORT].version,
        "0.1.0"
    );

    rt.install(&site, &binary(url, sha, "0.1.1"))
        .await
        .expect("install 0.1.1");
    assert!(sitedir.join("bin/0.1.1/app").is_file());
    assert_eq!(
        std::fs::read_link(sitedir.join("bin/current")).expect("link"),
        PathBuf::from("0.1.0")
    );
    assert_eq!(
        State::load(&state_path).expect("state").sites[SHORT].version,
        "0.1.0"
    );

    assert!(rt
        .switch_version_inner(SHORT, "0.1.1")
        .await
        .expect("switch"));
    assert_eq!(
        std::fs::read_link(sitedir.join("bin/current")).expect("link"),
        PathBuf::from("0.1.1")
    );
    assert_eq!(
        State::load(&state_path).expect("state").sites[SHORT].version,
        "0.1.1"
    );
}

#[tokio::test]
async fn start_then_status_reports_active() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = local_rt(dir.path());
    assert!(!rt.status(SHORT.to_string()).await.expect("before").active);
    rt.start(SHORT.to_string()).await.expect("start");
    let after = rt.status(SHORT.to_string()).await.expect("after");
    assert!(after.active);
    assert_eq!(after.sub, "running");
    rt.stop(SHORT.to_string()).await.expect("stop");
    assert!(!rt.status(SHORT.to_string()).await.expect("stopped").active);
}

#[tokio::test]
async fn remove_runs_site_remove_and_forgets_state() {
    let dir = tempfile::tempdir().expect("tempdir");
    let rt = local_rt(dir.path());
    let site = test_site(dir.path(), 20006);
    rt.site_create(&site).await.expect("site_create");
    rt.write_unit(&site).await.expect("write_unit");
    rt.write_env_inner(SHORT, &[]).await.expect("env");
    assert!(dir
        .path()
        .join("srv/setu/sites")
        .join(SHORT)
        .join("env")
        .is_file());
    rt.remove(SHORT.to_string()).await.expect("remove");
    assert!(!dir.path().join("srv/setu/sites").join(SHORT).exists());
    assert!(!dir
        .path()
        .join("etc/systemd/system/setu-site@00000001.service.d")
        .exists());
    assert!(State::load(&dir.path().join("var/lib/setu/state.json"))
        .expect("state")
        .sites
        .is_empty());
}
