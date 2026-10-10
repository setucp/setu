//! `site-create {dirs}`: the site user and the tree (spec §4):
//! `<short>/` root 0755, `bin/` root 0755, `data/` and `run/` site user
//! 0750, `data/<dir>` site user 0750. `env` is written by `write-env`.

use std::ffi::OsStr;
use std::os::fd::AsFd;

use serde_json::json;

use super::fs::{ensure_dir, open_root, walk_create};
use super::{validate, Ctx, DirsInput, HelperError, SITES};

pub fn site_create(
    ctx: &Ctx,
    short: &str,
    input: DirsInput,
) -> Result<serde_json::Value, HelperError> {
    validate::dirs(&input.dirs)?;
    let (uid, gid) = ctx.users.ensure_site_user(short)?;

    let root = open_root(&ctx.root)?;
    let sites = walk_create(root.as_fd(), &SITES)?;
    let (site, _) = ensure_dir(sites.as_fd(), OsStr::new(short), 0o755)?;
    let site_path = ctx.site_path(short, &[]);
    ctx.own.chown(site.as_fd(), &site_path, 0, 0)?;
    ctx.own.chmod(site.as_fd(), &site_path, 0o755)?;

    let (bin, _) = ensure_dir(site.as_fd(), OsStr::new("bin"), 0o755)?;
    let bin_path = ctx.site_path(short, &["bin"]);
    ctx.own.chown(bin.as_fd(), &bin_path, 0, 0)?;
    ctx.own.chmod(bin.as_fd(), &bin_path, 0o755)?;

    let mut data_fd = None;
    for name in ["data", "run"] {
        let (fd, _) = ensure_dir(site.as_fd(), OsStr::new(name), 0o750)?;
        let path = ctx.site_path(short, &[name]);
        ctx.own.chown(fd.as_fd(), &path, uid, gid)?;
        ctx.own.chmod(fd.as_fd(), &path, 0o750)?;
        if name == "data" {
            data_fd = Some(fd);
        }
    }
    let data = data_fd.ok_or_else(|| HelperError::io("data", "missing"))?;
    for dir in &input.dirs {
        // `data/` belongs to the site user, who may have planted a
        // symlink here: `ensure_dir` opens no-follow and refuses it.
        let (fd, _) = ensure_dir(data.as_fd(), OsStr::new(dir), 0o750)?;
        let path = ctx.site_path(short, &["data", dir]);
        ctx.own.chown(fd.as_fd(), &path, uid, gid)?;
        ctx.own.chmod(fd.as_fd(), &path, 0o750)?;
    }
    Ok(json!({ "user": super::users::user_name(short), "uid": uid, "gid": gid }))
}
