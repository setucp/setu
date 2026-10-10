//! Owner and mode changes, on open fds (no path re-resolution).
//!
//! `RealOwnership` calls `fchown`/`fchmod`; `Recording` records every
//! request and applies only the mode (an unprivileged test cannot
//! chown), so the tests assert the intended owners from the record and
//! the modes from disk. Modes are masked to `0o777`: the helper never
//! sets setuid, setgid or sticky bits.

use std::os::fd::BorrowedFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use nix::sys::stat::{fchmod, Mode};
use nix::unistd::{fchown, Gid, Uid};

use super::HelperError;

/// Owner/mode side effects. `path` names the object for records and
/// errors only; the change is applied through `fd`.
pub trait Ownership: Send + Sync {
    fn chown(&self, fd: BorrowedFd<'_>, path: &Path, uid: u32, gid: u32)
        -> Result<(), HelperError>;
    fn chmod(&self, fd: BorrowedFd<'_>, path: &Path, mode: u32) -> Result<(), HelperError>;
}

fn apply_mode(fd: BorrowedFd<'_>, path: &Path, mode: u32) -> Result<(), HelperError> {
    fchmod(fd, Mode::from_bits_truncate(mode & 0o777))
        .map_err(|e| HelperError::io(path.display(), e))
}

/// Production: `fchown` + `fchmod` as root.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealOwnership;

impl Ownership for RealOwnership {
    fn chown(
        &self,
        fd: BorrowedFd<'_>,
        path: &Path,
        uid: u32,
        gid: u32,
    ) -> Result<(), HelperError> {
        fchown(fd, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
            .map_err(|e| HelperError::io(path.display(), e))
    }

    fn chmod(&self, fd: BorrowedFd<'_>, path: &Path, mode: u32) -> Result<(), HelperError> {
        apply_mode(fd, path, mode)
    }
}

/// One recorded request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnOp {
    Chown { path: PathBuf, uid: u32, gid: u32 },
    Chmod { path: PathBuf, mode: u32 },
}

/// Test double: records chown/chmod, applies the (masked) mode only.
#[derive(Debug, Clone, Default)]
pub struct Recording {
    ops: Arc<Mutex<Vec<OwnOp>>>,
}

impl Recording {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every request so far, in order.
    pub fn ops(&self) -> Vec<OwnOp> {
        self.ops.lock().expect("recording lock").clone()
    }

    /// Last `(uid, gid)` requested for `path`.
    pub fn owner_of(&self, path: &Path) -> Option<(u32, u32)> {
        self.ops().iter().rev().find_map(|op| match op {
            OwnOp::Chown { path: p, uid, gid } if p == path => Some((*uid, *gid)),
            _ => None,
        })
    }

    /// Last mode requested for `path`.
    pub fn mode_of(&self, path: &Path) -> Option<u32> {
        self.ops().iter().rev().find_map(|op| match op {
            OwnOp::Chmod { path: p, mode } if p == path => Some(*mode),
            _ => None,
        })
    }

    /// True when anything was recorded for `path`.
    pub fn touched(&self, path: &Path) -> bool {
        self.ops().iter().any(|op| match op {
            OwnOp::Chown { path: p, .. } | OwnOp::Chmod { path: p, .. } => p == path,
        })
    }
}

impl Ownership for Recording {
    fn chown(
        &self,
        _fd: BorrowedFd<'_>,
        path: &Path,
        uid: u32,
        gid: u32,
    ) -> Result<(), HelperError> {
        self.ops.lock().expect("recording lock").push(OwnOp::Chown {
            path: path.to_path_buf(),
            uid,
            gid,
        });
        Ok(())
    }

    fn chmod(&self, fd: BorrowedFd<'_>, path: &Path, mode: u32) -> Result<(), HelperError> {
        self.ops.lock().expect("recording lock").push(OwnOp::Chmod {
            path: path.to_path_buf(),
            mode,
        });
        apply_mode(fd, path, mode)
    }
}
