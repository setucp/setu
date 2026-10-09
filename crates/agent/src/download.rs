//! Binary download: stream to a temp file, verify sha256, extract the
//! tar.gz, chmod the exec bit, then atomically switch `bin/current`.
//!
//! Decision: the `tar` + `flate2` crates instead of shelling out to the
//! system `tar` (no external dependency, staging dir keeps a failed
//! extract from polluting the version dir).

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Errors from download/install/switch.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download {0} failed: {1}")]
    Fetch(String, String),
    #[error("checksum mismatch for {0}: expected {1}, got {2}")]
    Checksum(String, String, String),
    #[error("invalid sha256 {0}: must be 64 hex chars")]
    BadSha(String),
    #[error("extract {0} failed: {1}")]
    Extract(String, String),
    #[error("install I/O for {0}: {1}")]
    Io(String, String),
}

/// Version dir for a site binary (`<sites_root>/<id>/bin/<version>`).
pub fn version_dir(sites_root: &Path, site_id: &str, version: &str) -> PathBuf {
    sites_root.join(site_id).join("bin").join(version)
}

/// `bin` dir holding versions plus the `current` symlink.
pub fn bin_dir(sites_root: &Path, site_id: &str) -> PathBuf {
    sites_root.join(site_id).join("bin")
}

/// Download `url`, verify `sha256_hex`, extract into
/// `bin/<version>/`, make `exec` executable. Idempotent: when
/// `bin/<version>/<exec>` already exists the download is skipped.
pub async fn fetch_and_install(
    url: &str,
    sha256_hex: &str,
    exec: &str,
    sites_root: &Path,
    site_id: &str,
    version: &str,
) -> Result<PathBuf, DownloadError> {
    if sha256_hex.len() != 64 || hex::decode(sha256_hex).is_err() {
        return Err(DownloadError::BadSha(sha256_hex.to_string()));
    }
    let dest = version_dir(sites_root, site_id, version);
    if dest.join(exec).is_file() {
        return Ok(dest);
    }
    let bin = bin_dir(sites_root, site_id);
    std::fs::create_dir_all(&bin)
        .map_err(|e| DownloadError::Io(bin.display().to_string(), e.to_string()))?;

    let tmp = bin.join(format!(".tmp-{version}-{}", std::process::id()));
    download_to(url, sha256_hex, &tmp).await?;

    // Extract into a staging dir, then rename: a failed extract never
    // leaves a half-written version dir behind.
    let stage = bin.join(format!(".stage-{version}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage)
        .map_err(|e| DownloadError::Io(stage.display().to_string(), e.to_string()))?;
    if let Err(e) = extract_tar_gz(&tmp, &stage) {
        let _ = std::fs::remove_dir_all(&stage);
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    let _ = std::fs::remove_file(&tmp);

    let exe = stage.join(exec);
    if !exe.is_file() {
        let _ = std::fs::remove_dir_all(&stage);
        return Err(DownloadError::Extract(
            url.to_string(),
            format!("archive has no top-level {exec}"),
        ));
    }
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755))
        .map_err(|e| DownloadError::Io(exe.display().to_string(), e.to_string()))?;

    let _ = std::fs::remove_dir_all(&dest);
    std::fs::rename(&stage, &dest)
        .map_err(|e| DownloadError::Io(dest.display().to_string(), e.to_string()))?;
    Ok(dest)
}

async fn download_to(url: &str, sha256_hex: &str, tmp: &Path) -> Result<(), DownloadError> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| DownloadError::Fetch(url.to_string(), e.to_string()))?;
    if !resp.status().is_success() {
        return Err(DownloadError::Fetch(
            url.to_string(),
            format!("HTTP {}", resp.status()),
        ));
    }
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let mut hasher = Sha256::new();
    let mut file = tokio::fs::File::create(tmp)
        .await
        .map_err(|e| DownloadError::Io(tmp.display().to_string(), e.to_string()))?;
    let mut stream = resp.bytes_stream();
    use futures::StreamExt;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DownloadError::Fetch(url.to_string(), e.to_string()))?;
        hasher.update(&chunk);
        file.write_all(&chunk)
            .await
            .map_err(|e| DownloadError::Io(tmp.display().to_string(), e.to_string()))?;
    }
    file.flush()
        .await
        .map_err(|e| DownloadError::Io(tmp.display().to_string(), e.to_string()))?;
    drop(file);
    let got = hex::encode(hasher.finalize());
    if got != sha256_hex.to_lowercase() {
        let _ = std::fs::remove_file(tmp);
        return Err(DownloadError::Checksum(
            url.to_string(),
            sha256_hex.to_string(),
            got,
        ));
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<(), DownloadError> {
    let file = std::fs::File::open(archive)
        .map_err(|e| DownloadError::Io(archive.display().to_string(), e.to_string()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(dest)
        .map_err(|e| DownloadError::Extract(archive.display().to_string(), e.to_string()))?;
    Ok(())
}

/// Repoint `bin/current` at `version` atomically (symlink + rename).
/// No-op when it already points there. Fails when the version dir is
/// missing, so a bad update never stops the running site.
pub fn switch_current(
    sites_root: &Path,
    site_id: &str,
    version: &str,
) -> Result<bool, DownloadError> {
    let bin = bin_dir(sites_root, site_id);
    let target = bin.join(version);
    if !target.is_dir() {
        return Err(DownloadError::Io(
            target.display().to_string(),
            "no such version dir".to_string(),
        ));
    }
    let current = bin.join("current");
    if std::fs::read_link(&current).is_ok_and(|l| l == Path::new(version)) {
        return Ok(false);
    }
    let tmp_link = bin.join(format!(".current-{}-{}", version, std::process::id()));
    let _ = std::fs::remove_file(&tmp_link);
    std::os::unix::fs::symlink(version, &tmp_link)
        .map_err(|e| DownloadError::Io(tmp_link.display().to_string(), e.to_string()))?;
    std::fs::rename(&tmp_link, &current)
        .map_err(|e| DownloadError::Io(current.display().to_string(), e.to_string()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bad_sha_is_rejected_before_any_io() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        let err = rt
            .block_on(fetch_and_install(
                "http://127.0.0.1:9/nope.tar.gz",
                "zzz",
                "app",
                dir.path(),
                "s",
                "0.1.0",
            ))
            .expect_err("bad sha");
        assert!(matches!(err, DownloadError::BadSha(_)), "got {err:?}");
    }
}
