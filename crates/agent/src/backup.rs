//! Backup and restore of one site into
//! `/var/lib/setu/backups/<short>/<name>/{db.dump,dirs.tar.zst}`.
//!
//! The agent writes and reads the archive itself; the helper streams the
//! data dirs (`backup` on stdout, `restore` on stdin). Until WS5/WS6 move
//! to `backup_id`, `<name>` is the last component of the control plane's
//! `dest`/`src` path (validated); the rest of that path is ignored, so
//! the control plane cannot steer where anything is written.

use std::path::{Path, PathBuf};

use setu_proto::SiteSpec;

use crate::helper_client::Helper;
use crate::paths::check_short;
use crate::postgres::PostgresAdmin;
use crate::runtime::NativeRuntime;

/// Data-dir archive name inside a backup dir.
pub const DIRS_ARCHIVE: &str = "dirs.tar.zst";
/// Database dump name inside a backup dir.
pub const DB_DUMP: &str = "db.dump";

/// `[A-Za-z0-9._-]{1,64}`, not `.`/`..`: the backup's directory name.
pub fn backup_name(p: &Path) -> anyhow::Result<String> {
    let name = p
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string();
    let ok = !name.is_empty()
        && name.len() <= 64
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(name)
    } else {
        anyhow::bail!("invalid backup name")
    }
}

/// `<backups_root>/<short>/<name>`.
pub fn backup_dir(backups_root: &Path, short: &str, location: &Path) -> anyhow::Result<PathBuf> {
    check_short(short)?;
    Ok(backups_root.join(short).join(backup_name(location)?))
}

/// Distinct first components of a tar.zst's entries (validated dirs).
pub fn top_level_dirs(archive: &[u8]) -> anyhow::Result<Vec<String>> {
    let dec = zstd::stream::read::Decoder::new(archive)?;
    let mut ar = tar::Archive::new(dec);
    let mut dirs: Vec<String> = Vec::new();
    for entry in ar.entries()? {
        let entry = entry?;
        let path = entry.path()?;
        let Some(first) = path
            .components()
            .find(|c| !matches!(c, std::path::Component::CurDir))
        else {
            continue;
        };
        let name = first.as_os_str().to_string_lossy().into_owned();
        if !setu_common::validate::dir(&name) {
            anyhow::bail!("backup archive holds an invalid dir name");
        }
        if !dirs.contains(&name) {
            dirs.push(name);
        }
    }
    Ok(dirs)
}

/// Take a backup: `pg_dump` (stdout) into `db.dump`, helper `backup` into
/// `dirs.tar.zst`. Returns the backup dir.
pub async fn backup_site<H: Helper, P: PostgresAdmin, C>(
    rt: &NativeRuntime<H, P, C>,
    site: &SiteSpec,
    db: &str,
    dirs: &[String],
    dest: &Path,
) -> anyhow::Result<PathBuf> {
    let short = &site.short_id;
    let dir = backup_dir(&rt.config().backups_root, short, dest)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dir.display()))?;
    // The agent's own files (`setu`, 0750): `pg_dump` writes to stdout,
    // so postgres never writes here.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750))
            .map_err(|e| anyhow::anyhow!("cannot chmod {}: {e}", dir.display()))?;
    }
    let dump = rt.postgres().dump(db).await?;
    crate::config::atomic_write(&dir.join(DB_DUMP), &dump)
        .map_err(|e| anyhow::anyhow!("cannot write {DB_DUMP}: {e}"))?;
    let archive = rt.backup_dirs(short, dirs).await?;
    crate::config::atomic_write(&dir.join(DIRS_ARCHIVE), &archive)
        .map_err(|e| anyhow::anyhow!("cannot write {DIRS_ARCHIVE}: {e}"))?;
    Ok(dir)
}

/// Restore a backup (the caller stopped the site): the database is
/// replaced from `db.dump` (drop with force, create, `pg_restore` as the
/// site role `site_<short>`), then helper `restore` of the dirs in
/// `dirs.tar.zst`. WS5 adds `Restore.role`; until then the role is the
/// site's by construction (ledger R81).
pub async fn restore_site<H: Helper, P: PostgresAdmin, C>(
    rt: &NativeRuntime<H, P, C>,
    site: &SiteSpec,
    db: &str,
    src: &Path,
) -> anyhow::Result<()> {
    let short = &site.short_id;
    let dir = backup_dir(&rt.config().backups_root, short, src)?;
    let dump = std::fs::read(dir.join(DB_DUMP))
        .map_err(|e| anyhow::anyhow!("backup has no readable {DB_DUMP}: {e}"))?;
    let role = format!("site_{short}");
    rt.postgres().restore_replace(db, &role, dump).await?;
    let archive_path = dir.join(DIRS_ARCHIVE);
    if archive_path.is_file() {
        let archive = std::fs::read(&archive_path)
            .map_err(|e| anyhow::anyhow!("cannot read {DIRS_ARCHIVE}: {e}"))?;
        let dirs = top_level_dirs(&archive)?;
        if !dirs.is_empty() {
            rt.restore_dirs(short, &dirs, archive).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_are_one_safe_component() {
        assert_eq!(
            backup_name(Path::new("/var/lib/setu/backups/x/manual-1700000000")).expect("ok"),
            "manual-1700000000"
        );
        for bad in ["/", "..", "/a/..", "/a/b c", "/a/b\nc"] {
            backup_name(Path::new(bad)).expect_err(bad);
        }
        backup_dir(Path::new("/b"), "../evil0", Path::new("/x/y")).expect_err("short");
    }
}
