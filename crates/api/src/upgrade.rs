//! `setu upgrade [--version <ver>]` (0.1.5 §2): moves this server's
//! binary to a release with the installer's checks, then restarts what
//! runs it. On a control plane it first takes a self-backup (the same
//! dump and rotation as the nightly one) and afterwards waits for
//! `/healthz` on the configured listener.

use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use sha2::{Digest as _, Sha256};

/// Where releases are published (`install.sh`'s `REPO`).
const REPO: &str = "setucp/setu";
/// The installed binary (`install.sh`).
const INSTALL_PATH: &str = "/opt/setu/setu";
/// Longest wait for `/healthz` after the restart.
const HEALTH_WAIT: Duration = Duration::from_secs(90);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_mins(5);

/// What `setu upgrade` was asked for.
#[derive(Debug, Clone, Default)]
pub struct UpgradeArgs {
    pub version: Option<String>,
    pub allow_downgrade: bool,
}

/// The release target of this build (`install.sh`'s `select_archive`).
pub fn target() -> Result<&'static str, String> {
    match std::env::consts::ARCH {
        "x86_64" => Ok("x86_64-unknown-linux-musl"),
        "aarch64" => Ok("aarch64-unknown-linux-musl"),
        other => Err(format!("no release is built for {other}")),
    }
}

/// `setu-<ver>-<target>.tar.gz`.
pub fn archive_name(version: &str, target: &str) -> String {
    format!("setu-{version}-{target}.tar.gz")
}

/// The checksum `SHA256SUMS` lists for `file` (`<hex>  <file>`, or the
/// binary-mode `<hex> *<file>`).
pub fn sum_for(sums: &str, file: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let hex = parts.next()?;
        let name = parts.next()?;
        let ok = hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit());
        (ok && name.trim_start_matches('*') == file).then(|| hex.to_ascii_lowercase())
    })
}

/// What to do going from `current` to `wanted`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    AlreadyCurrent,
    Upgrade,
    Downgrade,
}

/// Compares two release versions (semver; a leading `v` is ignored).
pub fn plan(current: &str, wanted: &str) -> Result<Plan, String> {
    let parse = |v: &str| {
        semver::Version::parse(v.trim_start_matches('v'))
            .map_err(|e| format!("version {v:?} is not semver: {e}"))
    };
    let (cur, want) = (parse(current)?, parse(wanted)?);
    Ok(match want.cmp(&cur) {
        std::cmp::Ordering::Equal => Plan::AlreadyCurrent,
        std::cmp::Ordering::Greater => Plan::Upgrade,
        std::cmp::Ordering::Less => Plan::Downgrade,
    })
}

fn say(msg: &str) {
    println!("==> {msg}");
}

fn http() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(concat!("setu/", env!("CARGO_PKG_VERSION")))
        .timeout(DOWNLOAD_TIMEOUT)
        .build()
        .map_err(|e| format!("http client: {e}"))
}

async fn get_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>, String> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("download failed: {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: {url}: {}", resp.status()));
    }
    Ok(resp
        .bytes()
        .await
        .map_err(|e| format!("download failed: {url}: {e}"))?
        .to_vec())
}

async fn latest_version(client: &reqwest::Client) -> Result<String, String> {
    #[derive(serde::Deserialize)]
    struct Release {
        tag_name: String,
    }
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let body = get_bytes(client, &url).await?;
    let release: Release = serde_json::from_slice(&body)
        .map_err(|e| format!("cannot read the latest release: {e}"))?;
    Ok(release.tag_name.trim_start_matches('v').to_string())
}

/// Extracts the archive's top-level `setu` into `dir`.
fn extract_binary(archive: &[u8], dir: &Path) -> Result<PathBuf, String> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let entries = tar.entries().map_err(|e| format!("bad archive: {e}"))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| format!("bad archive: {e}"))?;
        let path = entry.path().map_err(|e| format!("bad archive: {e}"))?;
        if path.as_os_str() == "setu" || path.as_os_str() == "./setu" {
            let out = dir.join("setu");
            let mut bytes = Vec::new();
            entry
                .read_to_end(&mut bytes)
                .map_err(|e| format!("bad archive: {e}"))?;
            write_executable(&out, &bytes)?;
            return Ok(out);
        }
    }
    Err("the archive has no top-level setu binary".into())
}

fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::os::unix::fs::OpenOptionsExt as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o755)
        .open(path)
        .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    f.write_all(bytes)
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

fn systemctl(args: &[&str]) -> bool {
    Command::new("systemctl")
        .args(args)
        .status()
        .is_ok_and(|s| s.success())
}

fn unit_enabled(unit: &str) -> bool {
    systemctl(&["is-enabled", "--quiet", unit])
}

/// Takes a control-plane backup into the self-backup directory; the dump
/// is handed to the owner of that directory (`setu`), since root wrote it.
async fn backup_control_plane() -> Result<String, String> {
    let config = setu_common::Config::load().map_err(|e| e.to_string())?;
    let db = setu_db::Db::connect(&config.database_url)
        .await
        .map_err(|e| format!("cannot connect to database: {}", e.message))?;
    let rec = crate::self_backup::run_once(
        &db,
        Arc::new(crate::cli::TimedProcessRunner {
            timeout: crate::self_backup::DUMP_TIMEOUT,
        }),
        &config.database_url,
        &config.data_dir,
        chrono::Utc::now(),
    )
    .await
    .map_err(|e| e.message)?;
    if let Some(err) = rec.last_error {
        return Err(err);
    }
    let dir = crate::self_backup::backup_dir(&config.data_dir);
    let file = dir.join(rec.file.unwrap_or_default());
    if let Ok(meta) = std::fs::metadata(&dir) {
        use std::os::unix::fs::MetadataExt as _;
        let _ = std::os::unix::fs::chown(&file, Some(meta.uid()), Some(meta.gid()));
    }
    Ok(file.display().to_string())
}

/// `GET /healthz` on the configured listener answers 200.
async fn healthy(listen: &setu_common::Listen) -> bool {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
    const REQ: &[u8] = b"GET /healthz HTTP/1.0\r\nHost: localhost\r\n\r\n";
    async fn ask<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(mut s: S) -> bool {
        let mut head = [0u8; 12];
        s.write_all(REQ).await.is_ok()
            && s.read_exact(&mut head).await.is_ok()
            && head.ends_with(b" 200")
    }
    match listen {
        setu_common::Listen::Unix(path) => match tokio::net::UnixStream::connect(path).await {
            Ok(s) => ask(s).await,
            Err(_) => false,
        },
        setu_common::Listen::Tcp(addr) => match tokio::net::TcpStream::connect(addr).await {
            Ok(s) => ask(s).await,
            Err(_) => false,
        },
    }
}

/// Runs `setu upgrade`.
pub async fn run(args: UpgradeArgs) -> Result<(), String> {
    if !nix::unistd::Uid::effective().is_root() {
        return Err("setu upgrade must run as root".into());
    }
    let current = env!("CARGO_PKG_VERSION");
    let target = target()?;
    let client = http()?;
    let version = match args.version {
        Some(v) => v.trim_start_matches('v').to_string(),
        None => latest_version(&client).await?,
    };
    match plan(current, &version)? {
        Plan::AlreadyCurrent => {
            say(&format!("Already on setu {current}"));
            return Ok(());
        }
        Plan::Downgrade if !args.allow_downgrade => {
            return Err(format!(
                "{version} is older than the installed {current}; migrations are forward-only \
                 (pass --allow-downgrade only if the database never ran a newer version)"
            ));
        }
        Plan::Downgrade | Plan::Upgrade => {}
    }

    let base = format!("https://github.com/{REPO}/releases/download/v{version}");
    let archive = archive_name(&version, target);
    say(&format!("Downloading {archive}"));
    let bytes = get_bytes(&client, &format!("{base}/{archive}")).await?;
    let sums = get_bytes(&client, &format!("{base}/SHA256SUMS")).await?;
    let sums = String::from_utf8(sums).map_err(|_| "SHA256SUMS is not text".to_string())?;
    let expected =
        sum_for(&sums, &archive).ok_or(format!("SHA256SUMS has no entry for {archive}"))?;
    if hex::encode(Sha256::digest(&bytes)) != expected {
        return Err(format!("checksum mismatch for {archive}"));
    }
    say("Checksum verified");

    let tmp = tempfile::tempdir().map_err(|e| format!("temp dir: {e}"))?;
    let new_bin = extract_binary(&bytes, tmp.path())?;
    let reported = Command::new(&new_bin)
        .arg("--version")
        .output()
        .map_err(|e| format!("cannot run the new binary: {e}"))?;
    let reported = String::from_utf8_lossy(&reported.stdout).trim().to_string();
    if reported != format!("setu {version}") {
        return Err(format!(
            "the new binary reports {reported:?}, not setu {version}"
        ));
    }

    let control_plane = unit_enabled("setu.service");
    if control_plane {
        say("Backing up the control-plane database");
        let file = backup_control_plane()
            .await
            .map_err(|e| format!("backup failed, nothing changed: {e}"))?;
        say(&format!("Backup written: {file}"));
    }

    let staged = Path::new(INSTALL_PATH).with_file_name(".setu.new");
    std::fs::copy(&new_bin, &staged).map_err(|e| format!("cannot stage the binary: {e}"))?;
    std::fs::rename(&staged, INSTALL_PATH)
        .map_err(|e| format!("cannot install {INSTALL_PATH}: {e}"))?;
    say(&format!("Installed setu {version}"));

    if control_plane {
        say("Restarting setu.service");
        if !systemctl(&["restart", "setu.service"]) {
            return Err("systemctl restart setu.service failed (journalctl -u setu)".into());
        }
    }
    if unit_enabled("setu-agent.service") {
        say("Restarting setu-agent.service");
        if !systemctl(&["restart", "setu-agent.service"]) {
            return Err(
                "systemctl restart setu-agent.service failed (journalctl -u setu-agent)".into(),
            );
        }
    }
    if control_plane {
        let config = setu_common::Config::load().map_err(|e| e.to_string())?;
        let deadline = tokio::time::Instant::now() + HEALTH_WAIT;
        while !healthy(&config.listen).await {
            if tokio::time::Instant::now() > deadline {
                return Err(format!(
                    "setu {version} did not answer /healthz within {}s (journalctl -u setu)",
                    HEALTH_WAIT.as_secs()
                ));
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        say("Control plane healthy");
    }
    say(&format!("Upgraded setu {current} -> {version}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sums_match_text_and_binary_mode() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let sums = format!(
            "{a}  setu-0.1.5-x86_64-unknown-linux-musl.tar.gz\n{b} *install.sh\nbad line\n"
        );
        assert_eq!(
            sum_for(&sums, "setu-0.1.5-x86_64-unknown-linux-musl.tar.gz"),
            Some(a)
        );
        assert_eq!(sum_for(&sums, "install.sh"), Some("b".repeat(64)));
        assert_eq!(
            sum_for(&sums, "setu-0.1.5-aarch64-unknown-linux-musl.tar.gz"),
            None
        );
        assert_eq!(sum_for("xyz  install.sh", "install.sh"), None);
    }

    #[test]
    fn plans_compare_semver() {
        assert_eq!(plan("0.1.4", "0.1.5"), Ok(Plan::Upgrade));
        assert_eq!(plan("0.1.4", "v0.1.4"), Ok(Plan::AlreadyCurrent));
        assert_eq!(plan("0.1.10", "0.1.9"), Ok(Plan::Downgrade));
        assert_eq!(plan("0.1.4", "0.2.0-rc.1"), Ok(Plan::Upgrade));
        assert!(plan("0.1.4", "latest").is_err());
    }

    #[test]
    fn archive_names_follow_the_release() {
        assert_eq!(
            archive_name("0.1.5", "aarch64-unknown-linux-musl"),
            "setu-0.1.5-aarch64-unknown-linux-musl.tar.gz"
        );
    }

    #[test]
    fn extracts_only_the_top_level_binary() {
        use std::os::unix::fs::PermissionsExt as _;
        let mut gz = Vec::new();
        {
            let enc = flate2::write::GzEncoder::new(&mut gz, flate2::Compression::fast());
            let mut tar = tar::Builder::new(enc);
            for (name, body) in [("README.md", &b"hi"[..]), ("setu", &b"#!/bin/sh\n"[..])] {
                let mut h = tar::Header::new_gnu();
                h.set_size(body.len() as u64);
                h.set_mode(0o644);
                h.set_cksum();
                tar.append_data(&mut h, name, body).expect("append");
            }
            tar.into_inner().expect("tar").finish().expect("gz");
        }
        let dir = tempfile::tempdir().expect("tmp");
        let bin = extract_binary(&gz, dir.path()).expect("extracts");
        assert_eq!(std::fs::read(&bin).expect("read"), b"#!/bin/sh\n");
        assert_eq!(
            std::fs::metadata(&bin).expect("meta").permissions().mode() & 0o777,
            0o755
        );
        assert!(extract_binary(b"not gzip", dir.path()).is_err());
    }
}
