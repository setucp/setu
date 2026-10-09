//! Semantic validation: collects ALL errors.

use std::path::{Component, Path};

use crate::schema::{EnvSpec, Manifest, ManifestError, Run, Target};

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

fn is_valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn is_hex64(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// Relative-path guard for `needs.dirs` / `backup.dirs` / `env.* dir`.
/// Rejects empty strings, absolute paths and any `..` component.
pub fn is_safe_dir(dir: &str) -> bool {
    if dir.is_empty() {
        return false;
    }
    let p = Path::new(dir);
    if p.is_absolute() {
        return false;
    }
    !p.components().any(|c| matches!(c, Component::ParentDir))
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Run the semantic rules over an already-parsed manifest, collecting
/// every failure. `src` is the original TOML source, used only to
/// attach line numbers.
pub fn validate(manifest: &Manifest, src: &str) -> Vec<ManifestError> {
    let mut errors = Vec::new();

    if manifest.schema != 1 {
        errors.push(ManifestError::new(
            "schema",
            line_of_key(src, "", "schema"),
            "schema must be 1",
        ));
    }

    if !is_valid_app_id(&manifest.app.id) {
        errors.push(ManifestError::new(
            "app.id",
            line_of_key(src, "app", "id"),
            "app.id must match [a-z0-9-]",
        ));
    }

    if !manifest.app.health.starts_with('/') {
        errors.push(ManifestError::new(
            "app.health",
            line_of_key(src, "app", "health"),
            "app.health must start with /",
        ));
    }

    if semver::Version::parse(&manifest.app.version).is_err() {
        errors.push(ManifestError::new(
            "app.version",
            line_of_key(src, "app", "version"),
            "app.version must be valid semver",
        ));
    }

    match &manifest.run {
        Run::Binary { binary } => {
            if !binary.targets.contains_key(&Target::X86_64Linux) {
                errors.push(ManifestError::new(
                    "run.binary",
                    line_of_section(src, "run.binary").or_else(|| line_of_section(src, "run")),
                    "run.binary must declare x86_64-linux",
                ));
            }
            if binary.args.iter().any(|a| a.is_empty()) {
                errors.push(ManifestError::new(
                    "run.binary.args",
                    line_of_key(src, "run.binary", "args"),
                    "run.binary.args must not contain empty entries",
                ));
            }
            for (target, entry) in &binary.targets {
                let target_line = line_of_key(src, "run.binary", &target.to_string());
                if entry.url.is_empty() {
                    let path = format!("run.binary.{target}.url");
                    let message = format!("run.binary.{target}.url must not be empty");
                    errors.push(ManifestError::new(path, target_line, message));
                }
                if entry.exec.is_empty() {
                    let path = format!("run.binary.{target}.exec");
                    let message = format!("run.binary.{target}.exec must not be empty");
                    errors.push(ManifestError::new(path, target_line, message));
                }
                if !is_hex64(&entry.sha256) {
                    let path = format!("run.binary.{target}.sha256");
                    let message = format!("run.binary.{target}.sha256 must be 64 hex chars");
                    errors.push(ManifestError::new(
                        path,
                        target_line.or_else(|| line_of_key(src, "run.binary", "sha256")),
                        message,
                    ));
                }
            }
        }
        Run::Image { image } => {
            // Phase 1 parses `kind = "image"` fine; the control plane
            // rejects it at provisioning time.
            if image.image.is_empty() {
                errors.push(ManifestError::new(
                    "run.image.image",
                    line_of_key(src, "run.image", "image"),
                    "run.image.image must not be empty",
                ));
            }
        }
    }

    for dir in &manifest.needs.dirs {
        if !is_safe_dir(dir) {
            errors.push(ManifestError::new(
                "needs.dirs",
                line_of_key(src, "needs", "dirs"),
                format!(
                    "needs.dirs entry \"{dir}\" must be a relative path without \"..\" or leading \"/\""
                ),
            ));
        }
    }

    for dir in &manifest.backup.dirs {
        if !is_safe_dir(dir) {
            errors.push(ManifestError::new(
                "backup.dirs",
                line_of_key(src, "backup", "dirs"),
                format!(
                    "backup.dirs entry \"{dir}\" must be a relative path without \"..\" or leading \"/\""
                ),
            ));
        } else if !manifest.needs.dirs.iter().any(|d| d == dir) {
            errors.push(ManifestError::new(
                "backup.dirs",
                line_of_key(src, "backup", "dirs"),
                format!("backup.dirs entry \"{dir}\" not in needs.dirs"),
            ));
        }
    }

    for (name, spec) in &manifest.env {
        if !is_valid_env_name(name) {
            errors.push(ManifestError::new(
                format!("env.{name}"),
                line_of_key(src, "env", name),
                format!("env.{name} must match [A-Za-z_][A-Za-z0-9_]*"),
            ));
        }
        match spec {
            EnvSpec::From(from) => {
                if from.from == crate::schema::EnvSource::Dir {
                    match &from.dir {
                        Some(dir) if !is_safe_dir(dir) => {
                            errors.push(ManifestError::new(
                                format!("env.{name}"),
                                line_of_key(src, "env", name),
                                format!(
                                    "env.{name} dir \"{dir}\" must be a relative path without \"..\" or leading \"/\""
                                ),
                            ));
                        }
                        Some(dir) if manifest.needs.dirs.iter().any(|d| d == dir) => {}
                        Some(dir) => {
                            errors.push(ManifestError::new(
                                format!("env.{name}"),
                                line_of_key(src, "env", name),
                                format!("env.{name} references dir \"{dir}\" not in needs.dirs"),
                            ));
                        }
                        None => {
                            errors.push(ManifestError::new(
                                format!("env.{name}"),
                                line_of_key(src, "env", name),
                                format!("env.{name} from \"dir\" requires dir = \"...\""),
                            ));
                        }
                    }
                } else if from.dir.is_some() {
                    errors.push(ManifestError::new(
                        format!("env.{name}"),
                        line_of_key(src, "env", name),
                        format!(
                            "env.{name} from \"{}\" must not set dir",
                            from.from.as_str()
                        ),
                    ));
                }
            }
            EnvSpec::Generate(gen) => {
                if gen.once && !gen.secret {
                    errors.push(ManifestError::new(
                        format!("env.{name}"),
                        line_of_key(src, "env", name),
                        format!("env.{name} once requires secret = true"),
                    ));
                }
            }
            EnvSpec::Prompt(prompt) => {
                if prompt.once && !prompt.secret {
                    errors.push(ManifestError::new(
                        format!("env.{name}"),
                        line_of_key(src, "env", name),
                        format!("env.{name} once requires secret = true"),
                    ));
                }
            }
            EnvSpec::Value(_) => {}
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
    fn safe_dir_rejects_absolute_and_parent() {
        assert!(is_safe_dir("media"));
        assert!(is_safe_dir("a/b"));
        assert!(!is_safe_dir(""));
        assert!(!is_safe_dir("/abs"));
        assert!(!is_safe_dir("../evil"));
        assert!(!is_safe_dir("a/../b"));
        assert!(is_safe_dir("my..dir"));
    }

    #[test]
    fn env_names_must_be_valid_identifiers() {
        assert!(is_valid_env_name("PORT"));
        assert!(is_valid_env_name("_X1"));
        assert!(!is_valid_env_name("bad-name"));
        assert!(!is_valid_env_name("1ABC"));
        assert!(!is_valid_env_name(""));
    }

    #[test]
    fn line_helpers_match_exact_keys_in_section() {
        let src = "schema = 1\n\n[app]\nport_env = \"PORT\"\n\n[env]\nPORT = { from = \"port\" }\n";
        assert_eq!(line_of_key(src, "env", "PORT"), Some(7));
        assert_eq!(line_of_key(src, "app", "port_env"), Some(4));
        // Substring must not match: searching PORT must not hit port_env.
        assert_eq!(line_of(src, "PORT"), Some(7));
        assert_eq!(line_of_section(src, "env"), Some(6));
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
port_env = "PORT"
admin_path = "/"

[run]
kind = "binary"

[run.binary]
args = ["serve"]
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
