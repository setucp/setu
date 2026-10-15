//! Distribution detection and package plans (spec 0.1.2 §2).
//!
//! `os-release` → [`Distro`] → [`Support`]. The same table is mirrored in
//! `install.sh` (the installer runs before the binary exists); its shell
//! test feeds it the same fixtures.

use anyhow::{bail, Context as _};
use sha2::{Digest as _, Sha256};

use crate::install_system::SystemCommands;

/// The PGDG signing key (fingerprint B97B 0AFC AA1A 47F0 44F2 44A0 7FCC
/// 7D46 ACCC 4CF8), as the PostgreSQL project publishes it.
pub const PGDG_KEY_URL: &str = "https://www.postgresql.org/media/keys/ACCC4CF8.asc";
/// SHA-256 of the armored key at [`PGDG_KEY_URL`], pinned 2026-10-10 by
/// downloading it once. A different digest aborts before apt sees a repo.
pub const PGDG_KEY_SHA256: &str =
    "0144068502a1eddd2a0280ede10ef607d1ec592ce819940991203941564e8e76";
/// Where the PGDG key is written (`signed-by` in the list file).
pub const PGDG_KEYRING_PATH: &str = "/etc/apt/keyrings/apt.postgresql.org.asc";
/// The apt list file for PGDG.
pub const PGDG_LIST_PATH: &str = "/etc/apt/sources.list.d/pgdg.list";
/// Largest keyring accepted (the PGDG key is about 5 KiB).
const MAX_KEYRING_BYTES: usize = 64 * 1024;

/// The fields of `/etc/os-release` Setu uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Distro {
    pub id: String,
    pub version_id: String,
    pub codename: Option<String>,
}

impl Distro {
    /// Parse `os-release` text (`KEY=value`, values optionally quoted).
    pub fn parse(os_release: &str) -> anyhow::Result<Distro> {
        let mut id = None;
        let mut version_id = None;
        let mut codename = None;
        let mut ubuntu_codename = None;
        for line in os_release.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value
                .trim()
                .trim_matches('"')
                .trim_matches('\'')
                .to_string();
            match key.trim() {
                "ID" => id = Some(value),
                "VERSION_ID" => version_id = Some(value),
                "VERSION_CODENAME" => codename = Some(value),
                "UBUNTU_CODENAME" => ubuntu_codename = Some(value),
                _ => {}
            }
        }
        let id = id
            .filter(|s| !s.is_empty())
            .context("os-release has no ID")?;
        Ok(Distro {
            id,
            version_id: version_id.unwrap_or_default(),
            codename: codename.or(ubuntu_codename).filter(|s| !s.is_empty()),
        })
    }

    /// Read `/etc/os-release` of this host.
    pub fn detect() -> anyhow::Result<Distro> {
        let text =
            std::fs::read_to_string("/etc/os-release").context("cannot read /etc/os-release")?;
        Distro::parse(&text)
    }
}

/// One apt source that is not the distribution's own.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AptRepo {
    /// The line written to [`PGDG_LIST_PATH`].
    pub list_line: String,
    pub keyring_url: String,
    pub keyring_sha256: &'static str,
    pub keyring_path: &'static str,
}

/// What `install` installs for a supported distribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackagePlan {
    pub repos: Vec<AptRepo>,
    /// Distribution packages, `sudo` first (Debian 12 may lack it).
    pub packages: Vec<&'static str>,
    pub postgres_major: u8,
}

/// Whether Setu installs on this distribution, and with which plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Support {
    Supported(PackagePlan),
    Unsupported { id: String, version: String },
}

/// The plan for a distribution (spec 0.1.2 §2 table).
pub fn plan_for(d: &Distro) -> Support {
    let pgdg = |codename: &str| {
        AptRepo {
        list_line: format!(
            "deb [signed-by={PGDG_KEYRING_PATH}] https://apt.postgresql.org/pub/repos/apt {codename}-pgdg main"
        ),
        keyring_url: PGDG_KEY_URL.to_string(),
        keyring_sha256: PGDG_KEY_SHA256,
        keyring_path: PGDG_KEYRING_PATH,
    }
    };
    let base = |major: u8| -> Vec<&'static str> {
        let pg = match major {
            18 => "postgresql-18",
            17 => "postgresql-17",
            _ => "postgresql-16",
        };
        vec![
            "sudo",
            "gnupg",
            "ca-certificates",
            "curl",
            pg,
            "nftables",
            "zstd",
        ]
    };
    let (repos, postgres_major) = match (d.id.as_str(), d.version_id.as_str()) {
        ("ubuntu", "26.04") => (vec![], 18),
        ("ubuntu", "24.04") => (vec![], 16),
        ("ubuntu", "22.04") => (vec![pgdg("jammy")], 16),
        ("debian", "12") => (vec![pgdg("bookworm")], 16),
        ("debian", "13") => (vec![], 17),
        _ => {
            return Support::Unsupported {
                id: d.id.clone(),
                version: d.version_id.clone(),
            }
        }
    };
    Support::Supported(PackagePlan {
        repos,
        packages: base(postgres_major),
        postgres_major,
    })
}

/// Fetches the keyring bytes (HTTPS only in production).
#[async_trait::async_trait]
pub trait KeyFetch: Send + Sync {
    async fn fetch(&self, url: &str) -> anyhow::Result<Vec<u8>>;
}

/// The real downloader: HTTPS GET with the default client.
#[derive(Debug, Clone, Copy, Default)]
pub struct HttpsFetch;

#[async_trait::async_trait]
impl KeyFetch for HttpsFetch {
    async fn fetch(&self, url: &str) -> anyhow::Result<Vec<u8>> {
        if !url.starts_with("https://") {
            bail!("keyring URL must be https: {url}");
        }
        let client = crate::config::http_client(None)?;
        let resp = client
            .get(url)
            .send()
            .await
            .with_context(|| format!("GET {url}"))?
            .error_for_status()
            .with_context(|| format!("GET {url}"))?;
        let bytes = resp.bytes().await.with_context(|| format!("GET {url}"))?;
        Ok(bytes.to_vec())
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Add the plan's apt sources: each keyring is downloaded, verified
/// against its pinned SHA-256 (a mismatch errors before any command
/// runs), then installed; the list file is written and `apt-get update`
/// runs once at the end.
pub async fn ensure_repos(
    plan: &PackagePlan,
    sys: &dyn SystemCommands,
    fetch: &dyn KeyFetch,
) -> anyhow::Result<()> {
    if plan.repos.is_empty() {
        return Ok(());
    }
    let mut verified = Vec::with_capacity(plan.repos.len());
    for repo in &plan.repos {
        let bytes = fetch.fetch(&repo.keyring_url).await?;
        if bytes.len() > MAX_KEYRING_BYTES {
            bail!("keyring {} is too large", repo.keyring_url);
        }
        let digest = sha256_hex(&bytes);
        if digest != repo.keyring_sha256 {
            bail!(
                "keyring checksum mismatch for {}: got {digest}, expected {}",
                repo.keyring_url,
                repo.keyring_sha256
            );
        }
        verified.push((repo, bytes));
    }
    let dir = tempfile::tempdir().context("temporary directory for apt files")?;
    for (repo, bytes) in &verified {
        let key = dir.path().join("key.asc");
        std::fs::write(&key, bytes).context("write keyring")?;
        install_file(sys, &key, repo.keyring_path).await?;
        let list = dir.path().join("list");
        std::fs::write(&list, format!("{}\n", repo.list_line)).context("write list")?;
        install_file(sys, &list, PGDG_LIST_PATH).await?;
    }
    sys.run("apt-get", &["update"]).await?;
    Ok(())
}

async fn install_file(
    sys: &dyn SystemCommands,
    from: &std::path::Path,
    to: &str,
) -> anyhow::Result<()> {
    let from = from.to_str().context("temporary path is not UTF-8")?;
    sys.run("install", &["-D", "-m", "0644", from, to]).await?;
    Ok(())
}
