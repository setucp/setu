//! fd-relative filesystem primitives: every component below the helper
//! root is opened with `openat(O_NOFOLLOW)`, so a symlink planted
//! anywhere on the way is refused rather than followed (no `lstat` then
//! `open` race). Writes are temp-file-plus-rename in the same dir.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use nix::errno::Errno;
use nix::fcntl::{openat, renameat, AtFlags, OFlag, AT_FDCWD};
use nix::sys::stat::{fstatat, mkdirat, FileStat, Mode, SFlag};
use nix::unistd::{unlinkat, UnlinkatFlags};

use super::{HelperError, Ownership};

const DIR_FLAGS: OFlag = OFlag::O_DIRECTORY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_RDONLY)
    .union(OFlag::O_CLOEXEC);

/// Kind of an existing directory entry (never following a symlink).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// Map an errno from a no-follow open to a refusal or an I/O error.
fn open_err(name: &OsStr, e: Errno) -> HelperError {
    match e {
        Errno::ELOOP | Errno::ENOTDIR => HelperError::Refused(format!(
            "{} is a symlink or not a directory",
            name.to_string_lossy()
        )),
        other => HelperError::io(name.to_string_lossy(), other),
    }
}

/// A single path component: no `/`, not empty, not `.` or `..`.
fn check_name(name: &OsStr) -> Result<(), HelperError> {
    let b = name.as_bytes();
    if b.is_empty() || b == b"." || b == b".." || b.contains(&b'/') || b.contains(&0) {
        return Err(HelperError::Invalid("path component".to_string()));
    }
    Ok(())
}

/// Open the helper root itself (trusted: `/` or the test tempdir).
pub fn open_root(root: &Path) -> Result<OwnedFd, HelperError> {
    openat(
        AT_FDCWD,
        root,
        OFlag::O_DIRECTORY | OFlag::O_RDONLY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| HelperError::io(root.display(), e))
}

/// Open one child directory without following a symlink.
pub fn open_dir_nofollow(dir: BorrowedFd<'_>, name: &OsStr) -> Result<OwnedFd, HelperError> {
    check_name(name)?;
    openat(dir, name, DIR_FLAGS, Mode::empty()).map_err(|e| open_err(name, e))
}

/// Walk `comps` from `root`, each step `O_NOFOLLOW|O_DIRECTORY`.
pub fn walk_nofollow(root: BorrowedFd<'_>, comps: &[&str]) -> Result<OwnedFd, HelperError> {
    let mut cur =
        openat(root, ".", DIR_FLAGS, Mode::empty()).map_err(|e| HelperError::io(".", e))?;
    for c in comps {
        cur = open_dir_nofollow(cur.as_fd(), OsStr::new(c))?;
    }
    Ok(cur)
}

/// Like [`walk_nofollow`] but creates missing components (mode 0755,
/// owned by the caller: root in production). For the fixed system dirs.
pub fn walk_create(root: BorrowedFd<'_>, comps: &[&str]) -> Result<OwnedFd, HelperError> {
    let mut cur =
        openat(root, ".", DIR_FLAGS, Mode::empty()).map_err(|e| HelperError::io(".", e))?;
    for c in comps {
        cur = ensure_dir(cur.as_fd(), OsStr::new(c), 0o755)?.0;
    }
    Ok(cur)
}

/// `mkdirat` (an existing directory is fine), then open it no-follow.
/// Returns the fd and whether it was created.
pub fn ensure_dir(
    dir: BorrowedFd<'_>,
    name: &OsStr,
    mode: u32,
) -> Result<(OwnedFd, bool), HelperError> {
    check_name(name)?;
    let created = match mkdirat(dir, name, Mode::from_bits_truncate(mode & 0o777)) {
        Ok(()) => true,
        Err(Errno::EEXIST) => false,
        Err(e) => return Err(HelperError::io(name.to_string_lossy(), e)),
    };
    Ok((open_dir_nofollow(dir, name)?, created))
}

/// `fstatat(AT_SYMLINK_NOFOLLOW)`; `None` when missing.
pub fn stat_at(dir: BorrowedFd<'_>, name: &OsStr) -> Result<Option<FileStat>, HelperError> {
    check_name(name)?;
    match fstatat(dir, name, AtFlags::AT_SYMLINK_NOFOLLOW) {
        Ok(st) => Ok(Some(st)),
        Err(Errno::ENOENT) => Ok(None),
        Err(e) => Err(HelperError::io(name.to_string_lossy(), e)),
    }
}

/// Kind of a stat result.
pub fn kind(st: &FileStat) -> Kind {
    let fmt = SFlag::from_bits_truncate(st.st_mode) & SFlag::S_IFMT;
    if fmt == SFlag::S_IFREG {
        Kind::File
    } else if fmt == SFlag::S_IFDIR {
        Kind::Dir
    } else if fmt == SFlag::S_IFLNK {
        Kind::Symlink
    } else {
        Kind::Other
    }
}

/// Kind of `name` in `dir`, `None` when missing.
pub fn kind_at(dir: BorrowedFd<'_>, name: &OsStr) -> Result<Option<Kind>, HelperError> {
    Ok(stat_at(dir, name)?.as_ref().map(kind))
}

/// Read a regular file without following a symlink; `None` when missing.
/// Anything but a regular file is refused.
pub fn read_file_nofollow(
    dir: BorrowedFd<'_>,
    name: &OsStr,
) -> Result<Option<Vec<u8>>, HelperError> {
    match kind_at(dir, name)? {
        None => return Ok(None),
        Some(Kind::File) => {}
        Some(_) => {
            return Err(HelperError::Refused(format!(
                "{} is not a regular file",
                name.to_string_lossy()
            )))
        }
    }
    let fd = openat(
        dir,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| open_err(name, e))?;
    let mut f = std::fs::File::from(fd);
    let mut buf = Vec::new();
    std::io::Read::read_to_end(&mut f, &mut buf)
        .map_err(|e| HelperError::io(name.to_string_lossy(), e))?;
    Ok(Some(buf))
}

/// Unique temp name in the helper's own namespace (leading dot).
pub fn tmp_name(prefix: &str) -> OsString {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    OsString::from(format!(".setu-{prefix}-{}-{nanos}", std::process::id()))
}

/// Create a new file exclusively (`O_CREAT|O_EXCL|O_NOFOLLOW`), mode
/// `0600` until the caller sets the final mode.
pub fn create_new(dir: BorrowedFd<'_>, name: &OsStr) -> Result<std::fs::File, HelperError> {
    check_name(name)?;
    let fd = openat(
        dir,
        name,
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_WRONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|e| match e {
        Errno::EEXIST => HelperError::Refused(format!("{} already exists", name.to_string_lossy())),
        other => open_err(name, other),
    })?;
    Ok(std::fs::File::from(fd))
}

/// Atomic write of `bytes` to `dir/name`: temp file, owner + mode set on
/// the open fd, fsync, `renameat`. A symlink or non-file at `name` is
/// refused (rename would replace the link, but its presence means
/// someone is playing games). `path` names the target in records.
#[allow(clippy::too_many_arguments)]
pub fn write_atomic(
    own: &dyn Ownership,
    dir: BorrowedFd<'_>,
    name: &OsStr,
    path: &Path,
    bytes: &[u8],
    uid: u32,
    gid: u32,
    mode: u32,
) -> Result<(), HelperError> {
    match kind_at(dir, name)? {
        None | Some(Kind::File) => {}
        Some(_) => {
            return Err(HelperError::Refused(format!(
                "{} is a symlink or not a regular file",
                path.display()
            )))
        }
    }
    let tmp = tmp_name("tmp");
    let mut f = create_new(dir, &tmp)?;
    let result = (|| {
        own.chown(f.as_fd(), path, uid, gid)?;
        own.chmod(f.as_fd(), path, mode)?;
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(|e| HelperError::io(path.display(), e))?;
        renameat(dir, tmp.as_os_str(), dir, name).map_err(|e| HelperError::io(path.display(), e))
    })();
    if result.is_err() {
        let _ = unlinkat(dir, tmp.as_os_str(), UnlinkatFlags::NoRemoveDir);
    }
    result
}

/// Remove `dir/name` recursively without ever following a symlink
/// (symlinks are unlinked, not traversed). Missing is fine.
pub fn remove_tree_at(dir: BorrowedFd<'_>, name: &OsStr) -> Result<(), HelperError> {
    match kind_at(dir, name)? {
        None => Ok(()),
        Some(Kind::Dir) => {
            let child = open_dir_nofollow(dir, name)?;
            for entry in list_dir(child.as_fd())? {
                remove_tree_at(child.as_fd(), &entry)?;
            }
            unlinkat(dir, name, UnlinkatFlags::RemoveDir)
                .map_err(|e| HelperError::io(name.to_string_lossy(), e))
        }
        Some(_) => unlinkat(dir, name, UnlinkatFlags::NoRemoveDir)
            .map_err(|e| HelperError::io(name.to_string_lossy(), e)),
    }
}

/// Entry names of an open directory (no `.`/`..`), sorted.
pub fn list_dir(dir: BorrowedFd<'_>) -> Result<Vec<OsString>, HelperError> {
    let fd = openat(dir, ".", DIR_FLAGS, Mode::empty()).map_err(|e| HelperError::io(".", e))?;
    let mut d = nix::dir::Dir::from_fd(fd).map_err(|e| HelperError::io("readdir", e))?;
    let mut names = Vec::new();
    for entry in d.iter() {
        let entry = entry.map_err(|e| HelperError::io("readdir", e))?;
        let b = entry.file_name().to_bytes();
        if b == b"." || b == b".." {
            continue;
        }
        names.push(OsStr::from_bytes(b).to_os_string());
    }
    names.sort();
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walk_refuses_symlinked_component() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("real/inner")).expect("mkdir");
        std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link"))
            .expect("symlink");
        let root = open_root(dir.path()).expect("root");
        walk_nofollow(root.as_fd(), &["real", "inner"]).expect("real path");
        let err = walk_nofollow(root.as_fd(), &["link", "inner"]).expect_err("symlink");
        assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
    }

    #[test]
    fn write_atomic_refuses_symlinked_target() {
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"keep").expect("write");
        std::os::unix::fs::symlink(&victim, dir.path().join("env")).expect("symlink");
        let root = open_root(dir.path()).expect("root");
        let own = crate::helper::Recording::new();
        let err = write_atomic(
            &own,
            root.as_fd(),
            OsStr::new("env"),
            &dir.path().join("env"),
            b"x",
            0,
            0,
            0o600,
        )
        .expect_err("symlink target");
        assert!(matches!(err, HelperError::Refused(_)), "{err:?}");
        assert_eq!(std::fs::read(&victim).expect("read"), b"keep");
        assert_eq!(list_dir(root.as_fd()).expect("ls").len(), 2, "no temp left");
    }

    #[test]
    fn remove_tree_unlinks_symlinks_without_following() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).expect("mkdir");
        std::fs::write(outside.join("keep"), b"x").expect("write");
        std::fs::create_dir_all(dir.path().join("tree/sub")).expect("mkdir");
        std::os::unix::fs::symlink(&outside, dir.path().join("tree/sub/link")).expect("symlink");
        let root = open_root(dir.path()).expect("root");
        remove_tree_at(root.as_fd(), OsStr::new("tree")).expect("remove");
        assert!(!dir.path().join("tree").exists());
        assert!(outside.join("keep").is_file(), "symlink target untouched");
        remove_tree_at(root.as_fd(), OsStr::new("tree")).expect("missing is fine");
    }
}
