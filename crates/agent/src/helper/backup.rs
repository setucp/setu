//! `backup {dirs}` → `tar --zstd` of `data/<dir>…` on stdout, and
//! `restore {dirs}` ← the same on stdin.
//!
//! `data/` belongs to the site user, so both directions walk it through
//! fds without following symlinks: backup skips symlinks and special
//! files (a planted link cannot exfiltrate a root-readable file), and
//! regular files the site user does not own or with more than one link
//! (a hard link to someone else's file; M4), and
//! restore extracts into a root-owned staging dir under the install
//! entry rules, chowns the result to the site user, then swaps it in.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Path, PathBuf};

use nix::fcntl::{openat, renameat, OFlag};
use nix::sys::stat::{fstat, Mode};
use serde_json::json;

use super::fs::{
    ensure_dir, kind, kind_at, list_dir, open_dir_nofollow, open_root, remove_tree_at, tmp_name,
    walk_nofollow, Kind,
};
use super::install::{extract_checked, Modes};
use super::{unit_name, validate, Ctx, DirsInput, HelperError, SITES};

/// Restored data: dirs 0750, files keep their exec bit, never world.
const RESTORE_MODES: Modes = Modes {
    dir: 0o750,
    file_exec: 0o750,
    file_plain: 0o640,
};

fn open_site(ctx: &Ctx, short: &str) -> Result<std::os::fd::OwnedFd, HelperError> {
    let root = open_root(&ctx.root)?;
    let mut comps: Vec<&str> = SITES.to_vec();
    comps.push(short);
    walk_nofollow(root.as_fd(), &comps)
}

/// Append `dir` (fd) and everything below it as `rel/…`. Regular files
/// not owned by `site_uid`, or with `st_nlink > 1`, are skipped and
/// reported on stderr: the site user could have hard-linked a file it
/// cannot read (root's, another site's) into its tree (M4).
#[allow(clippy::cast_sign_loss, clippy::manual_let_else)] // reviewed: kept as-is (R150)
fn append_tree<W: Write>(
    builder: &mut tar::Builder<W>,
    dir: BorrowedFd<'_>,
    rel: &Path,
    site_uid: u32,
) -> Result<(), HelperError> {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_mode(0o750);
    header.set_size(0);
    builder
        .append_data(&mut header, rel, std::io::empty())
        .map_err(|e| HelperError::io(rel.display(), e))?;
    for name in list_dir(dir)? {
        let child_rel = rel.join(&name);
        match kind_at(dir, &name)? {
            Some(Kind::Dir) => {
                let fd = open_dir_nofollow(dir, &name)?;
                append_tree(builder, fd.as_fd(), &child_rel, site_uid)?;
            }
            Some(Kind::File) => {
                let fd = match openat(
                    dir,
                    name.as_os_str(),
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
                    Mode::empty(),
                ) {
                    Ok(fd) => fd,
                    // Swapped for a link since the listing: skip it.
                    Err(_) => continue,
                };
                let st = fstat(fd.as_fd()).map_err(|e| HelperError::io(child_rel.display(), e))?;
                if kind(&st) != Kind::File {
                    continue;
                }
                if st.st_uid != site_uid || st.st_nlink > 1 {
                    eprintln!(
                        "setu helper backup: skipped {} (not owned by the site user, or hard-linked)",
                        child_rel.display()
                    );
                    continue;
                }
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Regular);
                header.set_mode((st.st_mode & 0o777) as u32);
                header.set_size(st.st_size as u64);
                header.set_mtime(st.st_mtime.max(0) as u64);
                let file = std::fs::File::from(fd);
                // Exactly `st_size` bytes even if the file grows meanwhile.
                builder
                    .append_data(&mut header, &child_rel, file.take(st.st_size as u64))
                    .map_err(|e| HelperError::io(child_rel.display(), e))?;
            }
            // Symlinks, sockets, fifos, devices: never archived.
            _ => {}
        }
    }
    Ok(())
}

#[allow(clippy::needless_pass_by_value)] // reviewed: kept as-is (R150)
pub fn backup(
    ctx: &Ctx,
    short: &str,
    input: DirsInput,
    stdout: &mut dyn Write,
) -> Result<serde_json::Value, HelperError> {
    validate::dirs(&input.dirs)?;
    let (site_uid, _) = ctx
        .users
        .site_ids(short)?
        .ok_or_else(|| HelperError::Refused("the site user does not exist".into()))?;
    let site = open_site(ctx, short)?;
    let data = open_dir_nofollow(site.as_fd(), OsStr::new("data"))?;
    let enc =
        zstd::stream::write::Encoder::new(stdout, 3).map_err(|e| HelperError::io("zstd", e))?;
    let mut builder = tar::Builder::new(enc);
    for dir in &input.dirs {
        match kind_at(data.as_fd(), OsStr::new(dir))? {
            None => continue, // never created: nothing to keep
            Some(Kind::Dir) => {}
            Some(_) => {
                return Err(HelperError::Refused(format!(
                    "data/{dir} is a symlink or not a directory"
                )))
            }
        }
        let fd = open_dir_nofollow(data.as_fd(), OsStr::new(dir))?;
        append_tree(&mut builder, fd.as_fd(), Path::new(dir), site_uid)?;
    }
    let enc = builder
        .into_inner()
        .map_err(|e| HelperError::io("tar", e))?;
    enc.finish()
        .and_then(std::io::Write::flush)
        .map_err(|e| HelperError::io("zstd", e))?;
    Ok(serde_json::Value::Null)
}

/// `chown -R` (through fds, no-follow) of `dir` to `uid:gid`.
fn chown_tree(
    ctx: &Ctx,
    dir: BorrowedFd<'_>,
    path: &Path,
    uid: u32,
    gid: u32,
) -> Result<(), HelperError> {
    ctx.own.chown(dir, path, uid, gid)?;
    for name in list_dir(dir)? {
        let child = path.join(&name);
        match kind_at(dir, &name)? {
            Some(Kind::Dir) => {
                let fd = open_dir_nofollow(dir, &name)?;
                chown_tree(ctx, fd.as_fd(), &child, uid, gid)?;
            }
            Some(Kind::File) => {
                let fd = openat(
                    dir,
                    name.as_os_str(),
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )
                .map_err(|e| HelperError::io(child.display(), e))?;
                ctx.own.chown(fd.as_fd(), &child, uid, gid)?;
            }
            // The staging tree is ours and holds only files and dirs.
            _ => {
                return Err(HelperError::Refused(format!(
                    "unexpected {}",
                    child.display()
                )))
            }
        }
    }
    Ok(())
}

/// One `renameat` (injectable so the failure path is tested).
type Rename<'r> = dyn FnMut(BorrowedFd<'_>, &OsStr, BorrowedFd<'_>, &OsStr) -> nix::Result<()> + 'r;

/// Move each `data/<dir>` to `stage/.old-<dir>` and `stage/<dir>` to
/// `data/<dir>`. On the first failure every step taken is undone in
/// reverse (new dirs back to `stage/<dir>`, old ones back to
/// `data/<dir>`), so the caller's removal of `stage` never deletes the
/// site's current data.
fn swap_in(
    data: BorrowedFd<'_>,
    stage: BorrowedFd<'_>,
    dirs: &[String],
    rename: &mut Rename<'_>,
) -> Result<(), HelperError> {
    // (dir, its old copy is in stage, its new copy is in data)
    let mut done: Vec<(&str, bool, bool)> = Vec::new();
    let mut failure = None;
    for dir in dirs {
        let name = OsStr::new(dir);
        let old = OsString::from(format!(".old-{dir}"));
        let had_old = match kind_at(data, name) {
            Ok(k) => k.is_some(),
            Err(e) => {
                failure = Some(e);
                break;
            }
        };
        if had_old {
            if let Err(e) = rename(data, name, stage, old.as_os_str()) {
                failure = Some(HelperError::io(format!("data/{dir}"), e));
                break;
            }
        }
        done.push((dir, had_old, false));
        if let Err(e) = rename(stage, name, data, name) {
            failure = Some(HelperError::io(format!("data/{dir}"), e));
            break;
        }
        if let Some(last) = done.last_mut() {
            last.2 = true;
        }
    }
    let Some(failure) = failure else {
        return Ok(());
    };
    for (dir, old_staged, new_in) in done.into_iter().rev() {
        let name = OsStr::new(dir);
        if new_in {
            if let Err(e) = rename(data, name, stage, name) {
                eprintln!("setu helper restore: cannot move data/{dir} back out: {e}");
                continue;
            }
        }
        if old_staged {
            let old = OsString::from(format!(".old-{dir}"));
            if let Err(e) = rename(stage, old.as_os_str(), data, name) {
                eprintln!("setu helper restore: cannot put the old data/{dir} back: {e}");
            }
        }
    }
    Err(failure)
}

#[allow(clippy::needless_pass_by_value)] // reviewed: kept as-is (R150)
pub fn restore(
    ctx: &Ctx,
    short: &str,
    input: DirsInput,
    stdin: &mut dyn Read,
) -> Result<serde_json::Value, HelperError> {
    validate::dirs(&input.dirs)?;
    if ctx.units.is_active(&unit_name(short))? {
        return Err(HelperError::Refused(
            "unit is active; stop the site before restore".into(),
        ));
    }
    let (uid, gid) = ctx.users.ensure_site_user(short)?;
    let site = open_site(ctx, short)?;
    let data = open_dir_nofollow(site.as_fd(), OsStr::new("data"))?;
    let staging = tmp_name("restore");
    let (stage, _) = ensure_dir(site.as_fd(), &staging, 0o700)?;
    let stage_path: PathBuf = ctx.site_path(short, &[]).join(&staging);
    let result = (|| {
        let dec =
            zstd::stream::read::Decoder::new(stdin).map_err(|e| HelperError::io("zstd", e))?;
        extract_checked(dec, stage.as_fd(), Some(&input.dirs), RESTORE_MODES)?;
        for dir in &input.dirs {
            // A dir absent from the archive was empty at backup time.
            let (fd, _) = ensure_dir(stage.as_fd(), OsStr::new(dir), RESTORE_MODES.dir)?;
            let final_path = ctx.site_path(short, &["data", dir]);
            chown_tree(ctx, fd.as_fd(), &final_path, uid, gid)?;
            ctx.own.chmod(fd.as_fd(), &final_path, RESTORE_MODES.dir)?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        let _ = remove_tree_at(site.as_fd(), &staging);
        return Err(e);
    }
    // Swap each dir in: the old one moves into staging (rename never
    // follows a link, so a planted symlink is just moved), then the new
    // one into data/, then staging (old data) is removed. A failed rename
    // first puts every old dir back (M5).
    let swap = swap_in(
        data.as_fd(),
        stage.as_fd(),
        &input.dirs,
        &mut |a, b, c, d| renameat(a, b, c, d),
    );
    let cleanup = remove_tree_at(site.as_fd(), &staging);
    swap?;
    cleanup.map_err(|e| HelperError::io(stage_path.display(), e))?;
    Ok(json!({ "dirs": input.dirs }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(root: &Path, files: &[(&str, &str)]) {
        for (rel, body) in files {
            let p = root.join(rel);
            std::fs::create_dir_all(p.parent().expect("parent")).expect("mkdir");
            std::fs::write(p, body).expect("write");
        }
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).expect("read")
    }

    /// A rename failing mid-swap puts every old dir back in `data/`
    /// before the staging dir (which held them) is removed (M5).
    #[test]
    fn restore_swap_failure_restores_old_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let data_path = tmp.path().join("data");
        let stage_path = tmp.path().join("stage");
        tree(
            &data_path,
            &[("media/a.txt", "old-a"), ("uploads/u.txt", "old-u")],
        );
        tree(
            &stage_path,
            &[("media/a.txt", "new-a"), ("uploads/u.txt", "new-u")],
        );
        let data = open_root(&data_path).expect("data");
        let stage = open_root(&stage_path).expect("stage");
        let dirs = vec!["media".to_string(), "uploads".to_string()];

        // Calls: media out, media in, uploads out, uploads in (fails).
        let mut calls = 0;
        let mut flaky = |from: BorrowedFd<'_>, a: &OsStr, to: BorrowedFd<'_>, b: &OsStr| {
            calls += 1;
            if calls == 4 {
                return Err(nix::errno::Errno::EIO);
            }
            renameat(from, a, to, b)
        };
        let err = swap_in(data.as_fd(), stage.as_fd(), &dirs, &mut flaky);
        assert!(err.is_err(), "the failed rename is reported");
        assert_eq!(read(&data_path.join("media/a.txt")), "old-a");
        assert_eq!(read(&data_path.join("uploads/u.txt")), "old-u");
        assert!(!stage_path.join(".old-media").exists());
        assert!(!stage_path.join(".old-uploads").exists());

        // Without a failure every new dir is in place, the old ones staged.
        let mut real = |from: BorrowedFd<'_>, a: &OsStr, to: BorrowedFd<'_>, b: &OsStr| {
            renameat(from, a, to, b)
        };
        swap_in(data.as_fd(), stage.as_fd(), &dirs, &mut real).expect("swap");
        assert_eq!(read(&data_path.join("media/a.txt")), "new-a");
        assert_eq!(read(&data_path.join("uploads/u.txt")), "new-u");
        assert_eq!(read(&stage_path.join(".old-media/a.txt")), "old-a");
    }
}
