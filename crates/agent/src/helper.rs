//! `setu helper write-file`: the only privileged file writer.
//!
//! Runs as root through a sudoers entry pinned to the installed path with
//! fixed `--path` values. The path must stay under one of the allowed
//! roots; escapes — including `..` and symlinks pointing outside — are
//! rejected before any byte is written.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Errors from the privileged file writer.
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    #[error("refusing to write outside allowed roots: {0}")]
    OutsideRoots(String),
    #[error("refusing to follow symlink: {0}")]
    Symlink(String),
    #[error("invalid mode {0}: must be 3-4 octal digits")]
    BadMode(String),
    #[error("refusing {0}: not a setu site unit")]
    BadUnit(String),
    #[error("refusing {0}: not a setu site user")]
    BadUser(String),
    #[error("refusing systemctl verb {0}")]
    BadVerb(String),
    #[error("I/O for {0}: {1}")]
    Io(String, String),
    #[error("systemctl {0} failed: {1}")]
    Systemctl(String, String),
}

/// Production allowed roots: site dirs and the template-unit drop-in dirs.
pub fn production_roots() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/srv/setu/sites"),
        PathBuf::from("/etc/systemd/system"),
    ]
}

/// Roots used when `use_sudo_helper = false` (tests): the process temp
/// dir, so tempdir paths pass the allowlist.
pub fn test_roots() -> Vec<PathBuf> {
    vec![std::env::temp_dir()]
}

/// Check `path` against `roots`: it must resolve (through existing
/// ancestors, without following a final symlink) to a location under one
/// of the roots. Additionally, `/etc/systemd/system` writes are limited
/// to `setu-site@*` unit files and drop-ins.
pub fn check_path_with_roots(path: &Path, roots: &[PathBuf]) -> Result<(), HelperError> {
    // A final symlink is never followed: write to the link target would
    // escape the allowlist.
    if path
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(HelperError::Symlink(path.display().to_string()));
    }
    // Resolve through the nearest existing ancestor.
    let mut anchor = path;
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if anchor.exists() {
            break;
        }
        match anchor.parent() {
            Some(parent) if parent != anchor => {
                if let Some(name) = anchor.file_name() {
                    rest.push(name.to_os_string());
                }
                anchor = parent;
            }
            _ => break,
        }
    }
    let canonical = anchor
        .canonicalize()
        .map_err(|e| HelperError::Io(anchor.display().to_string(), e.to_string()))?;
    let mut resolved = canonical;
    for comp in rest.iter().rev() {
        resolved.push(comp);
    }
    let under_root = roots.iter().any(|r| resolved.starts_with(r));
    if !under_root {
        return Err(HelperError::OutsideRoots(path.display().to_string()));
    }
    // Inside /etc/systemd/system only setu site units may be written.
    if resolved.starts_with("/etc/systemd/system") && !systemd_name_allowed(&resolved) {
        return Err(HelperError::OutsideRoots(path.display().to_string()));
    }
    Ok(())
}

/// True when a resolved `/etc/systemd/system/...` path is a setu site
/// unit file or drop-in (pure function, unit-tested).
fn systemd_name_allowed(resolved: &Path) -> bool {
    let name = resolved
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let parent_name = resolved
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    let is_site_unit = name.starts_with("setu-site@") || name == "setu-site@.service";
    let is_site_dropin = parent_name.starts_with("setu-site@") && name == "override.conf";
    is_site_unit || is_site_dropin
}

/// Parse a `--mode` value (`0600`, `644`) into permission bits.
pub fn parse_mode(mode: &str) -> Result<u32, HelperError> {
    if mode.is_empty() || mode.len() > 5 {
        return Err(HelperError::BadMode(mode.to_string()));
    }
    let digits = mode.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    if digits.len() > 4 || !digits.chars().all(|c| ('0'..='7').contains(&c)) {
        return Err(HelperError::BadMode(mode.to_string()));
    }
    u32::from_str_radix(digits, 8).map_err(|_| HelperError::BadMode(mode.to_string()))
}

/// True when the file is missing or its bytes differ.
pub fn is_changed(path: &Path, contents: &[u8]) -> bool {
    match std::fs::read(path) {
        Ok(cur) => cur != contents,
        Err(_) => true,
    }
}

/// Write `contents` to `path` (temp file + rename), set `mode`, and
/// `chown` to `owner` when given. The allowlist check runs first.
pub fn write_file_with_roots(
    path: &Path,
    contents: &[u8],
    mode: &str,
    owner: Option<&str>,
    roots: &[PathBuf],
) -> Result<(), HelperError> {
    check_path_with_roots(path, roots)?;
    let bits = parse_mode(mode)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| HelperError::Io(parent.display().to_string(), e.to_string()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&tmp, contents)
        .map_err(|e| HelperError::Io(tmp.display().to_string(), e.to_string()))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(bits))
        .map_err(|e| HelperError::Io(tmp.display().to_string(), e.to_string()))?;
    if let Some(owner) = owner {
        let out = std::process::Command::new("chown")
            .arg("--")
            .arg(owner)
            .arg(&tmp)
            .output()
            .map_err(|e| HelperError::Io("chown".to_string(), e.to_string()))?;
        if !out.status.success() {
            let _ = std::fs::remove_file(&tmp);
            return Err(HelperError::Io(
                path.display().to_string(),
                format!("chown failed: {}", String::from_utf8_lossy(&out.stderr)),
            ));
        }
    }
    std::fs::rename(&tmp, path)
        .map_err(|e| HelperError::Io(path.display().to_string(), e.to_string()))?;
    Ok(())
}

/// Production entry: allowlist is the fixed production roots.
pub fn write_file(
    path: &Path,
    contents: &[u8],
    mode: &str,
    owner: Option<&str>,
) -> Result<(), HelperError> {
    write_file_with_roots(path, contents, mode, owner, &production_roots())
}

/// Validate a site unit name (`setu-site@<id>.service`, id charset only)
/// and return the site id. This is the gate every privileged unit
/// operation passes through.
pub fn check_site_unit(unit: &str) -> Result<String, HelperError> {
    let id = unit
        .strip_prefix("setu-site@")
        .and_then(|s| s.strip_suffix(".service"))
        .ok_or_else(|| HelperError::BadUnit(unit.to_string()))?;
    if id.is_empty() || id.len() > 64 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(HelperError::BadUnit(unit.to_string()));
    }
    Ok(id.to_string())
}

/// systemctl verbs the helper performs (fixed list, no shell).
const SYSTEMCTL_VERBS: &[&str] = &["start", "stop", "restart", "show", "daemon-reload"];

/// Run `/bin/systemctl` as root (the helper already is root): `show`
/// returns its stdout, other verbs return empty on success. `unit` is
/// required except for `daemon-reload`.
pub fn systemctl(verb: &str, unit: Option<&str>) -> Result<String, HelperError> {
    if !SYSTEMCTL_VERBS.contains(&verb) {
        return Err(HelperError::BadVerb(verb.to_string()));
    }
    let mut cmd = std::process::Command::new("/bin/systemctl");
    cmd.arg(verb);
    if verb == "show" {
        cmd.arg("-p").arg("ActiveState,SubState,MainPID");
    }
    match (verb, unit) {
        ("daemon-reload", None) => {}
        (_, Some(u)) => {
            check_site_unit(u)?;
            cmd.arg(u);
        }
        (_, None) => return Err(HelperError::BadUnit("<missing>".to_string())),
    }
    let out = cmd
        .output()
        .map_err(|e| HelperError::Io("systemctl".to_string(), e.to_string()))?;
    if !out.status.success() {
        return Err(HelperError::Systemctl(
            verb.to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Journal tail for one site unit (oldest first).
pub fn journal(unit: &str, lines: u32) -> Result<Vec<String>, HelperError> {
    check_site_unit(unit)?;
    let out = std::process::Command::new("/bin/journalctl")
        .arg("-u")
        .arg(unit)
        .arg("-n")
        .arg(lines.clamp(1, 5000).to_string())
        .arg("--output=short-iso")
        .arg("--no-pager")
        .output()
        .map_err(|e| HelperError::Io("journalctl".to_string(), e.to_string()))?;
    if !out.status.success() {
        return Err(HelperError::Systemctl(
            format!("journal {unit}"),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

/// Remove one site's drop-in dir (`<unit_dir>/setu-site@<id>.service.d`).
/// The id charset check bounds this to setu site drop-ins even when
/// `unit_dir` is overridden.
pub fn remove_dropin(unit_dir: &Path, unit: &str) -> Result<(), HelperError> {
    let id = check_site_unit(unit)?;
    let dir = unit_dir.join(format!("setu-site@{id}.service.d"));
    match std::fs::remove_dir_all(&dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(HelperError::Io(dir.display().to_string(), e.to_string())),
    }
}

/// Validate a site username (`setu-site-<id>`, id charset only) and
/// return the site id. This is the gate the user create/delete helpers
/// pass through: without it a privileged `useradd`/`userdel` would take
/// an arbitrary name. Total length caps at 32 — shadow `useradd` rejects
/// longer names, and refusing here beats its cryptic error (found by the
/// e2e with 36-char full-ULID users; jobs now sends the 8-char short id).
pub fn check_site_user(user: &str) -> Result<String, HelperError> {
    let id = user
        .strip_prefix("setu-site-")
        .ok_or_else(|| HelperError::BadUser(user.to_string()))?;
    if id.is_empty()
        || user.len() > 32
        || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(HelperError::BadUser(user.to_string()));
    }
    Ok(id.to_string())
}

/// Ensure the site's system user exists (idempotent): `id <user>` first,
/// `useradd --system` when missing. Returns true when the user was
/// created. The agent runs `WriteUnit` before `WriteEnv`, and the env
/// write chowns to this user, so the user must exist by then.
pub fn ensure_site_user(user: &str) -> Result<bool, HelperError> {
    check_site_user(user)?;
    let missing = std::process::Command::new("/usr/bin/id")
        .arg(user)
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true);
    if !missing {
        return Ok(false);
    }
    let out = std::process::Command::new("/usr/sbin/useradd")
        .arg("--system")
        .arg("--no-create-home")
        .arg("--shell")
        .arg("/usr/sbin/nologin")
        .arg(user)
        .output()
        .map_err(|e| HelperError::Io("useradd".to_string(), e.to_string()))?;
    if !out.status.success() {
        return Err(HelperError::Io(
            "useradd".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(true)
}

/// Delete the site's system user when present (idempotent): missing users
/// are not an error. Runs after the unit is stopped, so no processes
/// should remain; a busy user errors rather than forcing removal.
pub fn remove_site_user(user: &str) -> Result<bool, HelperError> {
    check_site_user(user)?;
    let missing = std::process::Command::new("/usr/bin/id")
        .arg(user)
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(false);
    if missing {
        return Ok(false);
    }
    let out = std::process::Command::new("/usr/sbin/userdel")
        .arg(user)
        .output()
        .map_err(|e| HelperError::Io("userdel".to_string(), e.to_string()))?;
    if !out.status.success() {
        return Err(HelperError::Io(
            "userdel".to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
        ));
    }
    Ok(true)
}

/// Privileged write used by the runtime in production: re-exec through
/// `sudo <exe> helper write-file`, piping contents on stdin. `owner`
/// chowns inside the helper (which runs as root).
pub async fn sudo_write_file(
    exe: &Path,
    path: &Path,
    contents: &[u8],
    mode: &str,
    owner: Option<&str>,
) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut cmd = tokio::process::Command::new("sudo");
    cmd.arg(exe)
        .arg("helper")
        .arg("write-file")
        .arg("--path")
        .arg(path)
        .arg("--mode")
        .arg(mode);
    if let Some(owner) = owner {
        cmd.arg("--owner").arg(owner);
    }
    let mut child = cmd
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("cannot run helper write-file: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(contents)
            .await
            .map_err(|e| anyhow::anyhow!("cannot pipe to helper: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .await
        .map_err(|e| anyhow::anyhow!("helper write-file failed: {e}"))?;
    if !out.status.success() {
        anyhow::bail!(
            "helper write-file {} failed: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_under_test_root_with_mode() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = vec![dir.path().to_path_buf()];
        let path = dir.path().join("sub").join("override.conf");
        write_file_with_roots(&path, b"data", "0600", None, &roots).expect("write");
        assert_eq!(std::fs::read(&path).expect("read"), b"data");
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn rejects_dotdot_escape() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).expect("mkdir");
        let roots = vec![root.clone()];
        let escape = root.join("..").join("outside.txt");
        write_file_with_roots(&escape, b"x", "0644", None, &roots).expect_err("must reject");
    }

    #[test]
    fn rejects_final_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let roots = vec![dir.path().to_path_buf()];
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"real").expect("write target");
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        write_file_with_roots(&link, b"x", "0644", None, &roots).expect_err("must reject symlink");
    }

    #[test]
    fn systemd_root_only_allows_site_units() {
        assert!(systemd_name_allowed(Path::new(
            "/etc/systemd/system/setu-site@abc.service"
        )));
        assert!(systemd_name_allowed(Path::new(
            "/etc/systemd/system/setu-site@abc.service.d/override.conf"
        )));
        assert!(systemd_name_allowed(Path::new(
            "/etc/systemd/system/setu-site@.service"
        )));
        assert!(!systemd_name_allowed(Path::new(
            "/etc/systemd/system/evil.service"
        )));
        assert!(!systemd_name_allowed(Path::new(
            "/etc/systemd/system/setu-site@abc.service.d/other.conf"
        )));
        assert!(!systemd_name_allowed(Path::new(
            "/etc/systemd/system/other.d/override.conf"
        )));
    }

    #[test]
    fn bad_modes_rejected() {
        for bad in ["", "abc", "0999", "12345", "-1"] {
            parse_mode(bad).expect_err("must reject");
        }
        assert_eq!(parse_mode("0600").expect("0600"), 0o600);
        assert_eq!(parse_mode("644").expect("644"), 0o644);
    }

    #[test]
    fn site_unit_gate() {
        assert_eq!(
            check_site_unit("setu-site@abc-123.service").expect("valid"),
            "abc-123"
        );
        for bad in [
            "evil.service",
            "setu-site@.service",
            "setu-site@a/b.service",
            "setu-site@a b.service",
            "setu-site@a;reboot.service",
            "setu-site@",
            "other@abc.service",
        ] {
            check_site_unit(bad).expect_err("must reject");
        }
        // Unknown verbs never reach exec.
        systemctl("reboot", Some("setu-site@abc.service")).expect_err("bad verb");
        systemctl("start", Some("evil.service")).expect_err("bad unit");
        systemctl("start", None).expect_err("missing unit");
    }

    #[test]
    fn site_user_gate() {
        assert_eq!(
            check_site_user("setu-site-00000001").expect("valid"),
            "00000001"
        );
        for bad in [
            "root",
            "setu",
            "setu-site-",
            "setu-site-a/b",
            "setu-site-a b",
            "setu-site-a;reboot",
            "setu-site-$(reboot)",
            "other-abc",
            "setu-site@abc.service",
            // 36 chars: shadow useradd rejects overlong names, so the
            // gate refuses first (jobs sends the 8-char short id).
            "setu-site-01jtestsitetest00000001",
        ] {
            check_site_user(bad).expect_err("must reject");
        }
    }

    #[test]
    fn remove_dropin_only_touches_site_dirs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let unit_dir = dir.path().join("units");
        let dropin = unit_dir.join("setu-site@abc.service.d");
        std::fs::create_dir_all(&dropin).expect("mkdir");
        std::fs::write(dropin.join("override.conf"), b"x").expect("write");
        // A decoy that must survive.
        std::fs::write(unit_dir.join("other.service"), b"keep").expect("write");
        remove_dropin(&unit_dir, "setu-site@abc.service").expect("remove");
        assert!(!dropin.exists());
        assert!(unit_dir.join("other.service").is_file());
        remove_dropin(&unit_dir, "evil.service").expect_err("must reject");
    }
}
