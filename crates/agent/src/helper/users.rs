//! Per-site system users `setu-site-<short>` (spec §4: uid range
//! 30000–39999, no shell, home = the site dir).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::HelperError;

/// Lowest and highest uid/gid a site user may have.
pub const UID_MIN: u32 = 30000;
pub const UID_MAX: u32 = 39999;

/// Site user name for a (validated) short id.
pub fn user_name(short: &str) -> String {
    format!("setu-site-{short}")
}

/// Site user management. Both calls are idempotent.
pub trait Users: Send + Sync {
    /// Create the user when missing; return its `(uid, gid)`.
    fn ensure_site_user(&self, short: &str) -> Result<(u32, u32), HelperError>;
    /// Delete the user when present.
    fn remove(&self, short: &str) -> Result<(), HelperError>;
}

/// Production: `useradd --system` / `userdel`, lookups via NSS.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealUsers;

fn lookup(name: &str) -> Result<Option<(u32, u32)>, HelperError> {
    nix::unistd::User::from_name(name)
        .map(|u| u.map(|u| (u.uid.as_raw(), u.gid.as_raw())))
        .map_err(|e| HelperError::io(format!("lookup {name}"), e))
}

fn run(prog: &str, args: &[&str]) -> Result<(), HelperError> {
    let out = std::process::Command::new(prog)
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .map_err(|e| HelperError::Command(prog.to_string(), e.to_string()))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(HelperError::Command(
            prog.to_string(),
            err.lines().next().unwrap_or("non-zero exit").to_string(),
        ))
    }
}

impl Users for RealUsers {
    fn ensure_site_user(&self, short: &str) -> Result<(u32, u32), HelperError> {
        super::validate::site(short)?;
        let name = user_name(short);
        if lookup(&name)?.is_none() {
            let home = format!("/srv/setu/sites/{short}");
            let (umin, umax) = (
                format!("SYS_UID_MIN={UID_MIN}"),
                format!("SYS_UID_MAX={UID_MAX}"),
            );
            let (gmin, gmax) = (
                format!("SYS_GID_MIN={UID_MIN}"),
                format!("SYS_GID_MAX={UID_MAX}"),
            );
            run(
                "/usr/sbin/useradd",
                &[
                    "--system",
                    "--user-group",
                    "--no-create-home",
                    "--home-dir",
                    &home,
                    "--shell",
                    "/usr/sbin/nologin",
                    "-K",
                    &umin,
                    "-K",
                    &umax,
                    "-K",
                    &gmin,
                    "-K",
                    &gmax,
                    &name,
                ],
            )?;
        }
        let (uid, gid) = lookup(&name)?
            .ok_or_else(|| HelperError::Command("useradd".into(), format!("{name} missing")))?;
        // A same-named user outside the range was not made by us.
        if !(UID_MIN..=UID_MAX).contains(&uid) {
            return Err(HelperError::Refused(format!(
                "{name} exists with uid {uid} outside {UID_MIN}-{UID_MAX}"
            )));
        }
        Ok((uid, gid))
    }

    fn remove(&self, short: &str) -> Result<(), HelperError> {
        super::validate::site(short)?;
        let name = user_name(short);
        match lookup(&name)? {
            None => Ok(()),
            Some((uid, _)) if !(UID_MIN..=UID_MAX).contains(&uid) => Err(HelperError::Refused(
                format!("{name} has uid {uid} outside {UID_MIN}-{UID_MAX}"),
            )),
            Some(_) => run("/usr/sbin/userdel", &[&name]),
        }
    }
}

#[derive(Debug, Default)]
struct FakeInner {
    users: BTreeMap<String, (u32, u32)>,
    next: u32,
    calls: Vec<String>,
}

/// Test double: in-memory users with uids from 30000 up.
#[derive(Debug, Clone, Default)]
pub struct FakeUsers {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeUsers {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake users lock")
    }

    /// `(uid, gid)` of a site user, if it exists.
    pub fn get(&self, short: &str) -> Option<(u32, u32)> {
        self.lock().users.get(&user_name(short)).copied()
    }

    /// Call transcript (`ensure <name>`, `remove <name>`).
    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }
}

impl Users for FakeUsers {
    fn ensure_site_user(&self, short: &str) -> Result<(u32, u32), HelperError> {
        super::validate::site(short)?;
        let name = user_name(short);
        let mut inner = self.lock();
        inner.calls.push(format!("ensure {name}"));
        if let Some(ids) = inner.users.get(&name) {
            return Ok(*ids);
        }
        let id = UID_MIN + inner.next;
        inner.next += 1;
        inner.users.insert(name, (id, id));
        Ok((id, id))
    }

    fn remove(&self, short: &str) -> Result<(), HelperError> {
        super::validate::site(short)?;
        let name = user_name(short);
        let mut inner = self.lock();
        inner.calls.push(format!("remove {name}"));
        inner.users.remove(&name);
        Ok(())
    }
}
