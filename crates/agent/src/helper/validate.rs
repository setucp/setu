//! The helper re-validates everything it is given (it never trusts the
//! agent): the shared regexes from `setu_common::validate` plus the
//! helper-only checks (sha256, limits).

use super::HelperError;

/// `--site`: `^[a-z0-9]{8}$`.
pub fn site(short: &str) -> Result<(), HelperError> {
    if setu_common::ids::is_short_id(short) {
        Ok(())
    } else {
        Err(HelperError::Invalid("site id".to_string()))
    }
}

/// App version: `^[0-9A-Za-z.+-]{1,64}$`, never `.` or `..`.
pub fn version(v: &str) -> Result<(), HelperError> {
    if setu_common::validate::version(v) && v != "." && v != ".." {
        Ok(())
    } else {
        Err(HelperError::Invalid("version".to_string()))
    }
}

/// Executable path inside `bin/current`.
pub fn exec(e: &str) -> Result<(), HelperError> {
    if setu_common::validate::exec(e) {
        Ok(())
    } else {
        Err(HelperError::Invalid("exec".to_string()))
    }
}

/// File name of a raw `binary` download in `bin/<version>/`: an
/// [`exec`] of exactly one component.
pub fn exec_file(e: &str) -> Result<(), HelperError> {
    if setu_common::validate::exec_file(e) {
        Ok(())
    } else {
        Err(HelperError::Invalid(
            "exec (must be a single file name)".to_string(),
        ))
    }
}

/// One `ExecStart` argument: no control characters (newline, tab, …).
pub fn arg(a: &str) -> Result<(), HelperError> {
    if setu_common::validate::no_control(a) {
        Ok(())
    } else {
        Err(HelperError::Invalid("arg (control character)".to_string()))
    }
}

/// Data dir name: `^[a-z][a-z0-9-]{0,31}$`.
pub fn dir(d: &str) -> Result<(), HelperError> {
    if setu_common::validate::dir(d) {
        Ok(())
    } else {
        Err(HelperError::Invalid("dir name".to_string()))
    }
}

/// Every dir, and no duplicates.
pub fn dirs(ds: &[String]) -> Result<(), HelperError> {
    for (i, d) in ds.iter().enumerate() {
        dir(d)?;
        if ds[..i].contains(d) {
            return Err(HelperError::Invalid("dir name (duplicate)".to_string()));
        }
    }
    Ok(())
}

/// Env name `^[A-Z][A-Z0-9_]*$`; the value is never echoed.
pub fn env_name(n: &str) -> Result<(), HelperError> {
    if setu_common::validate::env_name(n) {
        Ok(())
    } else {
        Err(HelperError::Invalid("env name".to_string()))
    }
}

/// Env value: no control characters (a newline would inject a variable).
pub fn env_value(name: &str, v: &str) -> Result<(), HelperError> {
    if setu_common::validate::no_control(v) {
        Ok(())
    } else {
        Err(HelperError::Invalid(format!(
            "env value for {name} (control character)"
        )))
    }
}

/// sha256: exactly 64 lowercase hex chars (it names the download file).
pub fn sha256(s: &str) -> Result<(), HelperError> {
    if s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(())
    } else {
        Err(HelperError::Invalid("sha256".to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_and_shas() {
        version("0.2.0").expect("ok");
        version("1.0.0+build-1").expect("ok");
        for bad in ["", "../x", "a/b", "..", ".", "1 0", "v\n1"] {
            version(bad).expect_err(bad);
        }
        sha256(&"a".repeat(64)).expect("ok");
        sha256(&"A".repeat(64)).expect_err("upper");
        sha256(&"a".repeat(63)).expect_err("short");
        sha256(&format!("{}/", "a".repeat(63))).expect_err("slash");
    }

    #[test]
    fn dirs_reject_duplicates_and_bad_names() {
        dirs(&["media".into(), "uploads".into()]).expect("ok");
        dirs(&["media".into(), "media".into()]).expect_err("dup");
        dirs(&["../x".into()]).expect_err("dotdot");
        dirs(&["Media".into()]).expect_err("upper");
    }
}
