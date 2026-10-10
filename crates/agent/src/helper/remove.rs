//! `site-remove {}`: stop, remove the drop-in, `userdel`, `rm -rf` the
//! site tree. Idempotent: every missing piece is fine.

use std::ffi::OsStr;
use std::os::fd::AsFd;

use serde_json::json;

use super::fs::{open_root, remove_tree_at, walk_nofollow};
use super::unit::UnitOp;
use super::{unit_name, Ctx, EmptyInput, HelperError, SITES, UNITS};

pub fn site_remove(
    ctx: &Ctx,
    short: &str,
    _input: EmptyInput,
) -> Result<serde_json::Value, HelperError> {
    let unit = unit_name(short);
    // A unit that never started (or is gone) cannot be stopped: fine.
    let _ = ctx.units.op(UnitOp::Stop, &unit);
    let root = open_root(&ctx.root)?;
    match walk_nofollow(root.as_fd(), &UNITS) {
        Ok(units) => {
            remove_tree_at(units.as_fd(), OsStr::new(&format!("{unit}.d")))?;
            ctx.units.daemon_reload()?;
        }
        Err(HelperError::Io(..)) => {} // no unit dir (test root)
        Err(e) => return Err(e),
    }
    ctx.users.remove(short)?;
    match walk_nofollow(root.as_fd(), &SITES) {
        Ok(sites) => remove_tree_at(sites.as_fd(), OsStr::new(short))?,
        Err(HelperError::Io(..)) => {}
        Err(e) => return Err(e),
    }
    Ok(json!({ "removed": true }))
}
