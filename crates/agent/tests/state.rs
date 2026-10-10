//! `state.json` and `agent.toml`: round trips, the result cap, modes,
//! redacting `Debug`.

use std::os::unix::fs::PermissionsExt;

use setu_agent::state::{SiteRecord, StateStore, RECENT_CAP};
use setu_agent::{AgentConfig, State};
use setu_proto::CommandResult;

fn result(id: &str) -> CommandResult {
    CommandResult {
        id: id.to_string(),
        ok: true,
        output: serde_json::json!({"n": id}),
        log: "ok".into(),
        error: None,
    }
}

fn mode(path: &std::path::Path) -> u32 {
    std::fs::metadata(path).expect("meta").permissions().mode() & 0o777
}

#[test]
fn state_round_trips_and_caps_results() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("state.json");
    let store = StateStore::open(&path);
    store
        .update_site("abcd1234", |r| {
            *r = SiteRecord {
                domain: Some("a.example.test".into()),
                port: 20001,
                version: "1.2.3".into(),
                db: Some("site_abcd1234".into()),
            }
        })
        .expect("site");
    store
        .update(|s| {
            for i in 0..RECENT_CAP + 4 {
                s.push_result(result(&format!("c{i}")), false);
            }
        })
        .expect("batch");
    store
        .record_result(&result(&format!("c{}", RECENT_CAP + 4)), false)
        .expect("record");
    store.mark_posted("c10").expect("posted");
    assert_eq!(mode(&path), 0o600, "state.json is 0600");

    let back = State::load(&path).expect("load");
    assert_eq!(back, store.snapshot(), "round trip");
    assert_eq!(back.recent.len(), RECENT_CAP, "capped at {RECENT_CAP}");
    assert_eq!(
        back.recent.front().expect("front").id,
        "c5",
        "oldest dropped"
    );
    assert!(back.cached("c4").is_none());
    assert_eq!(back.cached("c1004"), Some(&result("c1004")));
    assert!(
        back.recent
            .iter()
            .find(|r| r.id == "c10")
            .expect("c10")
            .posted
    );
    assert_eq!(StateStore::open(&path).unposted().len(), RECENT_CAP - 1);
    assert_eq!(
        back.sites["abcd1234"].domain.as_deref(),
        Some("a.example.test")
    );

    // A re-recorded id replaces its entry instead of duplicating it.
    store.record_result(&result("c1004"), false).expect("again");
    assert_eq!(
        store.read(|s| s.recent.iter().filter(|r| r.id == "c1004").count()),
        1
    );
    // Read-only results leave once posted (a log page is large).
    store.record_result(&result("ro"), true).expect("ro");
    assert!(store.cached("ro").is_some(), "kept until posted");
    store.mark_posted("ro").expect("posted");
    assert!(store.cached("ro").is_none(), "dropped once posted");

    store.remove_site("abcd1234").expect("remove");
    assert!(State::load(&path).expect("load").sites.is_empty());
}

#[test]
fn agent_config_debug_redacts_token() {
    let cfg = AgentConfig {
        server_token: "setu_srv_SECRET-token".into(),
        ca: Some("/etc/setu/ca.pem".into()),
        ..AgentConfig::default()
    };
    let dbg = format!("{cfg:?}");
    assert!(!dbg.contains("SECRET"), "token leaked: {dbg}");
    assert!(dbg.contains("[redacted]") && dbg.contains("ca.pem"));
}

#[test]
fn agent_toml_is_created_0600() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("etc/setu/agent.toml");
    let cfg = AgentConfig {
        control_plane: "https://panel.example.test".into(),
        server_token: "tok".into(),
        reconcile_secs: 7,
        ca: Some("/etc/setu/ca.pem".into()),
        ..AgentConfig::default()
    };
    cfg.save(&path).expect("save");
    assert_eq!(mode(&path), 0o600);
    // Overwrite keeps 0600 and leaves no temp file behind.
    cfg.save(&path).expect("save again");
    assert_eq!(mode(&path), 0o600);
    let names: Vec<String> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("ls")
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["agent.toml"]);
    let back = AgentConfig::load(Some(&path)).expect("load");
    assert_eq!(back.control_plane, "https://panel.example.test");
    assert_eq!(back.reconcile_secs, 7);
    assert_eq!(
        back.ca.as_deref(),
        Some(std::path::Path::new("/etc/setu/ca.pem"))
    );
    // A pre-WS7 file names the URL `control_plane_url`.
    std::fs::write(&path, "control_plane_url = \"https://old.example\"\n").expect("write");
    assert_eq!(
        AgentConfig::load(Some(&path)).expect("old").control_plane,
        "https://old.example"
    );
}
