//! `write-env [{name, value}]`: `<site>/env`, root:root 0600, read only
//! by PID 1 (`EnvironmentFile=`). Values never appear in errors or logs.

use std::ffi::OsStr;
use std::os::fd::AsFd;

use serde_json::json;

use super::fs::{open_root, read_file_nofollow, walk_nofollow, write_atomic};
use super::{validate, Ctx, HelperError, SITES};

/// One variable. `Debug` redacts the value.
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EnvPair {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for EnvPair {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EnvPair")
            .field("name", &self.name)
            .field("value", &"[redacted]")
            .finish()
    }
}

/// `EnvironmentFile=` syntax: `NAME="value"` with `\`, `"`, `` ` `` and
/// `$` backslash-escaped (systemd's double-quote rules; no expansion).
pub fn render_env(vars: &[EnvPair]) -> Result<String, HelperError> {
    let mut out = String::new();
    for (i, v) in vars.iter().enumerate() {
        validate::env_name(&v.name)?;
        validate::env_value(&v.name, &v.value)?;
        if vars[..i].iter().any(|p| p.name == v.name) {
            return Err(HelperError::Invalid(format!(
                "env name {} (duplicate)",
                v.name
            )));
        }
        out.push_str(&v.name);
        out.push_str("=\"");
        for c in v.value.chars() {
            if matches!(c, '\\' | '"' | '`' | '$') {
                out.push('\\');
            }
            out.push(c);
        }
        out.push_str("\"\n");
    }
    Ok(out)
}

#[allow(clippy::needless_pass_by_value)] // reviewed: kept as-is (R150)
pub fn write_env(
    ctx: &Ctx,
    short: &str,
    vars: Vec<EnvPair>,
) -> Result<serde_json::Value, HelperError> {
    let text = render_env(&vars)?;
    let root = open_root(&ctx.root)?;
    let mut comps: Vec<&str> = SITES.to_vec();
    comps.push(short);
    // The site dir must exist (`site-create`); every step is no-follow.
    let site = walk_nofollow(root.as_fd(), &comps)?;
    let name = OsStr::new("env");
    let unchanged = read_file_nofollow(site.as_fd(), name)?.as_deref() == Some(text.as_bytes());
    if !unchanged {
        write_atomic(
            ctx.own.as_ref(),
            site.as_fd(),
            name,
            &ctx.site_path(short, &["env"]),
            text.as_bytes(),
            0,
            0,
            0o600,
        )?;
    }
    Ok(json!({ "vars": vars.len(), "changed": !unchanged }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(n: &str, v: &str) -> EnvPair {
        EnvPair {
            name: n.into(),
            value: v.into(),
        }
    }

    #[test]
    fn render_escapes_and_rejects_control_chars() {
        let text = render_env(&[pair("A", "plain"), pair("B", "q\"$x`\\")]).expect("render");
        assert_eq!(text, "A=\"plain\"\nB=\"q\\\"\\$x\\`\\\\\"\n");
        let err = render_env(&[pair("A", "line1\nB=evil")]).expect_err("newline");
        assert!(!err.to_string().contains("evil"), "value leaked: {err}");
        render_env(&[pair("a", "x")]).expect_err("lowercase name");
        render_env(&[pair("A", "x"), pair("A", "y")]).expect_err("duplicate");
        assert!(!format!("{:?}", pair("A", "s3cret")).contains("s3cret"));
    }
}
