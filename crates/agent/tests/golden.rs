//! Golden privileged artefacts (spec §4, plan Task 10b): the committed
//! assets must equal these texts byte for byte. A change here is a
//! security-review event, not a refactor.

const SUDOERS: &str = include_str!("../assets/sudoers");
const SITE_UNIT: &str = include_str!("../assets/setu-site@.service");
const AGENT_UNIT: &str = include_str!("../assets/setu-agent.service");
const DROPIN: &str = include_str!("../assets/dropin.conf.tmpl");

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
}
