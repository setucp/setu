//! Shared validation (plan Global Constraints): used by the manifest
//! validator and, independently, by the privileged helper. Each function
//! answers "is this value acceptable"; callers phrase the error.

use std::sync::LazyLock;

use regex::Regex;

fn re(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static validation regex compiles")
}

static VERSION: LazyLock<Regex> = LazyLock::new(|| re(r"^[0-9A-Za-z.+-]{1,64}$"));
static EXEC: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Za-z0-9._-]+(/[A-Za-z0-9._-]+)*$"));
static DIR: LazyLock<Regex> = LazyLock::new(|| re(r"^[a-z][a-z0-9-]{0,31}$"));
static ENV_NAME: LazyLock<Regex> = LazyLock::new(|| re(r"^[A-Z][A-Z0-9_]*$"));

/// App version: `^[0-9A-Za-z.+-]{1,64}$` (never a path).
pub fn version(s: &str) -> bool {
    VERSION.is_match(s)
}

/// Executable path inside an archive: relative, `/`-separated segments of
/// `[A-Za-z0-9._-]`, and no `..` (or `.`) component.
pub fn exec(s: &str) -> bool {
    EXEC.is_match(s) && s.split('/').all(|c| c != ".." && c != ".")
}

/// Executable file name for a raw binary download (`format = "binary"`):
/// an [`exec`] path of exactly one component.
pub fn exec_file(s: &str) -> bool {
    exec(s) && !s.contains('/')
}

/// Persistent data dir name: `^[a-z][a-z0-9-]{0,31}$`.
pub fn dir(s: &str) -> bool {
    DIR.is_match(s)
}

/// Environment variable name: `^[A-Z][A-Z0-9_]*$`.
pub fn env_name(s: &str) -> bool {
    ENV_NAME.is_match(s)
}

/// No control characters (C0, DEL, C1) anywhere: args and env values.
pub fn no_control(s: &str) -> bool {
    !s.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_exec_rejects_dotdot_and_newline() {
        assert!(exec("vyasa"));
        assert!(exec("vyasa-0.2.0-x86_64-unknown-linux-gnu/vyasa"));
        assert!(exec("a/b.c/d_e-f"));
        assert!(!exec(""));
        assert!(!exec("../x"));
        assert!(!exec("a/../b"));
        assert!(!exec(".."));
        assert!(!exec("/abs"));
        assert!(!exec("a//b"));
        assert!(!exec("a/"));
        assert!(!exec("vyasa\n"));
        assert!(!exec("a\nb"));
        assert!(!exec("a%b"));
        assert!(!exec("a b"));
        assert!(!exec("a$b"));
    }

    #[test]
    fn validate_version_rejects_slash() {
        assert!(version("0.2.0"));
        assert!(version("1.0.0-rc.1+build.5"));
        assert!(!version("../x"));
        assert!(!version("a/b"));
        assert!(!version(""));
        assert!(!version(&"1".repeat(65)));
        assert!(!version("1.0\n"));
    }

    #[test]
    fn validate_dir_env_name_and_control() {
        assert!(dir("media"));
        assert!(dir("a-b9"));
        assert!(!dir("Media"));
        assert!(!dir("9a"));
        assert!(!dir("a/b"));
        assert!(!dir("a_b"));
        assert!(!dir(&format!("a{}", "b".repeat(32))));
        assert!(env_name("VYASA_BIND_ADDR"));
        assert!(!env_name("_X"));
        assert!(!env_name("lower"));
        assert!(!env_name("A-B"));
        assert!(no_control("serve --flag=1"));
        assert!(!no_control("a\nb"));
        assert!(!no_control("a\u{7f}"));
        assert!(!no_control("a\u{85}"));
    }
}
