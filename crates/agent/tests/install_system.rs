// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! `install_system` (spec §4 "Not a helper verb", "Network", "Caddy";
//! plan Task 10b): runs as real root from `setu agent install` and the
//! installer; here against a `--root` tempdir with recording fakes.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use setu_agent::caddy::{
    route_json, site_route_id, site_upstream, CaddyAdmin, FakeCaddy, PanelRoute, PANEL_ROUTE_ID,
};
use setu_agent::install_system::{
    caddy_base_config, install_system_with, InstallSystemOpts, RecordingCommands,
    CADDY_DROPIN_ASSET, NFT_CONF_ASSET, NFT_UNIT_ASSET,
};

fn panel(host: &str) -> PanelRoute {
    PanelRoute {
        host: host.to_string(),
        upstream: "unix//run/setu/http.sock".to_string(),
    }
}

fn read(root: &Path, rel: &str) -> String {
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn mode(root: &Path, rel: &str) -> u32 {
    std::fs::metadata(root.join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .permissions()
        .mode()
        & 0o7777
}

fn loads(caddy: &FakeCaddy) -> Vec<serde_json::Value> {
    caddy
        .admin()
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST" && r.path == "/load")
        .map(|r| r.body.expect("load body"))
        .collect()
}

#[tokio::test]
async fn install_system_writes_all_assets_and_enables_units() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // A distro `caddy.service` autosave (admin on TCP 2019) must not be
    // resumed by caddy-api: it is moved aside.
    let autosave = root.join("var/lib/caddy/.config/caddy/autosave.json");
    std::fs::create_dir_all(autosave.parent().expect("parent")).expect("mkdir");
    std::fs::write(&autosave, r#"{"admin":{"listen":"localhost:2019"}}"#).expect("autosave");

    let caddy = FakeCaddy::new();
    caddy.admin().set_up(false);
    let sys = RecordingCommands::new().starting_caddy(caddy.admin().clone());
    let opts = InstallSystemOpts {
        root: Some(root.to_path_buf()),
        tls_internal: true,
        panel: Some(panel("panel.example.test")),
        caddy_only: false,
    };
    install_system_with(&opts, &sys, &caddy)
        .await
        .expect("install_system");

    assert_eq!(
        read(root, "etc/systemd/system/setu-site@.service"),
        setu_agent::install::SITE_TEMPLATE_ASSET
    );
    assert_eq!(
        read(root, "etc/systemd/system/setu-agent.service"),
        setu_agent::install::AGENT_UNIT_ASSET
    );
    assert_eq!(
        read(root, "etc/sudoers.d/setu"),
        setu_agent::install::SUDOERS_ASSET
    );
    assert_eq!(mode(root, "etc/sudoers.d/setu"), 0o440);
    assert!(!root.join("etc/sudoers.d/.setu.tmp").exists());
    assert_eq!(read(root, "etc/setu/nftables.conf"), NFT_CONF_ASSET);
    assert_eq!(
        read(root, "etc/systemd/system/setu-nftables.service"),
        NFT_UNIT_ASSET
    );
    assert_eq!(
        read(root, "etc/systemd/system/caddy-api.service.d/10-setu.conf"),
        CADDY_DROPIN_ASSET
    );
    let want = caddy_base_config(true, Some(&panel("panel.example.test"))).expect("base");
    let written: serde_json::Value =
        serde_json::from_str(&read(root, "etc/caddy/setu.json")).expect("setu.json");
    assert_eq!(written, want);
    assert_eq!(mode(root, "etc/caddy/setu.json"), 0o644);
    assert!(!autosave.exists(), "foreign autosave must be moved aside");
    assert!(autosave.with_extension("json.setu-foreign").exists());

    let visudo = format!(
        "visudo -cf {}",
        root.join("etc/sudoers.d/.setu.tmp").display()
    );
    assert_eq!(
        sys.calls(),
        vec![
            visudo,
            "usermod -aG caddy,systemd-journal setu".to_string(),
            "systemctl daemon-reload".to_string(),
            "systemctl enable setu-nftables.service".to_string(),
            "systemctl restart setu-nftables.service".to_string(),
            "install -d -o caddy -g caddy -m 0750 /var/lib/caddy".to_string(),
            "systemctl disable --now caddy.service".to_string(),
            "systemctl enable caddy-api.service".to_string(),
            "systemctl restart caddy-api.service".to_string(),
        ]
    );
    // Once Caddy is up on the socket, the base is applied through it.
    assert_eq!(loads(&caddy), vec![want]);
}

#[tokio::test]
async fn install_system_caddy_only_touches_only_caddy_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let caddy = FakeCaddy::new();
    caddy.admin().set_up(false);
    let sys = RecordingCommands::new().starting_caddy(caddy.admin().clone());
    let opts = InstallSystemOpts {
        root: Some(root.to_path_buf()),
        tls_internal: false,
        panel: Some(panel("panel.example.test")),
        caddy_only: true,
    };
    install_system_with(&opts, &sys, &caddy)
        .await
        .expect("install_system --caddy-only");

    let mut files: Vec<String> = walk(root)
        .into_iter()
        .map(|p| p.strip_prefix(root).expect("prefix").display().to_string())
        .collect();
    files.sort();
    assert_eq!(
        files,
        vec![
            "etc/caddy/setu.json".to_string(),
            "etc/systemd/system/caddy-api.service.d/10-setu.conf".to_string(),
        ]
    );
    assert_eq!(
        sys.calls(),
        vec![
            "systemctl daemon-reload".to_string(),
            "install -d -o caddy -g caddy -m 0750 /var/lib/caddy".to_string(),
            "systemctl disable --now caddy.service".to_string(),
            "systemctl enable caddy-api.service".to_string(),
            "systemctl restart caddy-api.service".to_string(),
        ]
    );
    let want = caddy_base_config(false, Some(&panel("panel.example.test"))).expect("base");
    assert_eq!(loads(&caddy), vec![want]);
}

#[tokio::test]
async fn install_system_rerun_merges_existing_routes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    // Caddy is already up on the socket with a site route and an old
    // panel host.
    let caddy = FakeCaddy::new();
    let site_id = site_route_id("abcd1234").expect("id");
    caddy
        .upsert_route(&site_id, "a.example.test", &site_upstream(20001))
        .await
        .expect("seed site");
    caddy
        .upsert_route(
            PANEL_ROUTE_ID,
            "old.example.test",
            "unix//run/setu/http.sock",
        )
        .await
        .expect("seed panel");
    caddy.admin().clear_requests();

    let sys = RecordingCommands::new().starting_caddy(caddy.admin().clone());
    let opts = InstallSystemOpts {
        root: Some(root.to_path_buf()),
        tls_internal: true,
        panel: Some(panel("panel.example.test")),
        caddy_only: false,
    };
    install_system_with(&opts, &sys, &caddy)
        .await
        .expect("re-run");

    let calls = sys.calls();
    assert!(
        !calls.contains(&"systemctl restart caddy-api.service".to_string()),
        "a running Caddy is reloaded through the socket, not restarted: {calls:?}"
    );
    assert!(calls.contains(&"systemctl enable caddy-api.service".to_string()));
    assert!(
        calls.contains(&"install -d -o caddy -g caddy -m 0750 /var/lib/caddy".to_string()),
        "Caddy's home must be owned by caddy even when the directory pre-existed (a volume, a re-install): {calls:?}"
    );

    let loaded = loads(&caddy);
    assert_eq!(loaded.len(), 1, "{loaded:?}");
    let routes = loaded[0]["apps"]["http"]["servers"]["setu"]["routes"]
        .as_array()
        .expect("routes")
        .clone();
    assert_eq!(
        routes,
        vec![
            route_json(
                PANEL_ROUTE_ID,
                "panel.example.test",
                "unix//run/setu/http.sock"
            )
            .expect("panel"),
            route_json(&site_id, "a.example.test", "127.0.0.1:20001").expect("site"),
        ],
        "site route survives, the panel route takes the new host once"
    );
    assert_eq!(
        loaded[0]["apps"]["tls"]["automation"]["policies"][0]["issuers"][0]["module"],
        "internal"
    );
    // The running config now is that merge.
    assert_eq!(
        caddy.list_route_ids().await.expect("ids"),
        vec![PANEL_ROUTE_ID.to_string(), site_id]
    );
}

fn walk(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).expect("read_dir") {
        let path = entry.expect("entry").path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test]
async fn ip_panel_host_gets_a_default_sni() {
    let cfg = setu_agent::install_system::caddy_base_config(true, Some(&panel("192.0.2.10")))
        .expect("config");
    assert_eq!(
        cfg["apps"]["http"]["servers"]["setu"]["tls_connection_policies"][0]["default_sni"],
        serde_json::json!("192.0.2.10")
    );
    let cfg =
        setu_agent::install_system::caddy_base_config(true, Some(&panel("panel.example.test")))
            .expect("config");
    assert!(cfg["apps"]["http"]["servers"]["setu"]["tls_connection_policies"].is_null());
}
