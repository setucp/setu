//! Backup and restore of one site into
//! `/var/lib/setu/backups/<short>/<backup_id>/{db.dump,dirs.tar.zst}`.
//!
//! The agent writes and reads the archive itself (the backups tree is
//! `setu`-owned, so deleting a backup needs no helper verb); the helper
//! streams the data dirs (`backup` on stdout, `restore` on stdin) and
//! `pg_dump`/`pg_restore` stream the dump, always between a pipe and a
//! file, never through a whole buffer (I3). The
//! directory name is the control plane's `backup_id`, validated as one
//! safe path component; nothing else path-like arrives.

use std::path::{Path, PathBuf};

use crate::helper_client::Helper;
use crate::paths::check_short;
use crate::postgres::PostgresAdmin;
use crate::runtime::NativeRuntime;

/// Data-dir archive name inside a backup dir.
pub const DIRS_ARCHIVE: &str = "dirs.tar.zst";
/// Database dump name inside a backup dir.
pub const DB_DUMP: &str = "db.dump";

/// `[A-Za-z0-9._-]{1,64}`, not `.`/`..`: a backup id usable as one
/// directory name.
pub fn check_backup_id(id: &str) -> anyhow::Result<()> {
    let ok = !id.is_empty()
        && id.len() <= 64
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b));
    if ok {
        Ok(())
    } else {
        anyhow::bail!("invalid backup id")
    }
}

/// `<backups_root>/<short>/<backup_id>`.
pub fn backup_dir(backups_root: &Path, short: &str, backup_id: &str) -> anyhow::Result<PathBuf> {
    check_short(short)?;
    check_backup_id(backup_id)?;
    Ok(backups_root.join(short).join(backup_id))
}

/// Distinct first components of the tar.zst at `archive` (validated
/// dirs), read as a stream.
pub fn top_level_dirs(archive: &Path) -> anyhow::Result<Vec<String>> {
    let file = std::fs::File::open(archive)
        .map_err(|e| anyhow::anyhow!("cannot read {DIRS_ARCHIVE}: {e}"))?;
    let dec = zstd::stream::read::Decoder::new(std::io::BufReader::new(file))?;
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

/// `<dir>/.<name>.part`: where a file streams before its rename.
fn part_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!(".{name}.part"))
}

/// Take a backup: `pg_dump` (stdout) streamed into `db.dump`, helper
/// `backup` streamed into `dirs.tar.zst` (each through a `.part` file
/// renamed when complete). Returns the backup dir and its size in bytes.
pub async fn backup_site<H: Helper, P: PostgresAdmin, C>(
    rt: &NativeRuntime<H, P, C>,
    short: &str,
    db: &str,
    dirs: &[String],
    backup_id: &str,
) -> anyhow::Result<(PathBuf, u64)> {
    let dir = backup_dir(&rt.config().backups_root, short, backup_id)?;
    std::fs::create_dir_all(&dir)
        .map_err(|e| anyhow::anyhow!("cannot create {}: {e}", dir.display()))?;
    // The agent's own files (`setu`, 0750): `pg_dump` writes to stdout,
    // so postgres never writes here.
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o750))
            .map_err(|e| anyhow::anyhow!("cannot chmod {}: {e}", dir.display()))?;
    }
    let mut total = 0;
    for (name, is_db) in [(DB_DUMP, true), (DIRS_ARCHIVE, false)] {
        let part = part_path(&dir, name);
        let written = if is_db {
            rt.postgres().dump(db, &part).await
        } else {
            rt.backup_dirs(short, dirs, &part).await
        };
        let size = match written {
            Ok(size) => size,
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                return Err(e);
            }
        };
        std::fs::rename(&part, dir.join(name))
            .map_err(|e| anyhow::anyhow!("cannot write {name}: {e}"))?;
        total += size;
    }
    Ok((dir, total))
}

/// Restore a backup (the caller stopped the site): the database is
/// replaced from `db.dump` (drop with force, create, `pg_restore` as
/// `role`, the file streamed to its stdin), then helper `restore` of the
/// requested `dirs` that the archive holds (streamed the same way).
pub async fn restore_site<H: Helper, P: PostgresAdmin, C>(
    rt: &NativeRuntime<H, P, C>,
    short: &str,
    db: &str,
    role: &str,
    dirs: &[String],
    backup_id: &str,
) -> anyhow::Result<()> {
    let dir = backup_dir(&rt.config().backups_root, short, backup_id)?;
    let dump = dir.join(DB_DUMP);
    if !dump.is_file() {
        anyhow::bail!("backup has no readable {DB_DUMP}");
    }
    rt.postgres().restore_replace(db, role, &dump).await?;
    let archive_path = dir.join(DIRS_ARCHIVE);
    if archive_path.is_file() {
        let present = top_level_dirs(&archive_path)?;
        let wanted: Vec<String> = dirs
            .iter()
            .filter(|d| present.contains(d))
            .cloned()
            .collect();
        if !wanted.is_empty() {
            rt.restore_dirs(short, &wanted, &archive_path).await?;
        }
    }
    Ok(())
}

/// Delete one backup's directory (idempotent: a missing one is fine).
/// The backups tree is the agent's own, so no helper verb is involved.
pub fn delete_backup(backups_root: &Path, short: &str, backup_id: &str) -> anyhow::Result<bool> {
    let dir = backup_dir(backups_root, short, backup_id)?;
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => anyhow::bail!("cannot inspect backup: {e}"),
        // `remove_dir_all` never follows a symlink: it removes the link.
        Ok(_) => std::fs::remove_dir_all(&dir)
            .map(|()| true)
            .map_err(|e| anyhow::anyhow!("cannot delete backup: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_ids_are_one_safe_component() {
        assert_eq!(
            backup_dir(Path::new("/b"), "abcd1234", "01JBACKUP0000000000000001").expect("ok"),
            PathBuf::from("/b/abcd1234/01JBACKUP0000000000000001")
        );
        for bad in ["", ".", "..", "a/b", "../x", "b c", "b\nc", &"x".repeat(65)] {
            check_backup_id(bad).expect_err(bad);
        }
        backup_dir(Path::new("/b"), "../evil0", "x").expect_err("short");
    }

    #[test]
    fn delete_backup_removes_only_that_dir_and_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let keep = root.path().join("abcd1234/keep");
        let gone = root.path().join("abcd1234/gone");
        std::fs::create_dir_all(&keep).expect("mk");
        std::fs::create_dir_all(&gone).expect("mk");
        std::fs::write(gone.join(DB_DUMP), b"x").expect("w");
        assert!(delete_backup(root.path(), "abcd1234", "gone").expect("delete"));
        assert!(!gone.exists() && keep.exists());
        assert!(!delete_backup(root.path(), "abcd1234", "gone").expect("again"));
        // A symlinked backup dir: the link goes, its target stays.
        let outside = tempfile::tempdir().expect("tempdir");
        std::fs::write(outside.path().join("precious"), b"y").expect("w");
        std::os::unix::fs::symlink(outside.path(), root.path().join("abcd1234/link")).expect("ln");
        delete_backup(root.path(), "abcd1234", "link").expect("delete link");
        assert!(outside.path().join("precious").exists());
        delete_backup(root.path(), "abcd1234", "../keep").expect_err("traversal");
    }
}
