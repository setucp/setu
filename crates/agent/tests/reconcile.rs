//! `reconcile::run`: Caddy rebuilt from `state.json` and `agent.toml`.

use setu_agent::caddy::{CaddyAdmin, PanelRoute, ROUTES_PATH};
use setu_agent::state::SiteRecord;
use setu_agent::{AgentConfig, FakeCaddy, FakeHelper, FakePostgres, Paths, PostgresAdmin, State};

fn record(domain: Option<&str>, port: u16) -> SiteRecord {
    SiteRecord {
        domain: domain.map(str::to_string),
        port,
        version: "1.0.0".into(),
        db: None,
    }
}

fn route<'a>(routes: &'a [serde_json::Value], id: &str) -> Option<&'a serde_json::Value> {
    routes.iter().find(|r| r["@id"] == id)
}

#[tokio::test]
async fn reconcile_restores_missing_routes_including_panel_and_removes_stale() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = Paths::new(Some(dir.path().to_path_buf()));
    let config = AgentConfig {
        sites_root: p.sites(),
        state_path: p.state(),
        panel: Some(PanelRoute {
            host: "panel.example.test".into(),
            upstream: "unix//run/setu/http.sock".into(),
        }),
        ..AgentConfig::default()
    };
    let mut state = State::default();
    state
        .sites
        .insert("aaaaaaaa".into(), record(Some("a.example.test"), 20001));
    state
        .sites
        .insert("bbbbbbbb".into(), record(Some("b.example.test"), 20002));
    // Known site without a route (never routed, or RemoveRoute ran).
    state.sites.insert("cccccccc".into(), record(None, 20003));

    let caddy = FakeCaddy::new();
    // `a` survived with a stale port, `b` was lost with the autosave,
    // `cccccccc` and `zzzzzzzz` are stale, `foreign` is not ours.
    caddy
        .upsert_route("setu-site-aaaaaaaa", "a.example.test", "127.0.0.1:29999")
        .await
        .expect("seed a");
    caddy
        .upsert_route("setu-site-cccccccc", "c.example.test", "127.0.0.1:20003")
        .await
        .expect("seed c");
    caddy
        .upsert_route("setu-site-zzzzzzzz", "z.example.test", "127.0.0.1:20009")
        .await
        .expect("seed z");
    let foreign = serde_json::json!({"@id": "foreign", "match": [{"host": ["f.test"]}]});
    let (status, _) = caddy.admin().handle(
        "POST",
        ROUTES_PATH,
        &serde_json::to_vec(&foreign).expect("json"),
    );
    assert_eq!(status, 200);

    // Unknown leftovers are reported, never deleted.
    let unknown_dir = p.sites().join("dddddddd");
    std::fs::create_dir_all(&unknown_dir).expect("mkdir");
    let postgres = FakePostgres::new();
    postgres
        .create_or_alter("site_dddddddd", "site_dddddddd", "pw")
        .await
        .expect("db");

    let helper = FakeHelper::new();
    for _ in 0..2 {
        let report = setu_agent::reconcile::run(&state, &config, &helper, &caddy, &postgres)
            .await
            .expect("reconcile");
        let routes = caddy.admin().routes();
        let ids: Vec<&str> = routes.iter().filter_map(|r| r["@id"].as_str()).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), ids.len(), "no duplicate @id: {ids:?}");
        assert_eq!(
            route(&routes, "setu-site-aaaaaaaa").expect("a")["handle"][0]["upstreams"][0]["dial"],
            "127.0.0.1:20001",
            "port repaired"
        );
        assert_eq!(
            route(&routes, "setu-site-bbbbbbbb").expect("b restored")["match"][0]["host"][0],
            "b.example.test"
        );
        let panel = route(&routes, "setu-panel").expect("panel restored");
        assert_eq!(
            panel["handle"][0]["upstreams"][0]["dial"],
            "unix//run/setu/http.sock"
        );
        assert!(
            route(&routes, "setu-site-cccccccc").is_none(),
            "no domain: removed"
        );
        assert!(
            route(&routes, "setu-site-zzzzzzzz").is_none(),
            "unknown: removed"
        );
        assert!(
            route(&routes, "foreign").is_some(),
            "foreign routes untouched"
        );

        assert!(report.sites_on_disk.contains(&"dddddddd".to_string()));
        assert!(report.databases.contains(&"site_dddddddd".to_string()));
        assert_eq!(report.units.len(), 3, "one unit state per known site");
        assert!(unknown_dir.is_dir(), "unknown tree left in place");
        assert!(postgres.has_db("site_dddddddd"), "unknown db left in place");
    }
    // The helper was only asked for unit states.
    assert!(helper
        .calls()
        .iter()
        .all(|c| c.verb == setu_agent::helper::Verb::Unit && c.input["op"] == "show"));
}
