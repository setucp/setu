//! Backup and restore of one site: `pg_dump -Fc` plus a tar of the
//! manifest's backup dirs, into `<backups_root>/<site>/<ts>/`.
//!
//! Layout: `<dest>/db.dump` (custom format) + `<dest>/files.tar.gz`
//! (relative paths under the site data dir). Restore is the inverse:
//! stop (caller), `pg_restore`, extract over the data dir, start (caller).

use std::path::Path;

use setu_proto::SiteSpec;

use crate::postgres::PostgresAdmin;

/// Take a backup into `dest` (created as needed).
///
/// The leaf dir ends `0770`: the agent (owner) manages it while
/// `pg_dump` (group `postgres`, inherited via the setgid backups root
/// created by `install`) writes inside. Mode-only — no privilege needed.
/// Other site users are blocked at the leaf (`o---`).
pub async fn backup_site<P: PostgresAdmin>(
    pg: &P,
    site: &SiteSpec,
    db: &str,
    dirs: &[String],
    dest: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dest.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o770))
            .map_err(|e| anyhow::anyhow!("cannot chmod {}: {e}", dest.display()))?;
    }
    pg.dump(db, &dest.join("db.dump")).await?;
    archive_dirs(&site.data_dir, dirs, &dest.join("files.tar.gz"))?;
    Ok(())
}

/// Restore a backup from `src` (must hold `db.dump` + `files.tar.gz`).
pub async fn restore_site<P: PostgresAdmin>(
    pg: &P,
    site: &SiteSpec,
    db: &str,
    src: &Path,
) -> anyhow::Result<()> {
    let dump = src.join("db.dump");
    if !dump.is_file() {
        anyhow::bail!("backup {} has no db.dump", src.display());
    }
    pg.restore(db, &dump).await?;
    let files = src.join("files.tar.gz");
    if files.is_file() {
        extract_archive(&files, &site.data_dir)?;
    }
    Ok(())
}

fn archive_dirs(data_dir: &Path, dirs: &[String], dest: &Path) -> anyhow::Result<()> {
    let file = std::fs::File::create(dest)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dest.display()))?;
    let gz = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(gz);
    for dir in dirs {
        // Skip missing dirs (e.g. never-created optional media): the dump
        // is still a valid backup; the log names what was archived.
        let full = data_dir.join(dir);
        if full.is_dir() {
            builder
                .append_dir_all(dir, &full)
                .map_err(|e| anyhow::anyhow!("cannot archive {}: {e}", full.display()))?;
        }
    }
    let gz = builder
        .into_inner()
        .map_err(|e| anyhow::anyhow!("cannot finish {}: {e}", dest.display()))?;
    gz.finish()
        .map_err(|e| anyhow::anyhow!("cannot finish {}: {e}", dest.display()))?;
    Ok(())
}

fn extract_archive(archive: &Path, data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", data_dir.display()))?;
    let file = std::fs::File::open(archive)
        .map_err(|e| anyhow::anyhow!("cannot open {}: {e}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut ar = tar::Archive::new(gz);
    ar.unpack(data_dir)
        .map_err(|e| anyhow::anyhow!("cannot extract {}: {e}", archive.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::postgres::FakePostgres;

    #[tokio::test]
    async fn backup_then_restore_round_trips() {
        let dir = tempfile::tempdir().expect("tempdir");
        let site_dir = dir.path().join("sites").join("s1");
        let data = site_dir.join("data");
        std::fs::create_dir_all(data.join("media")).expect("mkdir");
        std::fs::write(data.join("media").join("a.txt"), b"hello").expect("write");
        let site = SiteSpec {
            site_id: "s1".to_string(),
            short_id: "s1".to_string(),
            user: "setu-site-s1".to_string(),
            data_dir: data.clone(),
            run_dir: site_dir.join("run"),
            port: 20001,
            exec: site_dir.join("bin").join("current").join("app"),
            args: vec![],
            env_file: site_dir.join("env"),
            limits: setu_proto::Limits {
                memory_mb: 512,
                cpu_percent: 100,
                disk_mb: 5120,
            },
            health_path: "/healthz".to_string(),
            binary: None,
        };
        let pg = FakePostgres::new();
        let dest = dir.path().join("backups").join("s1").join("ts");
        backup_site(&pg, &site, "site_s1", &["media".to_string()], &dest)
            .await
            .expect("backup");
        assert!(dest.join("db.dump").is_file());
        assert!(dest.join("files.tar.gz").is_file());
        // The leaf must admit the postgres group writer (real `pg_dump`
        // runs as the postgres user; the fake writes as us).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&dest).expect("stat").permissions().mode() & 0o777,
                0o770,
                "backup leaf must be 0770"
            );
        }

        // Delete the data, restore, expect it back.
        std::fs::remove_dir_all(&data).expect("rm data");
        restore_site(&pg, &site, "site_s1", &dest)
            .await
            .expect("restore");
        assert_eq!(
            std::fs::read(data.join("media").join("a.txt")).expect("read"),
            b"hello"
        );
    }

    #[tokio::test]
    async fn restore_without_dump_fails() {
        let dir = tempfile::tempdir().expect("tempdir");
        let site = SiteSpec {
            site_id: "s1".to_string(),
            short_id: "s1".to_string(),
            user: "setu-site-s1".to_string(),
            data_dir: dir.path().join("data"),
            run_dir: dir.path().join("run"),
            port: 20001,
            exec: dir.path().join("app"),
            args: vec![],
            env_file: dir.path().join("env"),
            limits: setu_proto::Limits {
                memory_mb: 512,
                cpu_percent: 100,
                disk_mb: 5120,
            },
            health_path: "/healthz".to_string(),
            binary: None,
        };
        let pg = FakePostgres::new();
        restore_site(&pg, &site, "site_s1", dir.path())
            .await
            .expect_err("missing dump must fail");
    }
}
