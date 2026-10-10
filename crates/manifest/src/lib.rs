//! App manifest schema, validator and env resolution.

pub mod resolve;
pub mod schema;
pub mod validate;

pub use resolve::{merge, resolve_env, EnvVar, ResolveError, ResolveInput};
pub use schema::Manifest;
pub use schema::{
    App, Backup, BinaryRun, BinaryTarget, EnvSource, EnvSpec, FromSpec, GenerateKind, GenerateSpec,
    ImageRun, Limits, LimitsSection, ManifestError, Needs, PromptSpec, Run, RunKind, Strategy,
    Target, Update, ValueSpec, Workdir,
};

impl Manifest {
    /// Parse TOML and run semantic validation, collecting ALL errors.
    pub fn parse(src: &str) -> Result<Self, Vec<ManifestError>> {
        match toml::from_str::<Manifest>(src) {
            Ok(manifest) => {
                let errors = validate::validate(&manifest, src);
                if errors.is_empty() {
                    Ok(manifest)
                } else {
                    Err(errors)
                }
            }
            Err(e) => Err(vec![toml_error_to_manifest_error(src, &e)]),
        }
    }
}

fn offset_to_line(src: &str, offset: usize) -> usize {
    let mut end = offset.min(src.len());
    while end > 0 && !src.is_char_boundary(end) {
        end -= 1;
    }
    src[..end].matches('\n').count() + 1
}

/// Last `[section]` header at or before `line_no` (1-based), without
/// brackets. Empty string when none is found.
fn section_at(src: &str, line_no: usize) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let upto = line_no.min(lines.len());
    for line in lines[..upto].iter().rev() {
        let t = line.trim();
        if t.starts_with('[') {
            if let Some(end) = t.find(']') {
                return t[1..end].trim().to_string();
            }
        }
    }
    String::new()
}

fn extract_quoted_after(haystack: &str, marker: &str) -> Option<String> {
    let idx = haystack.find(marker)?;
    let rest = &haystack[idx + marker.len()..];
    for quote in ['\'', '"', '`'] {
        if let Some(start) = rest.find(quote) {
            let after = &rest[start + quote.len_utf8()..];
            if let Some(end) = after.find(quote) {
                return Some(after[..end].to_string());
            }
        }
    }
    None
}

/// TOML key defined on this line (`^KEY\s*=`, bare or quoted), if any.
fn key_from_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.is_empty() || t.starts_with('#') || t.starts_with('[') {
        return None;
    }
    if let Some(rest) = t.strip_prefix('"') {
        if let Some(end) = rest.find('"') {
            if rest[end + 1..].trim_start().starts_with('=') {
                return Some(rest[..end].to_string());
            }
        }
        return None;
    }
    if let Some(rest) = t.strip_prefix('\'') {
        if let Some(end) = rest.find('\'') {
            if rest[end + 1..].trim_start().starts_with('=') {
                return Some(rest[..end].to_string());
            }
        }
        return None;
    }
    let mut len = 0;
    for c in t.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            len += c.len_utf8();
        } else {
            break;
        }
    }
    if len == 0 {
        return None;
    }
    if t[len..].trim_start().starts_with('=') {
        Some(t[..len].to_string())
    } else {
        None
    }
}

/// `[run.*]` section headers in source order, e.g. `run.binary` or
/// `run.binary."x86_64-linux"`.
fn run_subsections(src: &str) -> Vec<String> {
    src.lines()
        .filter_map(|line| {
            let t = line.trim();
            let end = t.find(']')?;
            let h = t.strip_prefix('[')?[..end - 1].trim().to_string();
            h.starts_with("run.").then_some(h)
        })
        .collect()
}

/// Refine a `[run]`-attributed error to the `[run.*]` table that holds
/// `field`. Target tables deserialize through `serde_json::Value` and
/// lose inner spans, so without this an extra key in a binary target or
/// an unknown triple reports the `[run]` line. Returns (section, line).
fn refine_run_error(src: &str, field: Option<&str>) -> (String, Option<usize>) {
    use crate::validate::{line_of_key, line_of_section};
    let display = |h: &str| h.replace(['"', '\''], "");
    let sections = run_subsections(src);
    if let Some(f) = field {
        for section in &sections {
            if let Some(l) = line_of_key(src, section, f) {
                return (display(section), Some(l));
            }
        }
        // Inline-table case: `extra` inside `"x86_64-linux" = { ..., extra = ... }`
        // does not start a line, so fall back to the line containing it.
        for section in &sections {
            let mut current = String::new();
            for (i, line) in src.lines().enumerate() {
                let t = line.trim();
                if t.starts_with('[') {
                    if let Some(end) = t.find(']') {
                        current = t[1..end].trim().to_string();
                        continue;
                    }
                }
                if &current == section && line.contains(f) {
                    return (display(section), Some(i + 1));
                }
            }
        }
    }
    if let Some(section) = sections.first() {
        return (display(section), line_of_section(src, section));
    }
    ("run".to_string(), line_of_section(src, "run"))
}

fn toml_error_to_manifest_error(src: &str, err: &toml::de::Error) -> ManifestError {
    let line = err.span().map(|span| offset_to_line(src, span.start));
    let raw = err.message().to_string();
    if raw.contains("unknown field") {
        let section = line.map(|l| section_at(src, l)).unwrap_or_default();
        if let Some(field) = extract_quoted_after(&raw, "unknown field") {
            // Run-section errors lose inner spans: point at
            // [run.binary]/[run.image] (+target), never [run].
            if section == "run" || section.starts_with("run.") {
                let (refined_section, refined_line) = refine_run_error(src, Some(&field));
                let message = format!("unknown field '{field}' at [{refined_section}]");
                return ManifestError::new(refined_section, refined_line.or(line), message);
            }
            // Env entries: attribute to the specific var when possible.
            if section == "env" {
                if let Some(l) = line {
                    if let Some(var) = src.lines().nth(l - 1).and_then(key_from_line) {
                        let path = format!("env.{var}");
                        let message = format!("unknown field '{field}' at [{path}]");
                        return ManifestError::new(path, Some(l), message);
                    }
                }
            }
            if section.is_empty() {
                let message = format!("unknown field '{field}'");
                return ManifestError::new("manifest", line, message);
            }
            let message = format!("unknown field '{field}' at [{section}]");
            return ManifestError::new(section, line, message);
        }
        // Fallback: keep the raw message.
        let section = line.map(|l| section_at(src, l)).unwrap_or_default();
        let path = if section.is_empty() {
            "manifest".to_string()
        } else {
            section
        };
        return ManifestError::new(path, line, raw);
    }
    // Missing fields, type errors, value errors (including our custom
    // EnvSpec messages like `unknown from "banana" ...`): keep the concise
    // `message()` and attach the enclosing section as path, refining
    // `[env]` to the specific var. `[run]` errors stay at [run] here;
    // unknown-field run errors are refined above.
    let section = line.map(|l| section_at(src, l)).unwrap_or_default();
    let final_line = line;
    if section == "env" {
        if let Some(l) = line {
            if let Some(var) = src.lines().nth(l - 1).and_then(key_from_line) {
                let path = format!("env.{var}");
                let first_line = raw.lines().next().unwrap_or(&raw).trim().to_string();
                return ManifestError::new(path, Some(l), first_line);
            }
        }
    }
    let first_line = raw.lines().next().unwrap_or(&raw).trim().to_string();
    let path = if section.is_empty() {
        "manifest".to_string()
    } else {
        section
    };
    ManifestError::new(path, final_line, first_line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offset_to_line_counts_newlines() {
        let src = "a\nb\nc";
        assert_eq!(offset_to_line(src, 0), 1);
        assert_eq!(offset_to_line(src, 2), 2);
        assert_eq!(offset_to_line(src, 4), 3);
    }

    #[test]
    fn section_at_finds_enclosing_header() {
        let src = "schema = 1\n\n[app]\nid = \"x\"\n";
        assert_eq!(section_at(src, 4), "app");
    }
}
