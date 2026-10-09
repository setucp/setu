//! `write-unit` (the per-site drop-in) and `unit {op}` (systemctl).
//!
//! The drop-in is rendered from the fixed template
//! `assets/dropin.conf.tmpl`; every value is validated and the args are
//! systemd-quoted (`%`→`%%`, `$`→`$$`, `\` and `"` escaped, quoted when
//! not plain). Control characters are refused, so no value can start a
//! new directive line.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::os::fd::AsFd;
use std::sync::{Arc, Mutex};

use serde_json::json;

use super::fs::{ensure_dir, open_root, read_file_nofollow, walk_create, write_atomic};
use super::{unit_name, validate, Ctx, HelperError, UNITS};

/// The fixed drop-in template (golden asset).
pub const DROPIN_TEMPLATE: &str = include_str!("../../assets/dropin.conf.tmpl");

/// Drop-in file name inside `setu-site@<short>.service.d/`.
pub const DROPIN_NAME: &str = "10-setu.conf";

/// `WorkingDirectory=` choice: proto's mirror of the manifest's
/// `run.workdir` (`exec_dir` | `bin` | `data`), one type end to end.
pub use setu_proto::Workdir;

/// `write-unit` input: helper-local, built by the agent from proto's
/// `SiteSpec` (nothing path-like crosses sudo).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitInput {
    pub short_id: String,
    pub version: String,
    pub exec: String,
    pub args: Vec<String>,
    pub workdir: Workdir,
    pub memory_mb: u32,
    pub cpu_percent: u32,
}

/// `unit` ops.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnitOp {
    Start,
    Stop,
    Restart,
    Show,
}

impl UnitOp {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
            Self::Show => "show",
        }
    }
}

/// `unit` input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnitOpInput {
    pub op: UnitOp,
}

/// Properties `unit show` returns (spec §4), plus `data_bytes`.
pub const SHOW_PROPS: [&str; 7] = [
    "ActiveState",
    "SubState",
    "MainPID",
    "ControlGroup",
    "MemoryCurrent",
    "CPUUsageNSec",
    "NRestarts",
];

/// One `ExecStart` argument, systemd-quoted.
pub fn quote_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./:=,+@".contains(&b));
    let mut out = String::with_capacity(arg.len() + 2);
    for c in arg.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '%' => out.push_str("%%"),
            '$' => out.push_str("$$"),
            c => out.push(c),
        }
    }
    if plain {
        out
    } else {
        format!("\"{out}\"")
    }
}

/// Render the drop-in for a validated input.
pub fn render_dropin(input: &UnitInput) -> Result<String, HelperError> {
    validate::site(&input.short_id)?;
    validate::version(&input.version)?;
    validate::exec(&input.exec)?;
    for a in &input.args {
        validate::arg(a)?;
    }
    if input.memory_mb == 0 || input.cpu_percent == 0 {
        return Err(HelperError::Invalid("limits".to_string()));
    }
    let short = &input.short_id;
    let current = format!("/srv/setu/sites/{short}/bin/current");
    let workdir = match input.workdir {
        Workdir::ExecDir => match input.exec.rsplit_once('/') {
            Some((dir, _)) => format!("{current}/{dir}"),
            None => current.clone(),
        },
        Workdir::Bin => current.clone(),
        Workdir::Data => format!("/srv/setu/sites/{short}/data"),
    };
    let args: Vec<String> = input.args.iter().map(|a| quote_arg(a)).collect();
    let values: BTreeMap<&str, String> = BTreeMap::from([
        ("short", short.clone()),
        ("exec", input.exec.clone()),
        ("args", args.join(" ")),
        ("workdir", workdir),
        ("memory_mb", input.memory_mb.to_string()),
        ("cpu_percent", input.cpu_percent.to_string()),
    ]);
    // Single pass: a value is never re-scanned for placeholders.
    let mut out = String::with_capacity(DROPIN_TEMPLATE.len() + 128);
    let mut rest = DROPIN_TEMPLATE;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find('}')
            .ok_or_else(|| HelperError::Invalid("template".to_string()))?;
        let key = &after[..end];
        let value = values
            .get(key)
            .ok_or_else(|| HelperError::Invalid(format!("template key {key}")))?;
        out.push_str(value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    // No args leaves a trailing space on `ExecStart=`.
    let mut text: String = out
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    Ok(text)
}

pub fn write_unit(
    ctx: &Ctx,
    short: &str,
    input: UnitInput,
) -> Result<serde_json::Value, HelperError> {
    if input.short_id != short {
        return Err(HelperError::Invalid(
            "short_id (does not match --site)".to_string(),
        ));
    }
    let text = render_dropin(&input)?;
    let root = open_root(&ctx.root)?;
    let units = walk_create(root.as_fd(), &UNITS)?;
    let dir_name = format!("{}.d", unit_name(short));
    let (dir, _) = ensure_dir(units.as_fd(), OsStr::new(&dir_name), 0o755)?;
    let mut comps: Vec<&str> = UNITS.to_vec();
    comps.push(&dir_name);
    let dir_path = ctx.path(&comps);
    ctx.own.chown(dir.as_fd(), &dir_path, 0, 0)?;
    ctx.own.chmod(dir.as_fd(), &dir_path, 0o755)?;
    let name = OsStr::new(DROPIN_NAME);
    if read_file_nofollow(dir.as_fd(), name)?.as_deref() == Some(text.as_bytes()) {
        return Ok(json!({ "changed": false }));
    }
    write_atomic(
        ctx.own.as_ref(),
        dir.as_fd(),
        name,
        &dir_path.join(DROPIN_NAME),
        text.as_bytes(),
        0,
        0,
        0o644,
    )?;
    ctx.units.daemon_reload()?;
    Ok(json!({ "changed": true }))
}

pub fn unit_op(
    ctx: &Ctx,
    short: &str,
    input: UnitOpInput,
) -> Result<serde_json::Value, HelperError> {
    let unit = unit_name(short);
    match input.op {
        UnitOp::Show => {
            let props = ctx.units.show(&unit)?;
            let mut out = serde_json::Map::new();
            for key in SHOW_PROPS {
                let v = props.get(key).cloned().unwrap_or_default();
                out.insert(key.to_string(), serde_json::Value::String(v));
            }
            out.insert("data_bytes".to_string(), json!(data_bytes(ctx, short)));
            Ok(serde_json::Value::Object(out))
        }
        op => {
            ctx.units.op(op, &unit)?;
            Ok(json!({}))
        }
    }
}

/// `du -sb data/` as root (0 when the dir is missing or `du` fails).
fn data_bytes(ctx: &Ctx, short: &str) -> u64 {
    let path = ctx.site_path(short, &["data"]);
    if !path.is_dir() {
        return 0;
    }
    std::process::Command::new("/usr/bin/du")
        .arg("-sb")
        .arg("--")
        .arg(&path)
        .env_clear()
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .split_whitespace()
                .next()
                .and_then(|n| n.parse().ok())
        })
        .unwrap_or(0)
}

/// systemd side effects of the helper.
pub trait Systemctl: Send + Sync {
    /// `start|stop|restart` (never `show`).
    fn op(&self, op: UnitOp, unit: &str) -> Result<(), HelperError>;
    /// `show -p <SHOW_PROPS>` as a map.
    fn show(&self, unit: &str) -> Result<BTreeMap<String, String>, HelperError>;
    fn daemon_reload(&self) -> Result<(), HelperError>;
    /// Anything but `inactive`/`failed` counts as active.
    fn is_active(&self, unit: &str) -> Result<bool, HelperError> {
        let props = self.show(unit)?;
        let state = props.get("ActiveState").map(String::as_str).unwrap_or("");
        Ok(!matches!(state, "inactive" | "failed" | ""))
    }
}

/// Parse `KEY=VALUE` lines.
pub fn parse_show(text: &str) -> BTreeMap<String, String> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

/// Production: `/usr/bin/systemctl`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealSystemctl;

fn systemctl(args: &[&str]) -> Result<String, HelperError> {
    let out = std::process::Command::new("/usr/bin/systemctl")
        .args(args)
        .env_clear()
        .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
        .output()
        .map_err(|e| HelperError::Command("systemctl".into(), e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(HelperError::Command(
            format!("systemctl {}", args.first().copied().unwrap_or("")),
            err.lines().next().unwrap_or("non-zero exit").to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

impl Systemctl for RealSystemctl {
    fn op(&self, op: UnitOp, unit: &str) -> Result<(), HelperError> {
        if op == UnitOp::Show {
            return Err(HelperError::Invalid("op".to_string()));
        }
        systemctl(&[op.as_str(), unit]).map(|_| ())
    }

    fn show(&self, unit: &str) -> Result<BTreeMap<String, String>, HelperError> {
        let props = format!("--property={}", SHOW_PROPS.join(","));
        systemctl(&["show", &props, unit]).map(|t| parse_show(&t))
    }

    fn daemon_reload(&self) -> Result<(), HelperError> {
        systemctl(&["daemon-reload"]).map(|_| ())
    }
}

#[derive(Debug, Default)]
struct FakeInner {
    active: std::collections::BTreeSet<String>,
    calls: Vec<String>,
    reloads: usize,
}

/// Test double: tracks active units, counts reloads.
#[derive(Debug, Clone, Default)]
pub struct FakeSystemctl {
    inner: Arc<Mutex<FakeInner>>,
}

impl FakeSystemctl {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FakeInner> {
        self.inner.lock().expect("fake systemctl lock")
    }

    pub fn set_active(&self, unit: &str, active: bool) {
        let mut inner = self.lock();
        if active {
            inner.active.insert(unit.to_string());
        } else {
            inner.active.remove(unit);
        }
    }

    pub fn calls(&self) -> Vec<String> {
        self.lock().calls.clone()
    }

    pub fn reloads(&self) -> usize {
        self.lock().reloads
    }
}

impl Systemctl for FakeSystemctl {
    fn op(&self, op: UnitOp, unit: &str) -> Result<(), HelperError> {
        let mut inner = self.lock();
        inner.calls.push(format!("{} {unit}", op.as_str()));
        match op {
            UnitOp::Start | UnitOp::Restart => {
                inner.active.insert(unit.to_string());
            }
            UnitOp::Stop => {
                inner.active.remove(unit);
            }
            UnitOp::Show => return Err(HelperError::Invalid("op".to_string())),
        }
        Ok(())
    }

    fn show(&self, unit: &str) -> Result<BTreeMap<String, String>, HelperError> {
        let active = self.lock().active.contains(unit);
        let mut m = BTreeMap::new();
        let (state, sub, pid) = if active {
            ("active", "running", "4242")
        } else {
            ("inactive", "dead", "0")
        };
        m.insert("ActiveState".into(), state.into());
        m.insert("SubState".into(), sub.into());
        m.insert("MainPID".into(), pid.into());
        m.insert(
            "ControlGroup".into(),
            format!("/system.slice/system-setu\\x2dsite.slice/{unit}"),
        );
        m.insert("MemoryCurrent".into(), "1048576".into());
        m.insert("CPUUsageNSec".into(), "2000000000".into());
        m.insert("NRestarts".into(), "0".into());
        Ok(m)
    }

    fn daemon_reload(&self) -> Result<(), HelperError> {
        let mut inner = self.lock();
        inner.reloads += 1;
        inner.calls.push("daemon-reload".to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting_escapes_specifiers_and_quotes_when_needed() {
        assert_eq!(quote_arg("serve"), "serve");
        assert_eq!(quote_arg("127.0.0.1:20001"), "127.0.0.1:20001");
        assert_eq!(quote_arg("100%"), "\"100%%\"");
        assert_eq!(quote_arg("$HOME"), "\"$$HOME\"");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg("say \"hi\"\\"), "\"say \\\"hi\\\"\\\\\"");
        assert_eq!(quote_arg(""), "\"\"");
        assert_eq!(quote_arg(";"), "\";\"");
    }

    #[test]
    fn parse_show_reads_key_values() {
        let m = parse_show("ActiveState=active\nControlGroup=/system.slice/x\n");
        assert_eq!(m["ActiveState"], "active");
        assert_eq!(m["ControlGroup"], "/system.slice/x");
    }
}
