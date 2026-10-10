//! `setu helper <verb> --site <short>`: the only code that runs as root
//! (spec §4, through `sudo -n /opt/setu/setu helper *`).
//!
//! Nothing path-like, no mode and no owner crosses the sudo boundary:
//! the caller names a verb and a short id; typed JSON arrives as the
//! first line of stdin (`restore` streams its archive after it) and the
//! result leaves on stdout (`backup` streams its archive instead). Every
//! path is derived from the short id under the fixed roots, every
//! component is opened with `openat(O_NOFOLLOW|O_DIRECTORY)`, files are
//! written temp-file-plus-rename inside root-owned parents, and no
//! special mode bit is ever set.
//!
//! Owner, user and systemd side effects go through traits
//! ([`Ownership`], [`Users`], [`Systemctl`]) so the tests run
//! unprivileged in a tempdir with recording fakes.

pub mod backup;
pub mod cli;
pub mod env;
pub mod fs;
pub mod install;
pub mod ownership;
pub mod remove;
pub mod site;
pub mod unit;
pub mod users;
pub mod validate;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub use ownership::{OwnOp, Ownership, RealOwnership, Recording};
pub use unit::{FakeSystemctl, RealSystemctl, Systemctl, UnitInput, Workdir};
pub use users::{FakeUsers, RealUsers, Users};

/// Sites root, relative to the helper root (`/` in production).
pub const SITES: [&str; 3] = ["srv", "setu", "sites"];
/// systemd unit dir, relative to the helper root.
pub const UNITS: [&str; 3] = ["etc", "systemd", "system"];
/// Agent download dir (the agent writes, the helper only reads).
pub const DOWNLOADS: [&str; 4] = ["var", "lib", "setu", "downloads"];

/// The helper verbs (spec §4 table), exactly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Verb {
    SiteCreate,
    WriteUnit,
    WriteEnv,
    Install,
    SwitchVersion,
    Unit,
    Backup,
    Restore,
    SiteRemove,
}

impl Verb {
    /// The CLI spelling (`site-create`, …).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SiteCreate => "site-create",
            Self::WriteUnit => "write-unit",
            Self::WriteEnv => "write-env",
            Self::Install => "install",
            Self::SwitchVersion => "switch-version",
            Self::Unit => "unit",
            Self::Backup => "backup",
            Self::Restore => "restore",
            Self::SiteRemove => "site-remove",
        }
    }
}

impl std::fmt::Display for Verb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Helper failures. Messages name inputs by kind, never env values.
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    /// Bad input: refused before anything is touched.
    #[error("invalid {0}")]
    Invalid(String),
    /// A safety rule refused the operation (symlink, hostile archive,
    /// active unit, …); nothing was left behind.
    #[error("refused: {0}")]
    Refused(String),
    /// I/O failure on a named object.
    #[error("{0}: {1}")]
    Io(String, String),
    /// An external program (`systemctl`, `useradd`, …) failed.
    #[error("{0} failed: {1}")]
    Command(String, String),
}

impl HelperError {
    pub(crate) fn io(what: impl std::fmt::Display, e: impl std::fmt::Display) -> Self {
        Self::Io(what.to_string(), e.to_string())
    }
}

/// Everything a verb may touch: the root prefix and the three
/// side-effect traits.
pub struct Ctx {
    /// `/` in production; a tempdir in tests (`--root`, feature
    /// `test-root`, or the library API).
    pub root: PathBuf,
    pub users: Box<dyn Users>,
    pub own: Box<dyn Ownership>,
    pub units: Box<dyn Systemctl>,
}

#[allow(clippy::missing_fields_in_debug)] // reviewed: kept as-is (R150)
impl std::fmt::Debug for Ctx {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ctx").field("root", &self.root).finish()
    }
}

impl Ctx {
    /// Production context: real users, owners and systemctl at `/`.
    pub fn production() -> Self {
        Self {
            root: PathBuf::from("/"),
            users: Box::new(RealUsers),
            own: Box::new(RealOwnership),
            units: Box::new(RealSystemctl),
        }
    }

    /// Unprivileged context rooted at `root` with fresh fakes. Site
    /// users get the caller's own ids: the files it writes then count as
    /// the site user's, as root's chown would have made them.
    pub fn fake(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            users: Box::new(FakeUsers::with_ids(
                nix::unistd::getuid().as_raw(),
                nix::unistd::getgid().as_raw(),
            )),
            own: Box::new(Recording::new()),
            units: Box::new(FakeSystemctl::new()),
        }
    }

    /// Absolute path of `comps` under the root (for records and errors).
    pub(crate) fn path(&self, comps: &[&str]) -> PathBuf {
        let mut p = self.root.clone();
        for c in comps {
            p.push(c);
        }
        p
    }

    /// Absolute path of the site dir (plus `rest`).
    pub(crate) fn site_path(&self, short: &str, rest: &[&str]) -> PathBuf {
        let mut comps: Vec<&str> = SITES.to_vec();
        comps.push(short);
        comps.extend_from_slice(rest);
        self.path(&comps)
    }
}

/// systemd unit name for a site.
pub fn unit_name(short: &str) -> String {
    format!("setu-site@{short}.service")
}

/// Run one verb. `input` is the parsed JSON line; `stdin` is whatever
/// follows it (only `restore` reads it); `stdout` receives `backup`'s
/// archive. Returns the JSON result (`Null` for `backup`).
pub fn run(
    ctx: &Ctx,
    verb: Verb,
    short: &str,
    input: serde_json::Value,
    stdin: &mut dyn Read,
    stdout: &mut dyn Write,
) -> Result<serde_json::Value, HelperError> {
    validate::site(short)?;
    match verb {
        Verb::SiteCreate => site::site_create(ctx, short, parse(input)?),
        Verb::WriteUnit => unit::write_unit(ctx, short, parse(input)?),
        Verb::WriteEnv => env::write_env(ctx, short, parse(input)?),
        Verb::Install => install::install(ctx, short, parse(input)?),
        Verb::SwitchVersion => install::switch_version(ctx, short, parse(input)?),
        Verb::Unit => unit::unit_op(ctx, short, parse(input)?),
        Verb::Backup => backup::backup(ctx, short, parse(input)?, stdout),
        Verb::Restore => backup::restore(ctx, short, parse(input)?, stdin),
        Verb::SiteRemove => remove::site_remove(ctx, short, parse(input)?),
    }
}

fn parse<T: serde::de::DeserializeOwned>(input: serde_json::Value) -> Result<T, HelperError> {
    // serde's message can quote a value (an env value on a type error):
    // keep only the position-free category.
    serde_json::from_value(input).map_err(|e| {
        let kind = match e.classify() {
            serde_json::error::Category::Data => "input shape",
            _ => "input json",
        };
        HelperError::Invalid(kind.to_string())
    })
}

/// `{ dirs: [name] }` for `site-create`, `backup`, `restore`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DirsInput {
    pub dirs: Vec<String>,
}

/// `{}` for `site-remove`.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyInput {}
