//! Helpers behind `setu operator create`, `setu backup` and `setu setting
//! set` (spec §8):
//! passwords never travel on argv.
//!
//! `operator create` reads the password from `SETU_OPERATOR_PASSWORD`, else
//! the first line of stdin. `backup --out <file>` runs `pg_dump -Fc` with
//! the connection taken from the configured database URL and handed to
//! the child as `PGHOST`/`PGPORT`/`PGUSER`/`PGDATABASE`/`PGPASSWORD` (plus
//! `PGSSLMODE` when the URL names one) in its environment.

use std::io::BufRead;
use std::path::Path;

/// Env var that carries the new operator's password.
pub const OPERATOR_PASSWORD_ENV: &str = "SETU_OPERATOR_PASSWORD";

/// The password from `SETU_OPERATOR_PASSWORD`, else the first line of
/// `stdin` (trailing newline removed).
pub fn read_operator_password(
    env: Option<String>,
    stdin: &mut dyn BufRead,
) -> Result<String, String> {
    if let Some(pw) = env.filter(|p| !p.is_empty()) {
        return Ok(pw);
    }
    let mut line = String::new();
    stdin
        .read_line(&mut line)
        .map_err(|e| format!("cannot read the password from stdin: {e}"))?;
    let pw = line.trim_end_matches(['\n', '\r']).to_string();
    if pw.is_empty() {
        return Err(format!(
            "no password: set {OPERATOR_PASSWORD_ENV} or pipe it on stdin"
        ));
    }
    Ok(pw)
}

/// `setu setting set <key> <value>`: the keys an installer or operator
/// may write, each with its value type. `tls_internal` takes `true` or
/// `false`. Everything else (the setup token hash in particular) is
/// refused.
pub fn parse_setting(key: &str, value: &str) -> Result<(&'static str, serde_json::Value), String> {
    match key {
        setu_db::TLS_INTERNAL => match value {
            "true" => Ok((setu_db::TLS_INTERNAL, serde_json::Value::Bool(true))),
            "false" => Ok((setu_db::TLS_INTERNAL, serde_json::Value::Bool(false))),
            _ => Err(format!("{key} takes true or false")),
        },
        _ => Err(format!(
            "unknown setting {key} (known: {})",
            setu_db::TLS_INTERNAL
        )),
    }
}

/// A process to run: program, argv and extra environment.
#[derive(Clone, PartialEq, Eq)]
pub struct Invocation {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

// The environment carries the database password: `Debug` redacts it.
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let env: Vec<(&str, &str)> = self
            .env
            .iter()
            .map(|(k, v)| {
                let v = if k == "PGPASSWORD" { "[redacted]" } else { v };
                (k.as_str(), v)
            })
            .collect();
        f.debug_struct("Invocation")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("env", &env)
            .finish()
    }
}

/// Runs an [`Invocation`] (the real one spawns; tests record).
pub trait Runner {
    fn run(&self, inv: &Invocation) -> Result<(), String>;
}

/// Spawns the program and waits; a non-zero exit is an error.
pub struct ProcessRunner;

impl Runner for ProcessRunner {
    fn run(&self, inv: &Invocation) -> Result<(), String> {
        let status = std::process::Command::new(&inv.program)
            .args(&inv.args)
            .envs(inv.env.iter().map(|(k, v)| (k, v)))
            .status()
            .map_err(|e| format!("cannot run {}: {e}", inv.program))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("{} failed: {status}", inv.program))
        }
    }
}

/// Percent-decode a URL component (`+` stays `+`).
fn decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("");
            if let Ok(b) = u8::from_str_radix(hex, 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `pg_dump -Fc -w -f <out>` against `database_url`, the connection in
/// the child's environment only.
pub fn pg_dump_invocation(database_url: &str, out: &Path) -> Result<Invocation, String> {
    let url = url::Url::parse(database_url)
        .map_err(|_| "the configured database URL does not parse".to_string())?;
    if !matches!(url.scheme(), "postgres" | "postgresql") {
        return Err("the configured database URL is not postgres://".to_string());
    }
    let mut env: Vec<(String, String)> = Vec::new();
    let query: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .collect();
    let param = |name: &str| {
        query
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
    };
    let host = param("host").or_else(|| {
        url.host_str()
            .filter(|h| !h.is_empty())
            .map(|h| decode(h.trim_start_matches('[').trim_end_matches(']')))
    });
    if let Some(host) = host {
        env.push(("PGHOST".into(), host));
    }
    if let Some(port) = param("port").or_else(|| url.port().map(|p| p.to_string())) {
        env.push(("PGPORT".into(), port));
    }
    let user = param("user").unwrap_or_else(|| decode(url.username()));
    if !user.is_empty() {
        env.push(("PGUSER".into(), user));
    }
    let dbname = param("dbname").unwrap_or_else(|| decode(url.path().trim_start_matches('/')));
    if !dbname.is_empty() {
        env.push(("PGDATABASE".into(), dbname));
    }
    if let Some(pw) = param("password").or_else(|| url.password().map(decode)) {
        env.push(("PGPASSWORD".into(), pw));
    }
    if let Some(mode) = param("sslmode") {
        env.push(("PGSSLMODE".into(), mode));
    }
    Ok(Invocation {
        program: "pg_dump".to_string(),
        args: vec![
            "-Fc".to_string(),
            "-w".to_string(),
            "-f".to_string(),
            out.display().to_string(),
        ],
        env,
    })
}

/// `setu backup --out <file>`.
pub fn backup(runner: &dyn Runner, database_url: &str, out: &Path) -> Result<(), String> {
    runner.run(&pg_dump_invocation(database_url, out)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Invocation>>);

    impl Runner for Recorder {
        fn run(&self, inv: &Invocation) -> Result<(), String> {
            self.0.lock().unwrap().push(inv.clone());
            Ok(())
        }
    }

    #[test]
    fn backup_passes_the_password_in_the_environment_never_argv() {
        let rec = Recorder::default();
        backup(
            &rec,
            "postgres://setu:s%40cret-pw@db.internal:5433/setu?sslmode=require",
            Path::new("/tmp/setu.dump"),
        )
        .unwrap();
        let calls = rec.0.lock().unwrap();
        let inv = &calls[0];
        assert_eq!(inv.program, "pg_dump");
        assert_eq!(inv.args, ["-Fc", "-w", "-f", "/tmp/setu.dump"]);
        assert!(
            inv.args
                .iter()
                .all(|a| !a.contains("cret-pw") && !a.contains("postgres://")),
            "argv leaks the URL or password: {:?}",
            inv.args
        );
        let env = |k: &str| {
            inv.env
                .iter()
                .find(|(key, _)| key == k)
                .map(|(_, v)| v.as_str())
        };
        assert_eq!(env("PGPASSWORD"), Some("s@cret-pw"));
        assert_eq!(env("PGHOST"), Some("db.internal"));
        assert_eq!(env("PGPORT"), Some("5433"));
        assert_eq!(env("PGUSER"), Some("setu"));
        assert_eq!(env("PGDATABASE"), Some("setu"));
        assert_eq!(env("PGSSLMODE"), Some("require"));
        assert!(!format!("{inv:?}").contains("cret-pw"), "Debug leaks");
    }

    #[test]
    fn socket_host_from_the_query() {
        let inv = pg_dump_invocation(
            "postgresql:///setu?host=/var/run/postgresql&user=setu",
            Path::new("o"),
        )
        .unwrap();
        assert!(inv
            .env
            .contains(&("PGHOST".to_string(), "/var/run/postgresql".to_string())));
        assert!(inv
            .env
            .contains(&("PGUSER".to_string(), "setu".to_string())));
        assert!(!inv.env.iter().any(|(k, _)| k == "PGPASSWORD"));
        assert!(pg_dump_invocation("mysql://x@y/z", Path::new("o")).is_err());
    }

    #[test]
    fn setting_set_accepts_only_known_keys_with_typed_values() {
        assert_eq!(
            parse_setting("tls_internal", "true").unwrap(),
            (setu_db::TLS_INTERNAL, serde_json::json!(true))
        );
        assert_eq!(
            parse_setting("tls_internal", "false").unwrap(),
            (setu_db::TLS_INTERNAL, serde_json::json!(false))
        );
        assert!(parse_setting("tls_internal", "yes").is_err());
        assert!(parse_setting("setup_token_hash", "\"00\"").is_err());
        assert!(parse_setting("anything", "1").is_err());
    }

    #[test]
    fn operator_password_from_env_else_first_stdin_line() {
        let mut empty: &[u8] = b"";
        assert_eq!(
            read_operator_password(Some("from-env-1".into()), &mut empty).unwrap(),
            "from-env-1"
        );
        let mut stdin: &[u8] = b"from-stdin-1\nignored\n";
        assert_eq!(
            read_operator_password(None, &mut stdin).unwrap(),
            "from-stdin-1"
        );
        let mut nothing: &[u8] = b"";
        assert!(read_operator_password(None, &mut nothing).is_err());
    }
}
