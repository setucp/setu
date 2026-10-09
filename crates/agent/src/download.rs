//! Verified downloads into `/var/lib/setu/downloads/<sha256>.tar.gz`.
//!
//! The agent (unprivileged) streams the archive to `<sha256>.part`,
//! hashes while writing, and renames only on a match; a mismatch keeps
//! nothing. The helper then re-opens the file `O_NOFOLLOW`, re-hashes it
//! and extracts it as root (`install`).

use std::path::{Path, PathBuf};

/// Errors from a verified download.
#[derive(Debug, thiserror::Error)]
pub enum DownloadError {
    #[error("download {0} failed: {1}")]
    Fetch(String, String),
    #[error("checksum mismatch for {0}: expected {1}, got {2}")]
    Checksum(String, String, String),
    #[error("invalid sha256: must be 64 hex chars")]
    BadSha,
    #[error("refusing {0}: plain http only to loopback (or with --insecure)")]
    PlainHttp(String),
    #[error("invalid url {0}")]
    BadUrl(String),
    #[error("download I/O for {0}: {1}")]
    Io(String, String),
}

/// What a download may use. `insecure` (agent `--insecure`) allows
/// plain http to any host; https is always allowed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadPolicy {
    pub insecure: bool,
}

/// Scheme/host check, before any network I/O.
pub fn check_url(url: &str, policy: &DownloadPolicy) -> Result<(), DownloadError> {
    let parsed = url::Url::parse(url).map_err(|_| DownloadError::BadUrl(url.to_string()))?;
    match parsed.scheme() {
        "https" => Ok(()),
        "http" => {
            let loopback = match parsed.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
                None => false,
            };
            if loopback || policy.insecure {
                Ok(())
            } else {
                Err(DownloadError::PlainHttp(url.to_string()))
            }
        }
        _ => Err(DownloadError::BadUrl(url.to_string())),
    }
}

fn normalise_sha(sha256: &str) -> Result<String, DownloadError> {
    let sha = sha256.to_ascii_lowercase();
    if sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(sha)
    } else {
        Err(DownloadError::BadSha)
    }
}

fn sha_of_file(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let mut f = std::fs::File::open(path).ok()?;
    let mut h = Sha256::new();
    std::io::copy(&mut f, &mut h).ok()?;
    Some(hex::encode(h.finalize()))
}

/// Download `url` into `<downloads_dir>/<sha256>.tar.gz`, verified.
/// Idempotent: an existing file with the right hash is reused.
pub async fn fetch_verified(
    url: &str,
    sha256: &str,
    downloads_dir: &Path,
    policy: &DownloadPolicy,
) -> Result<PathBuf, DownloadError> {
    let sha = normalise_sha(sha256)?;
    check_url(url, policy)?;
    let io =
        |p: &Path, e: std::io::Error| DownloadError::Io(p.display().to_string(), e.to_string());
    std::fs::create_dir_all(downloads_dir).map_err(|e| io(downloads_dir, e))?;
    let dest = downloads_dir.join(format!("{sha}.tar.gz"));
    if dest.is_file() && sha_of_file(&dest).as_deref() == Some(sha.as_str()) {
        return Ok(dest);
    }
    // One part file per attempt: two sites installing the same release
    // concurrently (one task per command) must not share it.
    let part = downloads_dir.join(format!("{sha}.{}.part", setu_common::random_hex(6)));
    let result = stream_to(url, &sha, &part).await;
    if let Err(e) = result {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &dest).map_err(|e| {
        let _ = std::fs::remove_file(&part);
        io(&dest, e)
    })?;
    Ok(dest)
}

async fn stream_to(url: &str, sha: &str, part: &Path) -> Result<(), DownloadError> {
    use futures::StreamExt;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncWriteExt;
    let fetch = |e: String| DownloadError::Fetch(url.to_string(), e);
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .map_err(|e| fetch(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(fetch(format!("HTTP {}", resp.status())));
    }
    let io = |e: std::io::Error| DownloadError::Io(part.display().to_string(), e.to_string());
    let mut file = tokio::fs::File::create(part).await.map_err(io)?;
    let mut hasher = Sha256::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| fetch(e.to_string()))?;
        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(io)?;
    }
    file.flush().await.map_err(io)?;
    file.sync_all().await.map_err(io)?;
    let got = hex::encode(hasher.finalize());
    if got != sha {
        return Err(DownloadError::Checksum(
            url.to_string(),
            sha.to_string(),
            got,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn serve(body: Vec<u8>) -> String {
        let app = axum::Router::new().route(
            "/a.tar.gz",
            axum::routing::get(move || {
                let b = body.clone();
                async move { axum::body::Body::from(b) }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let addr = l.local_addr().expect("addr");
        tokio::spawn(async move { axum::serve(l, app).await.expect("serve") });
        format!("http://{addr}/a.tar.gz")
    }

    fn sha(b: &[u8]) -> String {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(b))
    }

    #[tokio::test]
    async fn fetch_verified_rejects_wrong_sha_and_keeps_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let url = serve(b"archive bytes".to_vec()).await;
        let wrong = sha(b"something else");
        let err = fetch_verified(&url, &wrong, dir.path(), &DownloadPolicy::default())
            .await
            .expect_err("mismatch");
        assert!(matches!(err, DownloadError::Checksum(..)), "{err:?}");
        let left: Vec<_> = std::fs::read_dir(dir.path()).expect("ls").collect();
        assert!(left.is_empty(), "nothing kept: {left:?}");
        fetch_verified(&url, "zz", dir.path(), &DownloadPolicy::default())
            .await
            .expect_err("bad sha");

        let right = sha(b"archive bytes");
        let path = fetch_verified(
            &url,
            &right.to_uppercase(),
            dir.path(),
            &DownloadPolicy::default(),
        )
        .await
        .expect("ok");
        assert_eq!(path, dir.path().join(format!("{right}.tar.gz")));
        assert_eq!(std::fs::read(&path).expect("read"), b"archive bytes");
        let parts: Vec<_> = std::fs::read_dir(dir.path())
            .expect("ls")
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".part"))
            .collect();
        assert!(parts.is_empty(), "no part file left: {parts:?}");
    }

    #[tokio::test]
    async fn fetch_verified_allows_plain_http_for_loopback_only() {
        let strict = DownloadPolicy::default();
        let insecure = DownloadPolicy { insecure: true };
        check_url("http://127.0.0.1:8080/a", &strict).expect("v4 loopback");
        check_url("http://127.9.9.9/a", &strict).expect("v4 loopback range");
        check_url("http://[::1]:8080/a", &strict).expect("v6 loopback");
        check_url("http://localhost/a", &strict).expect("localhost");
        check_url("https://example.com/a", &strict).expect("https");
        for bad in [
            "http://example.com/a",
            "http://10.0.0.1/a",
            "http://127.0.0.1.evil.com/a",
        ] {
            assert!(
                matches!(check_url(bad, &strict), Err(DownloadError::PlainHttp(_))),
                "{bad}"
            );
            check_url(bad, &insecure).expect("insecure allows");
        }
        for bad in ["ftp://example.com/a", "file:///etc/passwd", "not a url"] {
            assert!(
                matches!(check_url(bad, &insecure), Err(DownloadError::BadUrl(_))),
                "{bad}"
            );
        }
        // The policy gates before any I/O: nothing is created.
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("dl");
        let err = fetch_verified("http://192.0.2.1/a", &"a".repeat(64), &target, &strict)
            .await
            .expect_err("refused");
        assert!(matches!(err, DownloadError::PlainHttp(_)));
        assert!(!target.exists());
    }
}
