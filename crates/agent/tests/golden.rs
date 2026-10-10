// Test crate: a panic is the failure report (R146).
#![allow(clippy::unwrap_used, clippy::expect_used)]
//! Golden privileged artefacts (spec §4, plan Task 10b): the committed
//! assets must equal these texts byte for byte. A change here is a
//! security-review event, not a refactor.

const SUDOERS: &str = include_str!("../assets/sudoers");
const SITE_UNIT: &str = include_str!("../assets/setu-site@.service");
const AGENT_UNIT: &str = include_str!("../assets/setu-agent.service");
const DROPIN: &str = include_str!("../assets/dropin.conf.tmpl");
const NFT_UNIT: &str = include_str!("../assets/setu-nftables.service");
const NFT_CONF: &str = include_str!("../assets/nftables.conf");
const CADDY_BASE: &str = include_str!("../assets/caddy-base.json");
const CADDY_DROPIN: &str = include_str!("../assets/caddy-api.dropin.conf");

#[test]
fn sudoers_is_exactly_the_helper_and_postgres_lines() {
    // No `Defaults:setu` line (ledger R70): `!requiretty` and `env_reset`
    // are sudo's defaults already, and sudo-rs rejects `requiretty`.
    assert_eq!(
        SUDOERS,
        "setu ALL=(root) NOPASSWD: /opt/setu/setu helper *\n\
         setu ALL=(postgres) NOPASSWD: /usr/bin/psql, /usr/bin/pg_dump, /usr/bin/pg_restore\n"
    );
}

#[test]
fn site_template_unit_is_golden() {
    assert_eq!(
        SITE_UNIT,
        "[Unit]
Description=Setu site %i
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=setu-site-%i
Group=setu-site-%i
EnvironmentFile=/srv/setu/sites/%i/env
Restart=on-failure
RestartSec=2
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
NoNewPrivileges=yes
ProtectKernelTunables=yes
ProtectControlGroups=yes
RestrictSUIDSGID=yes
ReadWritePaths=/srv/setu/sites/%i/data /srv/setu/sites/%i/run
UMask=0027

[Install]
WantedBy=multi-user.target
"
    );
}

#[test]
fn dropin_template_is_golden() {
    assert_eq!(
        DROPIN,
        "[Service]
ExecStart=
ExecStart=/srv/setu/sites/{short}/bin/current/{exec} {args}
WorkingDirectory={workdir}
MemoryMax={memory_mb}M
CPUQuota={cpu_percent}%
"
    );
}

#[test]
fn agent_unit_is_golden() {
    // No `ProtectSystem=`/`ReadWritePaths=` (ledger R69): sudo children
    // share this unit's mount namespace and the helper writes under /etc;
    // file modes already stop the `setu` user. No `NoNewPrivileges`: it
    // would make sudo refuse.
    assert_eq!(
        AGENT_UNIT,
        "[Unit]
Description=Setu server agent
After=network-online.target postgresql.service
Wants=network-online.target

[Service]
Type=simple
User=setu
Group=setu
ExecStart=/opt/setu/setu agent run
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
"
    );
    assert!(!AGENT_UNIT.contains("NoNewPrivileges"));
    assert!(!AGENT_UNIT.contains("ProtectSystem"));
    assert!(!AGENT_UNIT.contains("ReadWritePaths"));
}

#[test]
fn rendered_dropin_matches_golden_file() {
    let input = setu_agent::helper::UnitInput {
        short_id: "abcd1234".into(),
        version: "0.1.0".into(),
        exec: "dist/app".into(),
        args: ["serve", "--bind", "127.0.0.1:20001", "100%", "$HOME", "a b"]
            .map(String::from)
            .to_vec(),
        workdir: setu_agent::helper::Workdir::ExecDir,
        memory_mb: 512,
        cpu_percent: 50,
    };
    assert_eq!(
        setu_agent::helper::unit::render_dropin(&input).expect("render"),
        include_str!("golden/dropin-abcd1234.conf")
    );
}

#[test]
fn install_embeds_the_golden_assets() {
    assert_eq!(setu_agent::install::SUDOERS_ASSET, SUDOERS);
    assert_eq!(setu_agent::install::SITE_TEMPLATE_ASSET, SITE_UNIT);
    assert_eq!(setu_agent::install::AGENT_UNIT_ASSET, AGENT_UNIT);
    assert_eq!(setu_agent::install_system::NFT_UNIT_ASSET, NFT_UNIT);
    assert_eq!(setu_agent::install_system::NFT_CONF_ASSET, NFT_CONF);
    assert_eq!(setu_agent::install_system::CADDY_BASE_ASSET, CADDY_BASE);
    assert_eq!(setu_agent::install_system::CADDY_DROPIN_ASSET, CADDY_DROPIN);
}

#[test]
fn nftables_unit_is_golden() {
    // After the distro `nftables.service` (whose /etc/nftables.conf may
    // `flush ruleset`), before the network comes up.
    assert_eq!(
        NFT_UNIT,
        "[Unit]
Description=Setu firewall table (inet setu)
After=nftables.service
Before=network-pre.target
Wants=network-pre.target

[Service]
Type=oneshot
RemainAfterExit=yes
ExecStart=/usr/sbin/nft -f /etc/setu/nftables.conf

[Install]
WantedBy=multi-user.target
"
    );
}

#[test]
fn nftables_conf_is_golden_and_never_flushes_the_ruleset() {
    assert_eq!(
        NFT_CONF,
        "# Setu: only `table inet setu` is touched (never `flush ruleset`), so
# Docker's and the operator's rules stay intact. `add` then `delete`
# makes the load idempotent on a first boot and on every re-run.
add table inet setu
delete table inet setu

table inet setu {
\tchain input {
\t\ttype filter hook input priority 0; policy accept;
\t\tiifname != \"lo\" tcp dport 20000-29999 drop
\t}
}
"
    );
    let rules: Vec<&str> = NFT_CONF
        .lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .collect();
    assert!(!rules.iter().any(|l| l.contains("flush")), "{rules:?}");
}

#[test]
fn caddy_base_json_is_golden() {
    assert_eq!(
        CADDY_BASE,
        r#"{
  "admin": {
    "listen": "unix//run/caddy/admin.sock|0660",
    "config": {
      "persist": true
    }
  },
  "apps": {
    "http": {
      "servers": {
        "setu": {
          "listen": [":443"],
          "routes": []
        }
      }
    }
  }
}
"#
    );
}

#[test]
fn caddy_base_config_adds_internal_tls_and_panel_route_only_when_asked() {
    use setu_agent::caddy::PanelRoute;
    use setu_agent::install_system::caddy_base_config;
    let base: serde_json::Value = serde_json::from_str(CADDY_BASE).expect("json");
    assert_eq!(caddy_base_config(false, None).expect("base"), base);

    let mut internal = base.clone();
    internal["apps"]["tls"] = serde_json::json!({
        "automation": { "policies": [ { "issuers": [ { "module": "internal" } ] } ] }
    });
    assert_eq!(caddy_base_config(true, None).expect("internal"), internal);

    let panel = PanelRoute {
        host: "panel.example.test".into(),
        upstream: "unix//run/setu/http.sock".into(),
    };
    let with_panel = caddy_base_config(false, Some(&panel)).expect("panel");
    assert_eq!(
        with_panel["apps"]["http"]["servers"]["setu"]["routes"],
        serde_json::json!([{
            "@id": "setu-panel",
            "match": [{ "host": ["panel.example.test"] }],
            "handle": [{
                "handler": "reverse_proxy",
                "upstreams": [{ "dial": "unix//run/setu/http.sock" }]
            }],
            "terminal": true
        }])
    );
    assert!(with_panel["apps"].get("tls").is_none());
}

#[test]
fn caddy_api_dropin_is_golden() {
    assert_eq!(
        CADDY_DROPIN,
        "[Service]
ExecStart=
ExecStart=/usr/bin/caddy run --environ --config /etc/caddy/setu.json --resume
RuntimeDirectory=caddy
RuntimeDirectoryMode=0750
Group=caddy
"
    );
}
