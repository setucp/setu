//! Semantic validation: collects ALL errors.
use std::collections::BTreeMap;

use setu_common::validate;

use crate::schema::{BinaryFormat, EnvSource, EnvSpec, Manifest, ManifestError, RunKind};

/// Parse a `[section]` header from a trimmed line.
fn parse_section_header(trimmed: &str) -> Option<String> {
    if !trimmed.starts_with('[') {
        return None;
    }
    let end = trimmed.find(']')?;
    Some(trimmed[1..end].trim().to_string())
}

/// Does this source line define TOML key `key` (`^KEY\s*=`, bare or quoted)?
fn key_matches_line(line: &str, key: &str) -> bool {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with('#') {
        return false;
    }
    if let Some(rest) = t.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            if rest[..end] != *key {
                return false;
            }
            return rest[end + 1..].trim_start().starts_with('=');
        }
        return false;
    }
    if let Some(rest) = t.strip_prefix('\'') {
        if let Some(end) = rest.find('\'') {
            if rest[..end] != *key {
                return false;
            }
            return rest[end + 1..].trim_start().starts_with('=');
        }
        return false;
    }
    let mut len = 0;
    for c in t.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    if len == 0 || &t[..len] != key {
        return false;
    }
    t[len..].trim_start().starts_with('=')
}

/// Exact header line for `[header]`.
pub fn line_of_section(src: &str, header: &str) -> Option<usize> {
    src.lines().enumerate().find_map(|(i, line)| {
        let t = line.trim();
        match parse_section_header(t) {
            Some(h) if h == header => Some(i + 1),
            _ => None,
        }
    })
}

/// Exact `^KEY\s*=` line within `[section]` (`""` for top-level keys).
pub fn line_of_key(src: &str, section: &str, key: &str) -> Option<usize> {
    let mut current = String::new();
    for (i, line) in src.lines().enumerate() {
        let t = line.trim();
        if let Some(h) = parse_section_header(t) {
            current = h;
            continue;
        }
        if current == section && key_matches_line(line, key) {
            return Some(i + 1);
        }
    }
    None
}

/// Backwards-compatible wrapper, now exact (no substring matching):
/// `[...]` searches headers, anything else searches `^KEY\s*=` across the
/// whole file. Prefer [`line_of_key`] / [`line_of_section`] for new code.
pub fn line_of(src: &str, needle: &str) -> Option<usize> {
    let n = needle.trim();
    if n.starts_with('[') {
        let inner = n.trim_start_matches('[');
        let inner = inner.split(']').next().unwrap_or(inner).trim();
        if let Some(l) = line_of_section(src, inner) {
            return Some(l);
        }
        for (i, line) in src.lines().enumerate() {
            let t = line.trim();
            if let Some(h) = parse_section_header(t) {
                if h == inner || h.starts_with(inner) {
                    return Some(i + 1);
                }
            }
        }
        return None;
    }
    let key = n.trim_end_matches(['=', ' ', '\t']).trim();
    if key.is_empty() {
        return None;
    }
    src.lines().enumerate().find_map(|(i, line)| {
        if key_matches_line(line, key) {
            Some(i + 1)
        } else {
            None
        }
    })
}

/// `app.id`: `^[a-z][a-z0-9-]{1,31}$` (spec §2).
fn is_valid_app_id(id: &str) -> bool {
    let mut chars = id.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase())
        && (2..=32).contains(&id.len())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// First line of a `[run.binary.<target>]` table or inline target entry,
/// and the line of `field` inside it when the target is a table.
fn target_lines(src: &str, target: &str, field: &str) -> Option<usize> {
    for header in [
        format!("run.binary.\"{target}\""),
        format!("run.binary.'{target}'"),
        format!("run.binary.{target}"),
    ] {
        if let Some(l) = line_of_key(src, &header, field) {
            return Some(l);
        }
        if let Some(l) = line_of_section(src, &header) {
            return Some(l);
        }
    }
    line_of_key(src, "run.binary", target)
}

/// First `[run.binary…]` header, else `[run]`.
fn run_binary_line(src: &str) -> Option<usize> {
    src.lines()
        .position(|l| {
            parse_section_header(l.trim())
                .is_some_and(|h| h == "run.binary" || h.starts_with("run.binary."))
        })
        .map(|i| i + 1)
        .or_else(|| line_of_section(src, "run"))
}

const DIR_RULE: &str = "must match [a-z][a-z0-9-]{0,31}";

/// Run the semantic rules over an already-parsed manifest, collecting
/// every failure. `src` is the original TOML source, used only to
/// attach line numbers.
#[allow(clippy::too_many_lines)] // reviewed: kept as-is (R150)
pub fn validate(manifest: &Manifest, src: &str) -> Vec<ManifestError> {
    let mut errors = Vec::new();
    let mut push = |path: String, line: Option<usize>, message: String| {
        errors.push(ManifestError::new(path, line, message));
    };

    if manifest.schema != 1 {
        push(
            "schema".into(),
            line_of_key(src, "", "schema"),
            "schema must be 1".into(),
        );
    }

    if !is_valid_app_id(&manifest.app.id) {
        push(
            "app.id".into(),
            line_of_key(src, "app", "id"),
            "app.id must match [a-z][a-z0-9-]{1,31}".into(),
        );
    }

    if !manifest.app.health.starts_with('/') || !validate::no_control(&manifest.app.health) {
        push(
            "app.health".into(),
            line_of_key(src, "app", "health"),
            "app.health must start with /".into(),
        );
    }

    if !validate::version(&manifest.app.version)
        || semver::Version::parse(&manifest.app.version).is_err()
    {
        push(
            "app.version".into(),
            line_of_key(src, "app", "version"),
            "app.version must be valid semver".into(),
        );
    }

    if !(1..=3600).contains(&manifest.app.startup_grace_s) {
        push(
            "app.startup_grace_s".into(),
            line_of_key(src, "app", "startup_grace_s"),
            "app.startup_grace_s must be between 1 and 3600".into(),
        );
    }

    let run = &manifest.run;
    if run
        .args
        .iter()
        .any(|a| a.is_empty() || !validate::no_control(a))
    {
        push(
            "run.args".into(),
            line_of_key(src, "run", "args"),
            "run.args entries must be non-empty and contain no control characters".into(),
        );
    }
    match run.kind {
        RunKind::Binary => {
            let targets = run.binary.as_ref().map(|b| &b.targets);
            // Any architecture will do: an ARM-only app is valid, and the
            // control plane refuses to place a site on a server whose
            // architecture the manifest does not cover (0.1.2).
            if targets.is_none_or(BTreeMap::is_empty) {
                push(
                    "run.binary".into(),
                    run_binary_line(src),
                    "run.binary must declare at least one target".into(),
                );
            }
            if run.image.is_some() {
                push(
                    "run.image".into(),
                    line_of_section(src, "run.image"),
                    "run.image is only allowed with kind = \"image\"".into(),
                );
            }
            for (target, entry) in targets.into_iter().flatten() {
                let t = target.to_string();
                if entry.url.is_empty() || !validate::no_control(&entry.url) {
                    push(
                        format!("run.binary.{t}.url"),
                        target_lines(src, &t, "url"),
                        format!("run.binary.{t}.url must not be empty"),
                    );
                }
                if !validate::exec(&entry.exec) {
                    push(
                        format!("run.binary.{t}.exec"),
                        target_lines(src, &t, "exec"),
                        format!(
                            "run.binary.{t}.exec must be a relative path of [A-Za-z0-9._-] segments without \"..\""
                        ),
                    );
                }
                if entry.format == BinaryFormat::Binary
                    && validate::exec(&entry.exec)
                    && !validate::exec_file(&entry.exec)
                {
                    push(
                        format!("run.binary.{t}.exec"),
                        target_lines(src, &t, "exec"),
                        format!(
                            "run.binary.{t}.exec must be a single file name when format = \"binary\""
                        ),
                    );
                }
                if !is_hex64(&entry.sha256) {
                    push(
                        format!("run.binary.{t}.sha256"),
                        target_lines(src, &t, "sha256"),
                        format!("run.binary.{t}.sha256 must be 64 hex chars"),
                    );
                }
            }
        }
        RunKind::Image => {
            // Phase 1 parses `kind = "image"`; the control plane rejects it
            // at provisioning time.
            match &run.image {
                Some(image) if !image.image.is_empty() => {}
                _ => push(
                    "run.image.image".into(),
                    line_of_key(src, "run.image", "image").or_else(|| line_of_section(src, "run")),
                    "run.image.image must not be empty".into(),
                ),
            }
            if run.binary.is_some() {
                push(
                    "run.binary".into(),
                    run_binary_line(src),
                    "run.binary is only allowed with kind = \"binary\"".into(),
                );
            }
        }
    }

    for dir in &manifest.needs.dirs {
        if !validate::dir(dir) {
            push(
                "needs.dirs".into(),
                line_of_key(src, "needs", "dirs"),
                format!("needs.dirs entry \"{dir}\" {DIR_RULE}"),
            );
        }
    }

    for dir in &manifest.backup.dirs {
        if !validate::dir(dir) {
            push(
                "backup.dirs".into(),
                line_of_key(src, "backup", "dirs"),
                format!("backup.dirs entry \"{dir}\" {DIR_RULE}"),
            );
        } else if !manifest.needs.dirs.iter().any(|d| d == dir) {
            push(
                "backup.dirs".into(),
                line_of_key(src, "backup", "dirs"),
                format!("backup.dirs entry \"{dir}\" not in needs.dirs"),
            );
        }
    }

    for (name, spec) in &manifest.env {
        let path = format!("env.{name}");
        let line = line_of_key(src, "env", name);
        if !validate::env_name(name) {
            push(
                path.clone(),
                line,
                format!("env.{name} must match [A-Z][A-Z0-9_]*"),
            );
        }
        match spec {
            EnvSpec::From(from) => {
                if from.from == EnvSource::Dir {
                    match &from.dir {
                        Some(dir) if !validate::dir(dir) => {
                            push(path, line, format!("env.{name} dir \"{dir}\" {DIR_RULE}"));
                        }
                        Some(dir) if manifest.needs.dirs.iter().any(|d| d == dir) => {}
                        Some(dir) => push(
                            path,
                            line,
                            format!("env.{name} references dir \"{dir}\" not in needs.dirs"),
                        ),
                        None => push(
                            path,
                            line,
                            format!("env.{name} from \"dir\" requires dir = \"...\""),
                        ),
                    }
                } else if from.dir.is_some() {
                    push(
                        path,
                        line,
                        format!(
                            "env.{name} from \"{}\" must not set dir",
                            from.from.as_str()
                        ),
                    );
                }
            }
            EnvSpec::Generate(gen) => {
                if gen.once && !gen.secret {
                    push(
                        path,
                        line,
                        format!("env.{name} once requires secret = true"),
                    );
                }
            }
            EnvSpec::Prompt(prompt) => {
                if prompt.once && !prompt.secret {
                    push(
                        path,
                        line,
                        format!("env.{name} once requires secret = true"),
                    );
                }
            }
            EnvSpec::Value(v) => {
                if !validate::no_control(&v.value) {
                    push(
                        path,
                        line,
                        format!("env.{name} value must not contain control characters"),
                    );
                }
            }
        }
    }

    errors
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_id_allows_lowercase_digits_dash() {
        assert!(is_valid_app_id("vyasa"));
        assert!(is_valid_app_id("my-app-2"));
        assert!(!is_valid_app_id("a"));
        assert!(!is_valid_app_id("2app"));
        assert!(!is_valid_app_id(&"a".repeat(33)));
        assert!(!is_valid_app_id("Hello"));
        assert!(!is_valid_app_id("hello_app"));
        assert!(!is_valid_app_id(""));
    }

    #[test]
    fn hex_check_requires_64_hex_chars() {
        assert!(is_hex64(&"a".repeat(64)));
        assert!(!is_hex64("abc"));
        assert!(!is_hex64(&"z".repeat(64)));
        assert!(!is_hex64(&"a".repeat(63)));
    }

    #[test]
    fn line_helpers_match_exact_keys_in_section() {
        let src =
            "schema = 1\n\n[app]\nport_name = \"PORT\"\n\n[env]\nPORT = { from = \"port\" }\n";
        assert_eq!(line_of_key(src, "env", "PORT"), Some(7));
        assert_eq!(line_of_key(src, "app", "port_name"), Some(4));
        // Substring must not match: searching PORT must not hit port_name.
        assert_eq!(line_of(src, "PORT"), Some(7));
        assert_eq!(line_of_section(src, "env"), Some(6));
    }

    #[test]
    fn backup_dirs_must_be_declared_and_from_must_be_known() {
        let hello = include_str!("../tests/corpus/hello.toml");
        let src = hello.replace(
            "[backup]\npostgres = true\ndirs = [\"notes\"]",
            "[backup]\npostgres = true\ndirs = [\"other\"]",
        );
        let errs = crate::Manifest::parse(&src).expect_err("undeclared backup dir");
        assert_eq!(
            errs[0].message,
            "backup.dirs entry \"other\" not in needs.dirs"
        );
        let src = hello.replace("{ from = \"bind_addr\" }", "{ from = \"banana\" }");
        let errs = crate::Manifest::parse(&src).expect_err("unknown from");
        assert!(
            errs[0].message.contains("unknown from \"banana\""),
            "{errs:?}"
        );
    }

    #[test]
    fn collects_multiple_errors_not_fail_fast() {
        let src = r#"schema = 2

[app]
id = "Hello_App"
name = "Hello"
version = "not-a-version"
homepage = "https://example.com"
health = "healthz"
admin_path = "/"

[run]
kind = "binary"
args = ["serve"]

[run.binary]
"aarch64-linux" = { url = "", sha256 = "abc", exec = "" }

[needs]
dirs = ["../evil"]

[env]
bad-name = { from = "port", dir = "media" }
HELLO_ONCE = { generate = "hex32", once = true }

[limits.default]
memory_mb = 256
cpu_percent = 50
disk_mb = 1024

[backup]
postgres = false
dirs = ["other"]

[update]
strategy = "replace"
"#;
        let manifest: Manifest = toml::from_str(src).expect("combined-bad manifest still parses");
        let errors = validate(&manifest, src);
        assert!(errors.len() > 1, "expected multiple errors, got {errors:?}");
    }
}
