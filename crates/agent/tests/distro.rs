//! Distribution detection and package plans (0.1.2 spec §2): one table,
//! unit-tested from fixture `os-release` files.

use setu_agent::distro::{
    ensure_repos, plan_for, AptRepo, Distro, KeyFetch, PackagePlan, Support, PGDG_KEY_SHA256,
    PGDG_KEY_URL,
};
use setu_agent::install_system::RecordingCommands;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/os-release/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

fn distro(id: &str, version_id: &str) -> Distro {
    Distro {
        id: id.to_string(),
        version_id: version_id.to_string(),
        codename: None,
    }
}

fn supported(d: &Distro) -> PackagePlan {
    match plan_for(d) {
        Support::Supported(plan) => plan,
        other @ Support::Unsupported { .. } => panic!("{d:?}: expected a plan, got {other:?}"),
    }
}

/// Serves fixed bytes for any URL (the keyring download).
struct FixedFetch(Vec<u8>);

#[async_trait::async_trait]
impl KeyFetch for FixedFetch {
    async fn fetch(&self, _url: &str) -> anyhow::Result<Vec<u8>> {
        Ok(self.0.clone())
    }
}

#[test]
fn parses_each_fixture() {
    let cases = [
        ("ubuntu-26.04", "ubuntu", "26.04", Some("resolute")),
        ("ubuntu-24.04", "ubuntu", "24.04", Some("noble")),
        ("ubuntu-22.04", "ubuntu", "22.04", Some("jammy")),
        ("debian-12", "debian", "12", Some("bookworm")),
        ("debian-13", "debian", "13", Some("trixie")),
        ("fedora-40", "fedora", "40", None),
    ];
    for (file, id, version, codename) in cases {
        let d = Distro::parse(&fixture(file)).unwrap_or_else(|e| panic!("{file}: {e:#}"));
        assert_eq!(d.id, id, "{file}");
        assert_eq!(d.version_id, version, "{file}");
        assert_eq!(d.codename.as_deref(), codename, "{file}");
    }
}

#[test]
fn ubuntu_2404_uses_distro_postgres_16() {
    let plan = supported(&distro("ubuntu", "24.04"));
    assert!(plan.repos.is_empty(), "no PGDG on 24.04");
    assert_eq!(plan.postgres_major, 16);
    assert!(
        plan.packages.contains(&"postgresql-16"),
        "{:?}",
        plan.packages
    );
}

#[test]
fn ubuntu_2204_and_debian_12_add_pgdg() {
    for (id, version, codename) in [("ubuntu", "22.04", "jammy"), ("debian", "12", "bookworm")] {
        let plan = supported(&distro(id, version));
        assert_eq!(plan.repos.len(), 1, "{id} {version}");
        let repo = &plan.repos[0];
        assert_eq!(
            repo.list_line,
            format!(
                "deb [signed-by=/etc/apt/keyrings/apt.postgresql.org.asc] \
                 https://apt.postgresql.org/pub/repos/apt {codename}-pgdg main"
            )
        );
        assert_eq!(repo.keyring_url, PGDG_KEY_URL);
        assert_eq!(repo.keyring_sha256, PGDG_KEY_SHA256);
        assert_eq!(plan.postgres_major, 16, "{id} {version}");
        assert!(plan.packages.contains(&"postgresql-16"));
    }
}

#[test]
fn debian_13_uses_distro_postgres() {
    let plan = supported(&distro("debian", "13"));
    assert!(plan.repos.is_empty(), "trixie ships Postgres");
    assert_eq!(plan.postgres_major, 17);
    assert!(
        plan.packages.contains(&"postgresql-17"),
        "{:?}",
        plan.packages
    );
}

#[test]
fn fedora_is_unsupported() {
    match plan_for(&Distro::parse(&fixture("fedora-40")).expect("parses")) {
        Support::Unsupported { id, version } => {
            assert_eq!(id, "fedora");
            assert_eq!(version, "40");
        }
        other @ Support::Supported(_) => panic!("fedora must be unsupported: {other:?}"),
    }
}

#[test]
fn plan_installs_sudo_first() {
    for (id, version) in [
        ("ubuntu", "26.04"),
        ("ubuntu", "24.04"),
        ("ubuntu", "22.04"),
        ("debian", "12"),
        ("debian", "13"),
    ] {
        let plan = supported(&distro(id, version));
        assert_eq!(
            plan.packages.first().copied(),
            Some("sudo"),
            "{id} {version}"
        );
    }
}

#[tokio::test]
async fn pgdg_keyring_mismatch_aborts() {
    let plan = supported(&distro("debian", "12"));
    let sys = RecordingCommands::new();
    let fetch = FixedFetch(b"not the PGDG key".to_vec());
    let err = ensure_repos(&plan, &sys, &fetch)
        .await
        .expect_err("a wrong keyring must abort");
    assert!(err.to_string().contains("checksum"), "{err:#}");
    assert!(
        sys.calls().is_empty(),
        "nothing may touch apt or /etc before the check: {:?}",
        sys.calls()
    );
}

#[tokio::test]
async fn pgdg_keyring_match_writes_the_list_and_updates() {
    // sha256("abc"): a stand-in keyring whose digest is pinned in the plan.
    let plan = PackagePlan {
        repos: vec![AptRepo {
            list_line: "deb https://apt.example.invalid/repo bookworm main".to_string(),
            keyring_url: "https://apt.example.invalid/key.asc".to_string(),
            keyring_sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            keyring_path: "/etc/apt/keyrings/test.asc",
        }],
        packages: vec!["sudo"],
        postgres_major: 16,
    };
    let sys = RecordingCommands::new();
    ensure_repos(&plan, &sys, &FixedFetch(b"abc".to_vec()))
        .await
        .expect("matching keyring installs");
    let calls = sys.calls();
    let key = calls
        .iter()
        .position(|c| c.contains("/etc/apt/keyrings/test.asc"))
        .expect("keyring written");
    let list = calls
        .iter()
        .position(|c| c.contains("/etc/apt/sources.list.d/pgdg.list"))
        .expect("list written");
    let update = calls
        .iter()
        .position(|c| c == "apt-get update")
        .expect("update");
    assert!(key < list && list < update, "{calls:?}");
    assert_eq!(calls.len(), 3, "{calls:?}");
}
