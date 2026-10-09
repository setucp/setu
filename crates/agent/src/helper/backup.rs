//! `backup {dirs}` → `tar --zstd` of `data/<dir>…` on stdout, and
//! `restore {dirs}` ← the same on stdin.
//!
//! `data/` belongs to the site user, so both directions walk it through
//! fds without following symlinks: backup skips symlinks and special
//! files (a planted link cannot exfiltrate a root-readable file), and
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

/// Append `dir` (fd) and everything below it as `rel/…`.
fn append_tree<W: Write>(
    builder: &mut tar::Builder<W>,
    dir: BorrowedFd<'_>,
    rel: &Path,
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
                append_tree(builder, fd.as_fd(), &child_rel)?;
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

pub fn backup(
    ctx: &Ctx,
    short: &str,
    input: DirsInput,
    stdout: &mut dyn Write,
) -> Result<serde_json::Value, HelperError> {
    validate::dirs(&input.dirs)?;
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
        append_tree(&mut builder, fd.as_fd(), Path::new(dir))?;
    }
    let enc = builder
        .into_inner()
        .map_err(|e| HelperError::io("tar", e))?;
    enc.finish()
        .and_then(|w| w.flush())
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
    // one into data/, then staging (old data) is removed.
    let swap = (|| {
        for dir in &input.dirs {
            let name = OsStr::new(dir);
            let old: OsString = OsString::from(format!(".old-{dir}"));
            if kind_at(data.as_fd(), name)?.is_some() {
                renameat(data.as_fd(), name, stage.as_fd(), old.as_os_str())
                    .map_err(|e| HelperError::io(format!("data/{dir}"), e))?;
            }
            renameat(stage.as_fd(), name, data.as_fd(), name)
                .map_err(|e| HelperError::io(format!("data/{dir}"), e))?;
        }
        Ok::<(), HelperError>(())
    })();
    let cleanup = remove_tree_at(site.as_fd(), &staging);
    swap?;
    cleanup.map_err(|e| HelperError::io(stage_path.display(), e))?;
    Ok(json!({ "dirs": input.dirs }))
}
