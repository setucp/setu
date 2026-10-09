//! `install {version, sha256}` and `switch-version {version}`, plus the
//! checked archive extractor `restore` shares.
//!
//! An archive may hold only regular files and directories with relative,
//! `..`-free names and no setuid/setgid/sticky bit. Anything else
//! (symlink, hard link, device, fifo, absolute name, `..`, special bit,
//! duplicate name) refuses the whole operation and the staging dir is
//! removed: nothing is left behind.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Seek};
use std::os::fd::{AsFd, BorrowedFd};
use std::path::{Component, Path};

use nix::fcntl::{openat, readlinkat, renameat, OFlag};
use nix::sys::stat::{fchmod, fstat, Mode};
use nix::unistd::symlinkat;
use serde_json::json;
use sha2::{Digest, Sha256};

use super::fs::{
    create_new, ensure_dir, kind, kind_at, open_dir_nofollow, open_root, remove_tree_at, tmp_name,
    walk_nofollow, Kind,
};
use super::{validate, Ctx, HelperError, DOWNLOADS, SITES};

/// `install` input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallInput {
    pub version: String,
    pub sha256: String,
}

/// `switch-version` input.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionInput {
    pub version: String,
}

/// Modes for extracted objects.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Modes {
    /// Every directory.
    pub dir: u32,
    /// Regular file: `true` when the archive mode has any execute bit.
    pub file_exec: u32,
    pub file_plain: u32,
}

/// Binaries: dirs 0755, executables 0755, other files 0644.
pub(crate) const INSTALL_MODES: Modes = Modes {
    dir: 0o755,
    file_exec: 0o755,
    file_plain: 0o644,
};

/// Validated relative components of an entry name (`.` dropped).
fn checked_components(path: &Path) -> Result<Vec<OsString>, HelperError> {
    let mut out = Vec::new();
    for c in path.components() {
        match c {
            Component::Normal(n) => out.push(n.to_os_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                return Err(HelperError::Refused("archive entry with `..`".to_string()))
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(HelperError::Refused(
                    "archive entry with an absolute name".to_string(),
                ))
            }
        }
    }
    Ok(out)
}

/// Extract `reader` (a tar stream) into the directory `dest` under the
/// entry rules. `allowed_top`, when given, limits the first component.
/// On error the caller removes `dest`.
pub(crate) fn extract_checked<R: Read>(
    reader: R,
    dest: BorrowedFd<'_>,
    allowed_top: Option<&[String]>,
    modes: Modes,
) -> Result<(), HelperError> {
    let mut ar = tar::Archive::new(reader);
    let entries = ar
        .entries()
        .map_err(|e| HelperError::Refused(format!("archive: {e}")))?;
    for entry in entries {
        let mut entry = entry.map_err(|e| HelperError::Refused(format!("archive: {e}")))?;
        let et = entry.header().entry_type();
        let is_dir = match et {
            tar::EntryType::Regular => false,
            tar::EntryType::Directory => true,
            tar::EntryType::XGlobalHeader => continue,
            tar::EntryType::Symlink => {
                return Err(HelperError::Refused("archive entry is a symlink".into()))
            }
            tar::EntryType::Link => {
                return Err(HelperError::Refused("archive entry is a hard link".into()))
            }
            tar::EntryType::Char | tar::EntryType::Block => {
                return Err(HelperError::Refused("archive entry is a device".into()))
            }
            tar::EntryType::Fifo => {
                return Err(HelperError::Refused("archive entry is a fifo".into()))
            }
            other => {
                return Err(HelperError::Refused(format!(
                    "archive entry type {other:?} not allowed"
                )))
            }
        };
        let mode = entry
            .header()
            .mode()
            .map_err(|e| HelperError::Refused(format!("archive: {e}")))?;
        if mode & 0o7000 != 0 {
            return Err(HelperError::Refused(
                "archive entry has a setuid, setgid or sticky bit".into(),
            ));
        }
        let path = entry
            .path()
            .map_err(|e| HelperError::Refused(format!("archive: {e}")))?
            .into_owned();
        let comps = checked_components(&path)?;
        let Some((last, parents)) = comps.split_last() else {
            if is_dir {
                continue; // `./`
            }
            return Err(HelperError::Refused(
                "archive entry with an empty name".into(),
            ));
        };
        if let Some(allowed) = allowed_top {
            let top = comps[0].to_string_lossy();
            if !allowed.iter().any(|d| *d == top) {
                return Err(HelperError::Refused(format!(
                    "archive entry outside the requested dirs: {top}"
                )));
            }
        }
        // Parents: created as needed, never followed (nothing in a
        // fresh staging dir can be a symlink, and none can be extracted).
        let mut cur = openat(
            dest,
            ".",
            OFlag::O_DIRECTORY | OFlag::O_RDONLY | OFlag::O_CLOEXEC,
            Mode::empty(),
        )
        .map_err(|e| HelperError::io("staging", e))?;
        for p in parents {
            let (fd, created) = ensure_dir(cur.as_fd(), p, modes.dir)?;
            if created {
                fchmod(fd.as_fd(), Mode::from_bits_truncate(modes.dir))
                    .map_err(|e| HelperError::io(p.to_string_lossy(), e))?;
            }
            cur = fd;
        }
        if is_dir {
            let (fd, _) = ensure_dir(cur.as_fd(), last, modes.dir)?;
            fchmod(fd.as_fd(), Mode::from_bits_truncate(modes.dir))
                .map_err(|e| HelperError::io(last.to_string_lossy(), e))?;
        } else {
            // O_EXCL: a duplicate name is refused, never overwritten.
            let mut f = create_new(cur.as_fd(), last)?;
            std::io::copy(&mut entry, &mut f)
                .map_err(|e| HelperError::Refused(format!("archive: {e}")))?;
            let m = if mode & 0o111 != 0 {
                modes.file_exec
            } else {
                modes.file_plain
            };
            fchmod(f.as_fd(), Mode::from_bits_truncate(m & 0o777))
                .map_err(|e| HelperError::io(last.to_string_lossy(), e))?;
        }
    }
    Ok(())
}

/// Open `bin/` of a site, no-follow all the way.
fn open_bin(ctx: &Ctx, short: &str) -> Result<std::os::fd::OwnedFd, HelperError> {
    let root = open_root(&ctx.root)?;
    let mut comps: Vec<&str> = SITES.to_vec();
    comps.push(short);
    comps.push("bin");
    walk_nofollow(root.as_fd(), &comps)
}

pub fn install(
    ctx: &Ctx,
    short: &str,
    input: InstallInput,
) -> Result<serde_json::Value, HelperError> {
    validate::version(&input.version)?;
    validate::sha256(&input.sha256)?;
    let root = open_root(&ctx.root)?;
    let downloads = walk_nofollow(root.as_fd(), &DOWNLOADS)?;
    let name = format!("{}.tar.gz", input.sha256);
    // O_NONBLOCK: a fifo planted here cannot hang the open; fstat then
    // refuses it like any non-regular file.
    let fd = openat(
        downloads.as_fd(),
        name.as_str(),
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK,
        Mode::empty(),
    )
    .map_err(|e| match e {
        nix::errno::Errno::ELOOP => HelperError::Refused("download is a symlink".to_string()),
        other => HelperError::io(format!("download {name}"), other),
    })?;
    let st = fstat(fd.as_fd()).map_err(|e| HelperError::io("download", e))?;
    if kind(&st) != Kind::File {
        return Err(HelperError::Refused(
            "download is not a regular file".into(),
        ));
    }
    let mut file = std::fs::File::from(fd);
    let mut hasher = Sha256::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| HelperError::io("download", e))?;
    if hex::encode(hasher.finalize()) != input.sha256 {
        return Err(HelperError::Refused(
            "download does not match sha256".into(),
        ));
    }
    file.rewind().map_err(|e| HelperError::io("download", e))?;

    let bin = open_bin(ctx, short)?;
    match kind_at(bin.as_fd(), OsStr::new(&input.version))? {
        Some(Kind::Dir) => return Ok(json!({ "version": input.version, "changed": false })),
        Some(_) => {
            return Err(HelperError::Refused(format!(
                "bin/{} exists and is not a directory",
                input.version
            )))
        }
        None => {}
    }
    let staging = tmp_name("install");
    let (stage_fd, _) = ensure_dir(bin.as_fd(), &staging, 0o700)?;
    let result = (|| {
        let gz = flate2::read::GzDecoder::new(&mut file);
        extract_checked(gz, stage_fd.as_fd(), None, INSTALL_MODES)?;
        let path = ctx.site_path(short, &["bin", &input.version]);
        ctx.own.chown(stage_fd.as_fd(), &path, 0, 0)?;
        ctx.own.chmod(stage_fd.as_fd(), &path, 0o755)?;
        renameat(
            bin.as_fd(),
            staging.as_os_str(),
            bin.as_fd(),
            input.version.as_str(),
        )
        .map_err(|e| HelperError::io(path.display(), e))
    })();
    if let Err(e) = result {
        let _ = remove_tree_at(bin.as_fd(), &staging);
        return Err(e);
    }
    Ok(json!({ "version": input.version, "changed": true }))
}

pub fn switch_version(
    ctx: &Ctx,
    short: &str,
    input: VersionInput,
) -> Result<serde_json::Value, HelperError> {
    validate::version(&input.version)?;
    let bin = open_bin(ctx, short)?;
    // The target must be an installed version dir (opened no-follow).
    open_dir_nofollow(bin.as_fd(), OsStr::new(&input.version))
        .map_err(|_| HelperError::Invalid(format!("version {} (not installed)", input.version)))?;
    let current = OsStr::new("current");
    match kind_at(bin.as_fd(), current)? {
        Some(Kind::Symlink) => {
            if readlinkat(bin.as_fd(), current).ok().as_deref() == Some(OsStr::new(&input.version))
            {
                return Ok(json!({ "version": input.version, "changed": false }));
            }
        }
        None => {}
        Some(_) => return Err(HelperError::Refused("bin/current is not a symlink".into())),
    }
    let tmp = tmp_name("current");
    symlinkat(input.version.as_str(), bin.as_fd(), tmp.as_os_str())
        .map_err(|e| HelperError::io("bin/current", e))?;
    // rename(2) over the old link: readers see the old or the new target.
    if let Err(e) = renameat(bin.as_fd(), tmp.as_os_str(), bin.as_fd(), current) {
        let _ = nix::unistd::unlinkat(
            bin.as_fd(),
            tmp.as_os_str(),
            nix::unistd::UnlinkatFlags::NoRemoveDir,
        );
        return Err(HelperError::io("bin/current", e));
    }
    Ok(json!({ "version": input.version, "changed": true }))
}
